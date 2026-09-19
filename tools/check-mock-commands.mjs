#!/usr/bin/env node
// Checks that the UI mock's command validation (ui/common/mock-reducers.js) matches the Rust
// source of truth (crates/sta-core/src/command.rs):
//
// - every `Command` variant is either a UI command in `COMMAND_FIELDS` or a shell event in
//   `SHELL_ONLY_COMMANDS` (from `Command::allowed_from_ui`), and nothing else is listed;
// - each UI command's required fields (neither `Option<_>` nor `#[serde(default)]`) are exactly the
//   `COMMAND_FIELDS` entries, with a kind matching the Rust type (enum value lists included);
// - `validateCommand` accepts a valid sample of every UI command (built from the Rust types, for
//   every `SidebarPanel` and `Container` variant too), rejects it with 400 when a required field is
//   missing, and `allowedFromUi` rejects every shell event;
// - the mock's `isTransientPanel` matches `SidebarPanel::is_transient` for every panel (transient
//   panels float with a hidden sidebar, the others dock it);
// - commands without a mock reducer are reported as warnings (they are accepted, not simulated).
//
//   node tools/check-mock-commands.mjs        exit 0 = in sync, 1 = mismatch
//
// The Rust parsing is deliberately simple: it relies on command.rs keeping one-line field
// declarations and full-line comments, as it does today.

import { readFileSync } from 'node:fs';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const rustPath = join(root, 'crates', 'sta-core', 'src', 'command.rs');
const mock = await import(pathToFileURL(join(root, 'ui', 'common', 'mock-reducers.js')).href);
const { COMMAND_FIELDS, SHELL_ONLY_COMMANDS, reducers, validateCommand, allowedFromUi, isTransientPanel } = mock;

const errors = [];
const warnings = [];
const fail = (msg) => errors.push(msg);

// ------------------------------------------------------------------------------------ Rust parsing

const source = readFileSync(rustPath, 'utf8')
  .split(/\r?\n/)
  .filter((line) => !line.trim().startsWith('//'))
  .join('\n');

/** Body (between the braces) of `pub enum <name> {`. */
function enumBody(name) {
  const start = source.search(new RegExp(`pub enum ${name}\\s*\\{`));
  if (start < 0) throw new Error(`enum ${name} not found in command.rs`);
  const open = source.indexOf('{', start);
  let depth = 0;
  for (let i = open; i < source.length; i++) {
    if (source[i] === '{') depth++;
    else if (source[i] === '}' && --depth === 0) return source.slice(open + 1, i);
  }
  throw new Error(`enum ${name}: unbalanced braces`);
}

/** Split on commas that are not nested in (), [], {} or <>. */
function splitTopLevel(text) {
  const parts = [];
  let depth = 0;
  let current = '';
  for (const ch of text) {
    if ('([{<'.includes(ch)) depth++;
    else if (')]}>'.includes(ch)) depth--;
    if (ch === ',' && depth === 0) {
      parts.push(current);
      current = '';
    } else {
      current += ch;
    }
  }
  parts.push(current);
  return parts.map((p) => p.trim()).filter(Boolean);
}

const stripAttributes = (text) => {
  const attrs = [];
  let rest = text.trim();
  while (rest.startsWith('#[')) {
    let depth = 0;
    let end = 0;
    for (let i = 1; i < rest.length; i++) {
      if (rest[i] === '[') depth++;
      else if (rest[i] === ']' && --depth === 0) {
        end = i;
        break;
      }
    }
    attrs.push(rest.slice(0, end + 1));
    rest = rest.slice(end + 1).trim();
  }
  return { attrs, rest };
};

const camel = (name) => name[0].toLowerCase() + name.slice(1);
const snakeToCamel = (name) => name.replace(/_([a-z0-9])/g, (_, c) => c.toUpperCase());

