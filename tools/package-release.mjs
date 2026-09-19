#!/usr/bin/env node
// Packages a release build into the archive a release carries, and writes the manifest the
// in-app updater reads (`crates/sta/src/update.rs`, docs/RELEASING.md).
//
//   node tools/package-release.mjs version  [--set 1.2.3]
//   node tools/package-release.mjs stage    [--target-dir target/release] [--out dist]
//   node tools/package-release.mjs manifest --archive dist/sta-1.2.3-windows-x64.zip [--notes "…"]
//                                          [--out dist/latest.json] [--base <download url prefix>]
//
// `stage` copies the browser and everything it needs at runtime out of a cargo target directory
// into `<out>/<name>/`, and prints the staged directory and the archive name the workflow should
// create from it. It **fails** when a required file is missing rather than shipping an archive
// that cannot start — a release that is missing `libcef.dll` looks fine until someone downloads it.
//
// `manifest` hashes a finished archive and writes/extends `latest.json`:
//
//   { "version": "1.2.3", "pubDate": "…", "notes": "…",
//     "platforms": { "windows-x86_64": { "url": "…", "sha256": "…", "size": 123 } } }
//
// The updater fetches that file from the *published* release
// (`https://github.com/<repo>/releases/latest/download/latest.json`), compares `version` with its
// own, downloads the archive for its platform and checks the hash before it unpacks anything. Draft
// releases are not served by that URL, which is exactly why the workflow leaves a draft: nothing
// updates until a person publishes it.

import { createHash } from 'node:crypto';
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(join(dirname(fileURLToPath(import.meta.url)), '..'));

/** The platform key in the manifest, and the archive's own name, for the host this runs on. */
export function platformKey(platform = process.platform, arch = process.arch) {
  const os = { win32: 'windows', darwin: 'darwin', linux: 'linux' }[platform];
  const cpu = { x64: 'x86_64', arm64: 'aarch64' }[arch];
  if (!os || !cpu) throw new Error(`unsupported platform ${platform}/${arch}`);
  return { key: `${os}-${cpu}`, label: `${os}-${cpu === 'x86_64' ? 'x64' : 'arm64'}` };
}

/**
 * What a release archive holds, per platform. Everything here must exist in the target directory
 * after `cargo build --release -p sta -p sta-mcp`, or `stage` fails.
 *
 * Windows: the two binaries plus the CEF runtime that `cef-dll-sys` drops beside them. `*.pdb`,
 * `*.lib`, `*.d`, `CMakeLists.txt` and the other build leftovers are deliberately not in it.
 */
const PAYLOAD = {
  win32: {
    required: [
      'sta.exe',
      'sta-mcp.exe',
      'libcef.dll',
      'chrome_elf.dll',
      'd3dcompiler_47.dll',
      'libEGL.dll',
      'libGLESv2.dll',
      'vk_swiftshader.dll',
      'vk_swiftshader_icd.json',
      'vulkan-1.dll',
      'chrome_100_percent.pak',
      'chrome_200_percent.pak',
      'resources.pak',
      'icudtl.dat',
      'v8_context_snapshot.bin',
    ],
    // Present in most CEF distributions, not worth failing a release over.
    optional: ['dxcompiler.dll', 'dxil.dll', 'CREDITS.html'],
    directories: ['locales'],
  },
  darwin: {
    // The macOS shell does not exist yet (docs/STATUS.md): when it does, this is where its app
    // bundle goes, and the workflow's `macos` job stops being skipped.
    required: [],
    optional: [],
    directories: [],
  },
};

/** The `[workspace.package] version` line, as it sits in Cargo.toml. */
const VERSION_LINE = /^(\s*version\s*=\s*)"([^"]+)"/m;

/** The workspace version — the one a release is named after. */
export function workspaceVersion() {
  const manifest = readFileSync(join(root, 'Cargo.toml'), 'utf8');
  const version = manifest.match(VERSION_LINE)?.[2];
  if (!version) throw new Error('no [workspace.package] version in Cargo.toml');
  return version;
}

/**
 * Rewrite `[workspace.package] version` in place and return what it now says.
 *
 * The release workflow calls this with the version off the tag, so a `vX.Y.Z` push cannot fail
 * merely because Cargo.toml was never bumped: the tag is the release's name, and every crate
 * inherits this line, so stamping it here is what keeps `sta --version`, the archive name and
 * `latest.json` all reporting the version the tag claims. The edit is never committed.
 */
export function setWorkspaceVersion(version) {
  if (!/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)*$/.test(version)) {
    throw new Error(`"${version}" is not a version cargo will accept`);
  }
  const file = join(root, 'Cargo.toml');
  const manifest = readFileSync(file, 'utf8');
  if (!VERSION_LINE.test(manifest)) throw new Error('no [workspace.package] version in Cargo.toml');
  const current = manifest.match(VERSION_LINE)[2];
  if (current !== version) {
    writeFileSync(file, manifest.replace(VERSION_LINE, `$1"${version}"`));
    console.error(`package-release: stamped [workspace.package] version ${current} -> ${version}`);
  }
  return version;
}

