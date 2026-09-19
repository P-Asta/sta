#!/usr/bin/env node
// Packages a release build into the archive a release carries, and writes the manifest the
// in-app updater reads (`crates/sta/src/update.rs`, docs/RELEASING.md).
//
//   node tools/package-release.mjs version  [--set 1.2.3]
//   node tools/package-release.mjs stage    [--target-dir target/release] [--out dist]
//   node tools/package-release.mjs sign     --dir dist/sta-1.2.3-windows-x64 | --file dist/….msi
//   node tools/package-release.mjs msi      --dir dist/sta-1.2.3-windows-x64 [--wix wix]
//   node tools/package-release.mjs dmg      --dir dist/sta-1.2.3-darwin-arm64
//   node tools/package-release.mjs manifest --archive dist/sta-1.2.3-windows-x64.zip [--notes "…"]
//                                          [--out dist/latest.json] [--base <download url prefix>]
//
// A release carries two things per platform. The **installer** is what a person downloads: one
// file, `sta-<version>-windows-x64.msi` (WiX, `tools/installer/sta.wxs`: per-machine, Program
// Files, a Start menu entry, starts sta when it is done) or `sta-<version>-darwin-arm64.dmg` (the
// app next to an Applications shortcut). The **archive** (`.zip`) is the same payload unpacked
// anywhere — the portable build, and what the in-app updater of a portable or macOS copy applies.
// An .msi install updates through the next .msi instead (`platforms["windows-x86_64-msi"]`).
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

