#!/usr/bin/env node
// Static half of the "no cmd window while testing" rule (docs/TESTING.md §console windows).
//
// A console window that flashes during a test run is a real defect: it steals the foreground from
// the window under test and it is what the user asked us to stop. The runtime half is the
// `test_console_windows` tool (it also catches a console opened by a child of a child); this check
// is what stops the problem coming back at the source.
//
// Rules:
//  1. every spawn of a *console* helper (powershell, taskkill, python, node, sta-mcp.exe, …) in
//     tools/ and crates/sta/e2e/ passes `windowsHide: true`;
//  2. a spawn of a *GUI* child (sta.exe / astatine.exe) must NOT be given `windowsHide`: Node's // rename:keep
//     flag is libuv's HIDE_CONSOLE | HIDE_GUI, which also sets STARTF_USESHOWWINDOW / SW_HIDE and
//     can start the browser invisible. Such a call site carries `// console-ok: <reason>` instead;
//  3. anything a Rust crate starts with `std::process::Command` sets
//     `creation_flags(CREATE_NO_WINDOW)`, and any raw `CreateProcessW` passes `CREATE_NO_WINDOW`
//     and never `DETACHED_PROCESS` (a console-subsystem child of a console-less parent gets a
//     freshly allocated console, which Windows 11 hands to Windows Terminal: ten visible windows
//     per browser launch — see crates/sta-mcp/src/win.rs::launch);
//  4. nothing goes through `cmd.exe /c`, `cmd /c`, `start `, `{ shell: true }` (libuv routes that
//     through cmd.exe) or PowerShell's `Start-Process` — in argv form (`execFileSync('cmd', ['/c',
//     …])`) as well as in one string.
//
// `// console-ok: <reason>` on the call's own line or the line before it is the only allowlist.
//
//   node tools/check-no-console.mjs        exit 0 = clean, 1 = violations

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(join(dirname(fileURLToPath(import.meta.url)), '..'));
const JS_DIRS = [join(root, 'tools'), join(root, 'crates', 'sta', 'e2e')];
const RS_DIRS = [join(root, 'crates')];
// `astatine.exe` is the legacy binary migration-e2e.mjs really launches. rename:keep
const GUI_TARGETS = /\bEXE\b|sta\.exe|astatine\.exe|this\.exe/; // rename:keep
const JS_CALLS = /(?<![.\w])(spawnSync|spawn|execFileSync|execFile|execSync|exec)\s*\(/g;

const problems = [];
const fail = (file, line, message) => problems.push(`${relative(root, file).replace(/\\/g, '/')}:${line}: ${message}`);

function walk(dir, extension, out = []) {
  let entries;
  try {
    entries = readdirSync(dir);
  } catch {
    return out;
  }
  for (const name of entries) {
    const full = join(dir, name);
    if (name === 'node_modules' || name === 'target' || name.startsWith('.')) continue;
    if (statSync(full).isDirectory()) walk(full, extension, out);
    else if (name.endsWith(extension)) out.push(full);
  }
  return out;
}

const lineOf = (text, index) => text.slice(0, index).split('\n').length;

/** The text of a call's arguments, from the opening paren to its match. */
function callText(text, openParen) {
  let depth = 0;
  for (let i = openParen; i < text.length && i < openParen + 4000; i++) {
    if (text[i] === '(') depth++;
    else if (text[i] === ')') {
      depth--;
      if (depth === 0) return text.slice(openParen + 1, i);
    }
  }
  return text.slice(openParen + 1, openParen + 4000);
}

/** The match sits in a `//` comment (the rules are about code, not about prose examples). */
function inComment(text, index) {
  const lineStart = text.lastIndexOf('\n', index) + 1;
  const before = text.slice(lineStart, index);
  return before.includes('//') || /^\s*[*#]/.test(before);
}

/** The body of the Rust function `index` sits in: from its `fn` keyword to the next `}` at col 0. */
function rustFunctionAt(text, index) {
  const before = text.slice(0, index);
  let start = 0;
  for (const m of before.matchAll(/(?:^|\n)[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+"[^"]*"\s+)?fn\s+/g)) start = m.index;
  const end = text.indexOf('\n}', index);
  return text.slice(start, end < 0 ? text.length : end);
}

/** `// console-ok: …` on this line or the one before it. */
function allowed(text, index) {
  const lines = text.slice(0, index).split('\n');
  const before = lines.slice(-2).join('\n');
  const line = text.slice(index).split('\n')[0];
  return /\/\/\s*console-ok:/.test(before) || /\/\/\s*console-ok:/.test(line);
}

for (const file of JS_DIRS.flatMap((d) => walk(d, '.mjs'))) {
  const text = readFileSync(file, 'utf8');
  for (const match of text.matchAll(JS_CALLS)) {
    const open = match.index + match[0].length - 1;
    const args = callText(text, open);
    const line = lineOf(text, match.index);
    const gui = GUI_TARGETS.test(args.split(',')[0] ?? '');
    if (allowed(text, match.index) || inComment(text, match.index)) continue;
    if (gui) {
      fail(file, line, `${match[1]}() of a GUI child needs a "// console-ok: <reason>" comment (windowsHide would start it hidden)`);
    } else if (!/windowsHide\s*:\s*true/.test(args)) {
      fail(file, line, `${match[1]}() without windowsHide: true`);
    }
    // Rule 4, argv form: the program and the flag are separate arguments, so the one-string regex
    // below cannot see them. `shell: true` is libuv's own trip through cmd.exe.
    if (/^\s*['"`]cmd(\.exe)?['"`]/.test(args) || /^\s*['"`][^'"`]*\\cmd\.exe['"`]/i.test(args)) {
      fail(file, line, `${match[1]}() of cmd.exe opens a console window; run the target directly`);
    }
    if (/\bshell\s*:\s*true/.test(args)) {
      fail(file, line, `${match[1]}() with shell: true goes through cmd.exe; run the target directly`);
    }
  }
}

for (const file of RS_DIRS.flatMap((d) => walk(d, '.rs'))) {
  const text = readFileSync(file, 'utf8');
  for (const match of text.matchAll(/Command::new\s*\(/g)) {
    if (allowed(text, match.index) || inComment(text, match.index)) continue;
    // The statement chain: up to the end of the statement, or 1200 characters.
    const tail = text.slice(match.index, match.index + 1200);
    const statement = tail.slice(0, tail.indexOf(';') < 0 ? tail.length : tail.indexOf(';') + 1);
    // A builder kept in a variable configures itself later in the function.
    const scope = /let\s+mut\s+\w+\s*=\s*$/.test(text.slice(Math.max(0, match.index - 40), match.index)) ? tail : statement;
    if (!/creation_flags\s*\(/.test(scope)) {
      fail(file, lineOf(text, match.index), 'Command::new without creation_flags(CREATE_NO_WINDOW)');
    }
  }
  // A raw CreateProcessW: the flags live in a variable a few lines up, so the rule is about the
  // whole enclosing function body (doc comments above `fn` are outside it and may name either flag).
  for (const match of text.matchAll(/CreateProcessW\s*\(/g)) {
    if (allowed(text, match.index) || inComment(text, match.index)) continue;
    const body = rustFunctionAt(text, match.index);
    if (!/\bCREATE_NO_WINDOW\b/.test(body)) {
      fail(file, lineOf(text, match.index), 'CreateProcessW without CREATE_NO_WINDOW in its flags');
    }
    if (/\bDETACHED_PROCESS\b/.test(body)) {
      fail(file, lineOf(text, match.index), 'CreateProcessW with DETACHED_PROCESS: a console child of a console-less parent opens a window');
    }
  }
}

const self = resolve(fileURLToPath(import.meta.url));
for (const file of [...JS_DIRS.flatMap((d) => walk(d, '.mjs')), ...RS_DIRS.flatMap((d) => walk(d, '.rs')), ...JS_DIRS.flatMap((d) => walk(d, '.ps1'))]) {
  if (resolve(file) === self) continue; // this file names the forbidden spellings on purpose
  const text = readFileSync(file, 'utf8');
  for (const match of text.matchAll(/cmd(\.exe)?\s+\/c\b|Start-Process/g)) {
    const line = text.split('\n')[lineOf(text, match.index) - 1] ?? '';
    if (/^\s*(\/\/|#|\*)/.test(line) || allowed(text, match.index)) continue;
    fail(file, lineOf(text, match.index), `${match[0]} opens a console window; run the target directly`);
  }
}

if (problems.length) {
  for (const p of problems) console.log(`FAIL ${p}`);
  console.log(`\ncheck-no-console: ${problems.length} problem(s)`);
  process.exit(1);
}
console.log('check-no-console: every helper is started without a console window');