/** `[{name, fields: [{name, type, optional}]}]` of a serde-tagged enum. */
function parseVariants(enumName) {
  return splitTopLevel(enumBody(enumName)).map((chunk) => {
    const { rest } = stripAttributes(chunk);
    const m = /^([A-Z]\w*)\s*(?:\{([\s\S]*)\})?$/.exec(rest);
    if (!m) throw new Error(`${enumName}: can't parse variant "${rest}"`);
    const fields = splitTopLevel(m[2] ?? '').map((f) => {
      const { attrs, rest: decl } = stripAttributes(f);
      const fm = /^(\w+)\s*:\s*([\s\S]+)$/.exec(decl);
      if (!fm) throw new Error(`${enumName}::${m[1]}: can't parse field "${decl}"`);
      const type = fm[2].replace(/\s+/g, '');
      const optional = type.startsWith('Option<') || attrs.some((a) => /serde\(.*\bdefault\b/.test(a));
      return { name: snakeToCamel(fm[1]), type, optional };
    });
    return { name: camel(m[1]), fields };
  });
}

const commands = parseVariants('Command');
const simpleEnums = Object.fromEntries(
  ['OpenTarget', 'LinkDisposition', 'SplitSide', 'CommandBarMode', 'InternalPage', 'ZoomDirection', 'WindowAction', 'DownloadAction'].map((name) => [
    name,
    parseVariants(name).map((v) => v.name),
  ]),
);
const sidebarPanels = parseVariants('SidebarPanel');
const containers = parseVariants('Container');

// Transient panels: the `matches!(self, SidebarPanel::A | SidebarPanel::B)` of is_transient.
const transientFn = source.slice(source.indexOf('pub fn is_transient'));
const transientList = /matches!\(\s*self\s*,([\s\S]*?)\)/.exec(transientFn)?.[1] ?? '';
const transientPanels = new Set([...transientList.matchAll(/SidebarPanel::(\w+)/g)].map((m) => camel(m[1])));
if (transientPanels.size === 0) fail('SidebarPanel::is_transient: no panels parsed (parser out of date?)');

// Shell events: the `!matches!(self, Command::A { .. } | Command::B ...)` list of allowed_from_ui.
const allowedFn = source.slice(source.indexOf('pub fn allowed_from_ui'));
const matchesList = /!matches!\(\s*self\s*,([\s\S]*?)\n\s*\)/.exec(allowedFn)?.[1] ?? '';
const shellEvents = new Set([...matchesList.matchAll(/Command::(\w+)/g)].map((m) => camel(m[1])));
if (shellEvents.size < 10) fail(`allowed_from_ui: parsed only ${shellEvents.size} shell events (parser out of date?)`);

// ------------------------------------------------------------------------------------ kinds

const STRUCT_OBJECTS = new Set(['SettingsPatch', 'Boost', 'Theme', 'Rect', 'Download']);

/** The COMMAND_FIELDS kind a Rust field type must have, or null when unmapped. */
function expectedKind(type) {
  switch (type) {
    case 'Id':
    case 'u64':
      return 'id';
    case 'u32':
    case 'usize':
    case 'i32':
    case 'bool':
      return type;
    case 'String':
      return 'string';
    case 'Vec<f32>':
    case 'Vec<f64>':
      return 'numbers';
    case 'DropTarget':
      return 'dropTarget';
    case 'SidebarPanel':
      return 'panel';
    case 'Box<Command>':
      return 'command';
    default:
      if (Object.hasOwn(simpleEnums, type)) return simpleEnums[type];
      if (STRUCT_OBJECTS.has(type)) return 'object';
      return null;
  }
}

const sameKind = (a, b) =>
  Array.isArray(a) && Array.isArray(b) ? a.length === b.length && a.every((x) => b.includes(x)) : a === b;
const showKind = (k) => (Array.isArray(k) ? `[${k.join('|')}]` : String(k));

/** A JSON value that deserializes as the Rust type (samples for validateCommand). */
function sample(type) {
  const kind = expectedKind(type);
  if (Array.isArray(kind)) return kind[0];
  switch (kind) {
    case 'id':
    case 'u32':
    case 'usize':
      return 1;
    case 'i32':
      return -1;
    case 'bool':
      return true;
    case 'string':
      return 'x';
    case 'numbers':
      return [0.5, 0.5];
    case 'dropTarget':
      return { container: { type: 'today', space: 1 }, before: null };
    case 'panel':
      return { type: 'downloads' };
    case 'command':
      return { type: 'reopenClosed' };
    case 'object':
      return {};
    default:
      return undefined;
  }
}

// ------------------------------------------------------------------------------------ checks

const uiCommands = commands.filter((c) => !shellEvents.has(c.name));
for (const name of shellEvents) {
  if (!commands.some((c) => c.name === name)) fail(`allowed_from_ui lists "${name}", which is not a Command variant`);
}

// Shell events.
for (const name of shellEvents) {
  if (!SHELL_ONLY_COMMANDS.has(name)) fail(`shell event "${name}" is missing from SHELL_ONLY_COMMANDS`);
  if (Object.hasOwn(COMMAND_FIELDS, name)) fail(`shell event "${name}" must not be in COMMAND_FIELDS`);
  if (allowedFromUi({ type: name }) !== false) fail(`allowedFromUi accepts shell event "${name}"`);
  if (allowedFromUi({ type: 'commitOmnibox', command: { type: name } }) !== false) fail(`allowedFromUi accepts "${name}" inside commitOmnibox`);
}
for (const name of SHELL_ONLY_COMMANDS) {
  if (!shellEvents.has(name)) fail(`SHELL_ONLY_COMMANDS lists "${name}", which is not a shell event in command.rs`);
}

// UI commands: fields and kinds.
for (const name of Object.keys(COMMAND_FIELDS)) {
  if (!uiCommands.some((c) => c.name === name)) fail(`COMMAND_FIELDS lists "${name}", which is not a UI command in command.rs`);
}
for (const cmd of uiCommands) {
  if (!Object.hasOwn(COMMAND_FIELDS, cmd.name)) {
    fail(`UI command "${cmd.name}" is missing from COMMAND_FIELDS`);
    continue;
  }
  const listed = COMMAND_FIELDS[cmd.name];
  const required = cmd.fields.filter((f) => !f.optional);
  for (const f of required) {
    if (!Object.hasOwn(listed, f.name)) {
      fail(`${cmd.name}.${f.name} (${f.type}) is required in command.rs but not in COMMAND_FIELDS`);
      continue;
    }
    const want = expectedKind(f.type);
    if (want == null) fail(`${cmd.name}.${f.name}: no kind mapping for Rust type ${f.type} (extend check-mock-commands.mjs)`);
    else if (!sameKind(want, listed[f.name])) fail(`${cmd.name}.${f.name}: COMMAND_FIELDS kind ${showKind(listed[f.name])}, command.rs ${f.type} → ${showKind(want)}`);
  }
  for (const field of Object.keys(listed)) {
    const rust = cmd.fields.find((f) => f.name === field);
    if (!rust) fail(`COMMAND_FIELDS ${cmd.name}.${field} does not exist in command.rs`);
    else if (rust.optional) fail(`COMMAND_FIELDS ${cmd.name}.${field} is optional in command.rs (Option / #[serde(default)])`);
  }

  // Behaviour: a full valid sample passes, each missing required field is a 400.
  const valid = { type: cmd.name };
  for (const f of required) valid[f.name] = sample(f.type);
  const verdict = validateCommand(valid);
  if (verdict) fail(`validateCommand rejects a valid ${cmd.name}: ${verdict.message}`);
  if (!allowedFromUi(valid)) fail(`allowedFromUi rejects UI command ${cmd.name}`);
  for (const f of required) {
    const { [f.name]: _dropped, ...partial } = valid;
    if (validateCommand(partial)?.code !== 400) fail(`validateCommand accepts ${cmd.name} without required field "${f.name}"`);
  }
  if (!Object.hasOwn(reducers, cmd.name)) warnings.push(`${cmd.name}: no mock reducer (accepted, not simulated)`);
}

// Nested shapes: every SidebarPanel and Container variant, with and without their id fields.
for (const panel of sidebarPanels) {
  const value = { type: panel.name };
  for (const f of panel.fields) value[f.name] = sample(f.type);
  const verdict = validateCommand({ type: 'openSidebarPanel', panel: value });
  if (verdict) fail(`validateCommand rejects SidebarPanel ${panel.name}: ${verdict.message}`);
  for (const f of panel.fields.filter((x) => !x.optional)) {
    if (!validateCommand({ type: 'openSidebarPanel', panel: { type: panel.name } })) fail(`validateCommand accepts SidebarPanel ${panel.name} without "${f.name}"`);
  }
}
for (const panel of sidebarPanels) {
  const value = { type: panel.name };
  for (const f of panel.fields) value[f.name] = sample(f.type);
  const transient = transientPanels.has(panel.name);
  if (Boolean(isTransientPanel(value)) !== transient) fail(`isTransientPanel(${panel.name}) must be ${transient} (SidebarPanel::is_transient)`);
}
for (const name of transientPanels) {
  if (!sidebarPanels.some((p) => p.name === name)) fail(`is_transient lists "${name}", which is not a SidebarPanel variant`);
}
for (const container of containers) {
  const value = { type: container.name };
  for (const f of container.fields) value[f.name] = sample(f.type);
  const verdict = validateCommand({ type: 'moveItem', id: 1, to: { container: value } });
  if (verdict) fail(`validateCommand rejects Container ${container.name}: ${verdict.message}`);
  for (const f of container.fields.filter((x) => !x.optional)) {
    if (!validateCommand({ type: 'moveItem', id: 1, to: { container: { type: container.name } } })) {
      fail(`validateCommand accepts Container ${container.name} without "${f.name}"`);
    }
  }
}
if (validateCommand({ type: 'noSuchCommand' })?.code !== 400) fail('validateCommand accepts an unknown command type');
if (validateCommand({ type: 'constructor' })?.code !== 400) fail('validateCommand accepts a prototype key as command type');
if (validateCommand({ type: 'commitOmnibox', command: { type: 'activateItem' } })?.code !== 400) {
  fail('validateCommand accepts commitOmnibox with an invalid inner command');
}

// ------------------------------------------------------------------------------------ report

for (const w of warnings) console.log(`warning: ${w}`);
for (const e of errors) console.log(`error: ${e}`);
console.log(
  `check-mock-commands: ${commands.length} Command variants (${uiCommands.length} UI, ${shellEvents.size} shell events), ` +
    `${sidebarPanels.length} sidebar panels (${transientPanels.size} transient), ${containers.length} containers: ` +
    (errors.length ? `${errors.length} error(s)` : 'COMMAND_FIELDS in sync') +
    (warnings.length ? `, ${warnings.length} warning(s)` : ''),
);
process.exit(errors.length ? 1 : 0);