import { spawnSync } from 'node:child_process';
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
 *
 * macOS: nothing is copied file by file — the browser assembles its own app bundle
 * (`assemble` below, `crates/sta/src/platform/mac_bundle.rs`), and `required` then says what that
 * bundle must contain before the archive is allowed to exist.
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
    // Built, not copied: `sta --sta-bundle-mac=<dir>` writes `<dir>/sta.app` with the CEF framework
    // and the helper apps inside it, and the bridge goes next to the browser in `Contents/MacOS`
    // (which is where it looks for it, `crates/sta-mcp/src/channel.rs`).
    assemble({ targetDir, dir }) {
      const browser = join(targetDir, 'sta');
      const bridge = join(targetDir, 'sta-mcp');
      for (const binary of [browser, bridge]) {
        if (!existsSync(binary)) throw new Error(`${targetDir} is missing ${basename(binary)}`);
      }
      // console-ok: the bundler is not a console program and this only runs on macOS.
      const built = spawnSync(browser, ['--sta-bundle-mac=' + dir], { encoding: 'utf8', windowsHide: true });
      if (built.status !== 0) throw new Error(`sta --sta-bundle-mac failed: ${built.stderr?.trim() || built.error?.message || built.status}`);
      cpSync(bridge, join(dir, 'sta.app', 'Contents', 'MacOS', 'sta-mcp'));
    },
    required: [
      'sta.app/Contents/Info.plist',
      'sta.app/Contents/MacOS/sta',
      'sta.app/Contents/MacOS/sta-mcp',
      'sta.app/Contents/Frameworks/sta Helper.app/Contents/MacOS/sta Helper',
      'sta.app/Contents/Frameworks/Chromium Embedded Framework.framework/Chromium Embedded Framework',
      'sta.app/Contents/Frameworks/Chromium Embedded Framework.framework/Resources/icudtl.dat',
    ],
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
    throw new Error(`sta has no ${process.platform} build yet (docs/STATUS.md); nothing to package`);
  }
  if (!existsSync(targetDir)) throw new Error(`${targetDir} does not exist: run cargo build --release -p sta -p sta-mcp first`);

  const name = `sta-${version}-${label}`;
  const dir = join(out, name);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  // A platform whose payload builds itself (macOS) fills the staging directory first; `required`
  // is then checked against what it produced instead of against the target directory.
  payload.assemble?.({ targetDir, dir });

  const missing = [];
  let bytes = 0;
  const copy = (entry, optional = false) => {
    const from = payload.assemble ? join(dir, entry) : join(targetDir, entry);
    if (!existsSync(from)) {
      if (!optional) missing.push(entry);
      return;
    }
    if (!payload.assemble) cpSync(from, join(dir, entry), { recursive: true });
    const walk = (p) => (statSync(p).isDirectory() ? readdirSync(p).forEach((c) => walk(join(p, c))) : (bytes += statSync(p).size));
    walk(from);
  };
  for (const entry of payload.required) copy(entry);
  for (const entry of payload.optional) copy(entry, true);
  for (const entry of payload.directories) copy(entry);
  if (missing.length) {
    const where = payload.assemble ? dir : targetDir;
    throw new Error(`${where} is missing ${missing.join(', ')} — the archive would not start. Build with:\n  cargo build --release -p sta -p sta-mcp`);
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

/** `signtool.exe`: on PATH, or the newest x64 one of an installed Windows SDK. */
function findSigntool() {
  const onPath = spawnSync('where', ['signtool.exe'], { encoding: 'utf8', windowsHide: true });
  const first = onPath.status === 0 ? onPath.stdout.split(/\r?\n/).find(Boolean) : null;
  if (first) return first.trim();
  const kits = join(process.env['ProgramFiles(x86)'] ?? 'C:\\Program Files (x86)', 'Windows Kits', '10', 'bin');
  if (!existsSync(kits)) return null;
  const versions = readdirSync(kits).filter((v) => /^10\./.test(v)).sort((a, b) => b.localeCompare(a, undefined, { numeric: true }));
  return versions.map((v) => join(kits, v, 'x64', 'signtool.exe')).find(existsSync) ?? null;
}

/**
 * Authenticode over the binaries of a staged directory (`--dir`) or over one file (`--file`, the
 * .msi). Windows only, and **only when a certificate is configured** — without one this prints a
 * line and succeeds, so an unsigned release keeps building exactly as before:
 *
 *   STA_SIGN_THUMBPRINT   a certificate in the current user's (or the machine's) store, by SHA-1
 *                         thumbprint: a developer's own certificate, or a hardware token's;
 *   STA_SIGN_PFX_BASE64   a .pfx as base64 (a CI secret), with STA_SIGN_PFX_PASSWORD.
 *   STA_SIGN_TIMESTAMP    RFC 3161 timestamp server (default DigiCert's), so a signature outlives
 *                         its certificate.
 *
 * What a signature is for here: SmartScreen, and programs that check who is calling before they
 * talk to a browser (1Password's desktop app refuses an unsigned one — docs/STATUS.md). It only
 * counts on a machine that trusts the certificate's issuer, which for a self-signed development
 * certificate is the machine whose owner chose to trust it, and no other.
 */
function sign(args) {
  if (process.platform !== 'win32') throw new Error('Authenticode signing only exists on Windows');
  const dir = args['--dir'] && args['--dir'] !== 'true' ? resolve(root, args['--dir']) : null;
  const file = args['--file'] && args['--file'] !== 'true' ? resolve(root, args['--file']) : null;
  if (!dir === !file) throw new Error('give exactly one of --dir <staged directory> or --file <file>');
  const files = dir ? ['sta.exe', 'sta-mcp.exe'].map((name) => join(dir, name)) : [file];
  for (const f of files) if (!existsSync(f)) throw new Error(`${f} does not exist`);

  const thumbprint = (process.env.STA_SIGN_THUMBPRINT ?? '').replace(/\s+/g, '');
  const pfxBase64 = process.env.STA_SIGN_PFX_BASE64 ?? '';
  if (!thumbprint && !pfxBase64) {
    console.log(`package-release: no signing certificate configured (STA_SIGN_THUMBPRINT / STA_SIGN_PFX_BASE64); ${files.map((f) => basename(f)).join(', ')} stay unsigned`);
    return [];
  }
  if (thumbprint && !/^[0-9a-f]{40}$/i.test(thumbprint)) throw new Error('STA_SIGN_THUMBPRINT is not a SHA-1 thumbprint (40 hex characters)');
  const signtool = findSigntool();
  if (!signtool) throw new Error('signtool.exe not found: install the Windows SDK, or put signtool on PATH');

  const timestamp = process.env.STA_SIGN_TIMESTAMP || 'http://timestamp.digicert.com';
  // A .pfx from a secret lives in a file only for as long as signtool needs it.
  const pfx = thumbprint ? null : join(dirname(files[0]), `.sign-${process.pid}.pfx`);
  try {
    if (pfx) writeFileSync(pfx, Buffer.from(pfxBase64, 'base64'), { mode: 0o600 });
    const identity = thumbprint ? ['/sha1', thumbprint] : ['/f', pfx, ...(process.env.STA_SIGN_PFX_PASSWORD ? ['/p', process.env.STA_SIGN_PFX_PASSWORD] : [])];
    const signed = spawnSync(signtool, ['sign', '/fd', 'SHA256', '/td', 'SHA256', '/tr', timestamp, '/d', 'sta', ...identity, ...files], { encoding: 'utf8', windowsHide: true });
    if (signed.error || signed.status !== 0) {
      // signtool echoes its command line on failure; the password must not end up in a CI log.
      const why = `${signed.stdout ?? ''}${signed.stderr ?? ''}`.split(/\r?\n/).filter((l) => l.trim() && !l.includes('/p ')).slice(-6).join('\n');
      throw new Error(`signtool sign failed (exit ${signed.status}):\n${why || signed.error?.message}`);
    }
  } finally {
    if (pfx) rmSync(pfx, { force: true });
  }
  console.log(`package-release: signed ${files.map((f) => basename(f)).join(', ')} (${thumbprint ? `certificate ${thumbprint.slice(0, 8)}…` : 'the .pfx from STA_SIGN_PFX_BASE64'}, timestamp ${timestamp})`);
  return files;
}

/** The numeric `x.y.z` Windows Installer accepts (a `-beta.1` suffix is not a product version). */
export function msiVersion(version) {
  const m = /^(\d+)\.(\d+)\.(\d+)/.exec(version);
  if (!m) throw new Error(`"${version}" has no x.y.z to give the installer`);
  return `${m[1]}.${m[2]}.${m[3]}`;
}

/** `wix build` over a staged directory → `<dir>.msi` next to it. Windows only. */
function msi(args) {
  if (process.platform !== 'win32') throw new Error('an .msi can only be built on Windows');
  const dir = resolve(root, args['--dir'] ?? '');
  if (!args['--dir'] || !existsSync(join(dir, 'sta.exe'))) throw new Error('--dir <staged directory> is required and must hold sta.exe (run `stage` first)');
  const out = `${dir}.msi`;
  const wix = args['--wix'] ?? 'wix';
  const version = msiVersion(workspaceVersion());
  const built = spawnSync(
    wix,
    [
      'build', join(root, 'tools', 'installer', 'sta.wxs'),
      '-arch', 'x64',
      '-d', `Version=${version}`,
      '-d', `Icon=${join(root, 'crates', 'sta', 'res', 'sta.ico')}`,
      '-bindpath', `stage=${dir}`,
      '-o', out,
    ],
    { encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'inherit', 'inherit'] },
  );
  if (built.error) throw new Error(`cannot run ${wix}: ${built.error.message} — install it with: dotnet tool install --global wix --version 5.0.2`);
  if (built.status !== 0 || !existsSync(out)) throw new Error(`wix build failed (exit ${built.status})`);
  rmSync(out.replace(/\.msi$/, '.wixpdb'), { force: true });
  const mb = (statSync(out).size / 1024 / 1024).toFixed(1);
  console.log(`package-release: built ${basename(out)} (${mb} MB, product version ${version})`);
  if (process.env.GITHUB_OUTPUT) writeFileSync(process.env.GITHUB_OUTPUT, `installer=${basename(out)}
`, { flag: 'a' });
  return out;
}