export function sha256(file) {
  return createHash('sha256').update(readFileSync(file)).digest('hex');
}

function stage(args) {
  const targetDir = resolve(root, args['--target-dir'] ?? join('target', 'release'));
  const out = resolve(root, args['--out'] ?? 'dist');
  const version = workspaceVersion();
  const { label } = platformKey();
  const payload = PAYLOAD[process.platform];
  if (!payload) throw new Error(`no release payload defined for ${process.platform}`);
  if (!payload.required.length) {
    throw new Error(`sta has no ${process.platform} build yet (docs/STATUS.md "Windows only"); nothing to package`);
  }
  if (!existsSync(targetDir)) throw new Error(`${targetDir} does not exist: run cargo build --release -p sta -p sta-mcp first`);

  const name = `sta-${version}-${label}`;
  const dir = join(out, name);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });

  const missing = [];
  let bytes = 0;
  const copy = (entry, optional = false) => {
    const from = join(targetDir, entry);
    if (!existsSync(from)) {
      if (!optional) missing.push(entry);
      return;
    }
    cpSync(from, join(dir, entry), { recursive: true });
    const walk = (p) => (statSync(p).isDirectory() ? readdirSync(p).forEach((c) => walk(join(p, c))) : (bytes += statSync(p).size));
    walk(from);
  };
  for (const entry of payload.required) copy(entry);
  for (const entry of payload.optional) copy(entry, true);
  for (const entry of payload.directories) copy(entry);
  if (missing.length) {
    throw new Error(`${targetDir} is missing ${missing.join(', ')} — the archive would not start. Build with:\n  cargo build --release -p sta -p sta-mcp`);
  }

  // A release build must never carry the debug-only MCP test surface (tools/check-release-clean.mjs
  // checks the binaries themselves; this is the reminder at the packaging step).
  const mb = (bytes / 1024 / 1024).toFixed(1);
  console.log(`package-release: staged ${name} (${mb} MB, ${payload.required.length + payload.directories.length} entries) in ${dir}`);
  if (process.env.GITHUB_OUTPUT) {
    writeFileSync(process.env.GITHUB_OUTPUT, `version=${version}\nname=${name}\ndir=${dir}\narchive=${name}.zip\n`, { flag: 'a' });
  }
  return { version, name, dir };
}

function manifest(args) {
  const archive = resolve(root, args['--archive'] ?? '');
  if (!archive || !existsSync(archive)) throw new Error('--archive <file> is required and must exist');
  const outFile = resolve(root, args['--out'] ?? join('dist', 'latest.json'));
  const version = workspaceVersion();
  const { key } = platformKey();
  const base = args['--base'] ?? `https://github.com/${process.env.GITHUB_REPOSITORY ?? 'P-Asta/Astatine'}/releases/download/v${version}`;

  const existing = existsSync(outFile) ? JSON.parse(readFileSync(outFile, 'utf8')) : {};
  const next = {
    version,
    pubDate: existing.pubDate ?? new Date().toISOString(),
    notes: args['--notes'] ?? existing.notes ?? '',
    platforms: { ...(existing.platforms ?? {}) },
  };
  next.platforms[key] = {
    url: `${base.replace(/\/$/, '')}/${basename(archive)}`,
    sha256: sha256(archive),
    size: statSync(archive).size,
  };
  mkdirSync(dirname(outFile), { recursive: true });
  writeFileSync(outFile, `${JSON.stringify(next, null, 2)}\n`);
  console.log(`package-release: ${outFile} now describes ${Object.keys(next.platforms).join(', ')} for ${version}`);
  return next;
}

function parse(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i].startsWith('--')) args[argv[i]] = argv[i + 1]?.startsWith('--') || argv[i + 1] === undefined ? 'true' : argv[(i += 1)];
  }
  return args;
}

const [command, ...rest] = process.argv.slice(2);
if (import.meta.url === `file://${process.argv[1].replace(/\\/g, '/')}` || process.argv[1].endsWith('package-release.mjs')) {
  try {
    if (command === 'stage') stage(parse(rest));
    else if (command === 'manifest') manifest(parse(rest));
    else if (command === 'version') {
      const args = parse(rest);
      const set = args['--set'];
      console.log(set && set !== 'true' ? setWorkspaceVersion(set.replace(/^v/, '')) : workspaceVersion());
    }
    else {
      console.log('usage: package-release.mjs stage|manifest|version [options] (see the header)');
      process.exit(command ? 1 : 0);
    }
  } catch (e) {
    console.error(`package-release: ${e.message}`);
    process.exit(1);
  }
}