/** `hdiutil` over a staged directory → `<dir>.dmg`: sta.app beside an Applications shortcut. macOS only. */
function dmg(args) {
  if (process.platform !== 'darwin') throw new Error('a .dmg can only be built on macOS');
  const dir = resolve(root, args['--dir'] ?? '');
  if (!args['--dir'] || !existsSync(join(dir, 'sta.app'))) throw new Error('--dir <staged directory> is required and must hold sta.app (run `stage` first)');
  const out = `${dir}.dmg`;
  // A scratch folder, so the staged directory (which the .zip is made from) keeps no symlink.
  const volume = `${dir}-dmg`;
  rmSync(volume, { recursive: true, force: true });
  mkdirSync(volume, { recursive: true });
  const run = (cmd, argv) => {
    // console-ok: macOS only; `windowsHide` is here because tools/check-no-console.mjs asks every spawn for it.
    const r = spawnSync(cmd, argv, { encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'inherit', 'inherit'] });
    if (r.error || r.status !== 0) throw new Error(`${cmd} ${argv[0]} failed: ${r.error?.message ?? `exit ${r.status}`}`);
  };
  // `ditto`, not cpSync: it is the copy that keeps a bundle's modes, links and extended attributes.
  run('ditto', [join(dir, 'sta.app'), join(volume, 'sta.app')]);
  run('ln', ['-s', '/Applications', join(volume, 'Applications')]);
  rmSync(out, { force: true });
  // `hdiutil create` fails now and then with "Resource busy" on CI runners (Spotlight is indexing
  // the folder it was just handed); the same command a moment later works.
  for (let attempt = 1; ; attempt += 1) {
    try {
      run('hdiutil', ['create', '-volname', 'sta', '-srcfolder', volume, '-ov', '-format', 'UDZO', '-imagekey', 'zlib-level=9', out]);
      break;
    } catch (e) {
      if (attempt >= 4) throw e;
      console.error(`package-release: ${e.message}; trying again (${attempt}/3)`);
      spawnSync('sleep', [String(3 * attempt)], { windowsHide: true });
    }
  }
  rmSync(volume, { recursive: true, force: true });
  const mb = (statSync(out).size / 1024 / 1024).toFixed(1);
  console.log(`package-release: built ${basename(out)} (${mb} MB)`);
  if (process.env.GITHUB_OUTPUT) writeFileSync(process.env.GITHUB_OUTPUT, `installer=${basename(out)}
`, { flag: 'a' });
  return out;
}

function manifest(args) {
  const archive = resolve(root, args['--archive'] ?? '');
  if (!archive || !existsSync(archive)) throw new Error('--archive <file> is required and must exist');
  const outFile = resolve(root, args['--out'] ?? join('dist', 'latest.json'));
  const version = workspaceVersion();
  // An installer is listed beside the archive, under its own key: a copy of sta that the .msi
  // installed cannot write to Program Files, so it updates through the next .msi
  // (`sta_core::update::Package`). A .dmg is for people, not for the updater, and is not listed.
  const suffix = /\.msi$/i.test(archive) ? '-msi' : '';
  const key = `${platformKey().key}${suffix}`;
  const base = args['--base'] ?? `https://github.com/${process.env.GITHUB_REPOSITORY ?? 'P-Asta/sta'}/releases/download/v${version}`;

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
    else if (command === 'sign') sign(parse(rest));
    else if (command === 'msi') msi(parse(rest));
    else if (command === 'dmg') dmg(parse(rest));
    else if (command === 'manifest') manifest(parse(rest));
    else if (command === 'version') {
      const args = parse(rest);
      const set = args['--set'];
      console.log(set && set !== 'true' ? setWorkspaceVersion(set.replace(/^v/, '')) : workspaceVersion());
    }
    else {
      console.log('usage: package-release.mjs stage|sign|msi|dmg|manifest|version [options] (see the header)');
      process.exit(command ? 1 : 0);
    }
  } catch (e) {
    console.error(`package-release: ${e.message}`);
    process.exit(1);
  }
}
