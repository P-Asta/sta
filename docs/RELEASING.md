# Releasing sta, and how a running sta updates itself

A release is a **git tag**, a **draft GitHub release** the workflow fills in, and — once a person
publishes that draft — the thing every running copy of sta finds by itself.

```
git tag v0.2.0 → .github/workflows/release.yml → draft release (archives + latest.json)
                                                       ↓ somebody presses Publish
     running sta ── GET releases/latest/download/latest.json ──→ "sta 0.2.0 is available"
                 ── GET the archive, check its SHA-256, unpack ─→ "restart to update"
```

## 1. Cutting a release

1. Set the version once, in the workspace: `[workspace.package] version` in `Cargo.toml`. Every
   crate inherits it, `sta.exe`'s VERSIONINFO comes from it (`crates/sta/build.rs`), the agent
   endpoint reports it, and Settings › About shows it.
2. Commit, then tag it **exactly** `v<that version>`:

   ```bash
   git tag v0.2.0
   git push origin main --tags
   ```

   The tag is what the release is actually named after: before anything compiles, the workflow
   stamps `[workspace.package] version` with the tag's version, so the binaries, the archive name
   and `latest.json` can never disagree with the tag. That edit lives only on the runner, so if you
   forget step 1 the build still succeeds — it logs a warning, and `Cargo.toml` on `main` keeps
   reporting the old version for local builds until you bump it.
3. Watch the run. It builds `--release`, refuses a binary that still carries the MCP test surface
   (`tools/check-release-clean.mjs`), stages the payload (`tools/package-release.mjs stage`), zips
   it, builds the installer from that same staged directory, hashes both and leaves a **draft**
   release holding:

   - `sta-<version>-windows-x64.msi` — **what a Windows user downloads**: one file, one
     double-click (`tools/installer/sta.wxs`, WiX 5 — per-machine into `C:\Program Files\sta`, a
     Start menu entry, a major upgrade of whatever version is installed, and it starts sta when it
     is done);
   - `sta-<version>-darwin-arm64.dmg` — **what a macOS user downloads**: `sta.app` beside an
     Applications shortcut (`hdiutil`, UDZO);
   - `sta-<version>-windows-x64.zip` — the same Windows payload with no installer (portable): the
     browser, its CEF runtime and `sta-mcp.exe`;
   - `sta-<version>-darwin-arm64.zip` — `sta.app`, with the CEF framework, the helper apps and
     `sta-mcp` inside it (what the macOS updater applies);
   - `latest.json` — `{version, pubDate, notes, platforms: {"windows-x86_64": {…},
     "windows-x86_64-msi": {…}, "darwin-aarch64": {…}}}`, merged from the per-platform manifests
     each build job uploads. The `.dmg` is for people, not for the updater, and is not listed.
4. Edit the draft's notes if the generated changelog needs it, then **publish** it. Nothing updates
   before that: `releases/latest/download/…` is served from published releases only, which is what
   makes the draft a safe place to look at a build first.

`workflow_dispatch` runs the same thing for a tag that already exists (a re-run after a CI fix).

To build the archive locally exactly as CI does:

```bash
cargo build --release -p sta -p sta-mcp
node tools/package-release.mjs stage --target-dir target/release --out dist
# dist/sta-<version>-<platform>/ — start its sta.exe (or sta.app) to check it runs, then zip it
# (on macOS use `ditto -c -k --keepParent`, which keeps the bundle's file modes)

# the installer, from that same directory:
dotnet tool install --global wix --version 5.0.2                       # Windows, once
node tools/package-release.mjs msi --dir dist/sta-<version>-windows-x64   # → dist/…-windows-x64.msi
node tools/package-release.mjs dmg --dir dist/sta-<version>-darwin-arm64  # → dist/…-darwin-arm64.dmg
```

An .msi can be looked into without installing it: `msiexec /a <msi> /qn TARGETDIR=<dir>` unpacks it
(the files land in `<dir>\PFiles64\sta`, and must match the staged directory file for file).

## 2. What the archive holds

`tools/package-release.mjs` knows what each platform's archive holds (`PAYLOAD`), and **fails** when
something is missing rather than publishing an archive that cannot start.

- **Windows**: a fixed list of files copied out of the target directory — `sta.exe`, `sta-mcp.exe`,
  the CEF runtime (`libcef.dll`, the `.pak` files, `icudtl.dat`, `v8_context_snapshot.bin`, the GPU
  DLLs) and `locales/`. `*.pdb`, `*.lib`, `*.d` and CMake leftovers are deliberately not in it.
- **macOS**: nothing is copied file by file. The browser assembles its own bundle
  (`sta --sta-bundle-mac=<staging dir>`, `crates/sta/src/platform/mac_bundle.rs`) — the same code
  that makes `target/debug/sta.app` for `cargo run`, told to produce a standalone one: the CEF
  framework copied in, and a real copy of the binary in each helper app instead of a hard link.
  `sta-mcp` is then placed next to the browser in `Contents/MacOS`, which is where it looks for it.
  The archive is made with `ditto`, the one macOS archiver that keeps file modes intact — without
  the executable bit the unpacked binaries cannot start (`crates/sta/src/unzip.rs` restores it).

The UI (`ui/`) is embedded in the binary in release builds, so it is not a file in either archive.

**The installers.** The `.msi` installs per-machine under Program Files on purpose: password
managers that pair with a desktop app check who is calling, and 1Password's "Add Browser" only
takes a browser that is code signed *or* lives under `C:\Program Files` — sta is not signed. It
writes `HKLM\Software\sta\InstallDir`, which is how a running sta knows it must update through
the next `.msi` (§3), and its `UpgradeCode` must never change: that is what makes the next version
an upgrade rather than a second copy. A double-click needs no answers — Windows' own progress box,
one UAC prompt, then sta starts (`LaunchApp`; a silent `/qn` install starts nothing unless it
passes `LAUNCHAPP=1`). The `.dmg` is a drag-to-Applications image and nothing more.

**Code signing (Windows) is there, and off until there is a certificate.** `node
tools/package-release.mjs sign --dir <staged directory>` (and `--file <the .msi>`) runs `signtool`
over `sta.exe` and `sta-mcp.exe` with SHA-256 and an RFC 3161 timestamp; the workflow calls it after
`stage` and again after `msi`, so the archive and the installer carry the same signed binaries.
Which certificate:

- `STA_SIGN_PFX_BASE64` + `STA_SIGN_PFX_PASSWORD` — a `.pfx` as base64. In CI these are repository
  **secrets** of the same names; the file exists on disk only while `signtool` runs.
- `STA_SIGN_THUMBPRINT` — a certificate already in the Windows certificate store (a developer's own,
  or a hardware token's), by SHA-1 thumbprint. For signing a local build.
- neither: the step prints one line and succeeds, and the release is unsigned — SmartScreen warns
  once, and 1Password only takes sta from `C:Program Files` (docs/STATUS.md).

A signature only counts where the certificate's issuer is trusted. A certificate from a public CA
(or Azure Trusted Signing) is trusted everywhere. A **self-signed** one
(`New-SelfSignedCertificate -Type CodeSigningCert …`) is trusted on no machine until its owner adds
it to that machine's *Trusted Root Certification Authorities* — a decision about that machine's
security, which is why nothing in this repository does it. Measured with 1Password 8.12: an
unsigned `sta.exe` is refused with `0x800B0100` ("No signature was present"), one signed with an
untrusted self-signed certificate is read (`publisher: …`) and refused with `0x800B0109` ("terminated
in a root certificate which is not trusted").

On macOS nothing is signed: Gatekeeper refuses a double-click on an unsigned, quarantined app —
open it from the right-click menu the first time. Signing and notarization there would be another
step in the workflow, not a change to any of this.

## 3. How a running sta updates itself

`crates/sta/src/update.rs` (the network and the files) and `crates/sta-core/src/update.rs` (the
manifest, the version comparison and the status the UI sees).

- **Check** — once, `CHECK_DELAY_MS` (8 s) after startup, and whenever Settings › About asks. It is
  a `GET` through Chromium's own network stack: system proxy, system certificates, no cookies, no
  cache. Nothing is sent but the request.
- **Compare** — semver-ish: numbers first, a pre-release is older than its release
  (`1.2.0-beta.1 < 1.2.0`), anything unparseable is "not an update".
- **Offer** — a toast (`sta 0.2.0 is available`) and the Updates row in Settings › About.
- **Download** — only when asked. Only the archive the manifest names for this platform, only from
  `https://github.com/<repo>/releases/download/…` (`Asset::trusted`), streamed to
  `%LOCALAPPDATA%\sta\updates\` and hashed while it arrives. A wrong size or a wrong **SHA-256**
  deletes it and fails the update; nothing is unpacked before the hash matches.
- **Unpack** — into `%LOCALAPPDATA%\sta\updates\sta-<version>\`, with every entry's path checked
  against that directory (`crates/sta/src/unzip.rs`: no `..`, no absolute paths, no drive letters)
  and every entry's CRC-32 verified. The status becomes `ready`.
- **Apply** — on "Restart to update": the **staged** `sta.exe` is started with
  `--sta-apply-update --source <staged> --target <installed> --pid <ours>` and sta quits. That
  helper waits for the process to exit, copies the staged files over the installed ones (retrying a
  file that is still locked) and starts the installed `sta.exe` again. It runs from the staging
  directory, never from the directory it replaces — which is the only way a program can replace its
  own files on Windows. Its log is `%LOCALAPPDATA%\sta\updates\apply.log`.
- **An .msi install takes another road from Download on.** Program Files is not writable for sta,
  so a copy whose directory matches `HKLM\Software\sta\InstallDir` (`update::package`) downloads
  `platforms["windows-x86_64-msi"]` instead — same trust rule, same hash check, nothing unpacked —
  and "Restart to update" runs `msiexec /i <msi> /passive /norestart LAUNCHAPP=1` and quits. Windows
  Installer asks for elevation itself, replaces the old version as a major upgrade once sta's files
  are free, and starts the new `sta.exe` as the user. Declining the UAC prompt leaves the old
  version installed (start it again; the update is offered again).
- **A portable copy that cannot write to its own directory** (a `.zip` unpacked into Program Files
  by hand) is told so — "install the new version with the .msi" — instead of being sent through a
  copy that must fail and an update that is offered for ever.
- **Clean up** — the next start deletes the staging directories (`update::clean_staging`).

Switches: `STA_NO_UPDATE_CHECK=1` turns the whole thing off for a run, and it never runs at all
under the e2e harness (`STA_E2E=1`), so no suite depends on the network or on what is published.
`debug.info.update` (debug builds) reports what the updater is doing.

### What is trusted, and what is not

Whoever can publish a release can ship code to every sta. That is the same trust as the repository
itself, so the update path does not try to be stronger than it — but it is exactly that strong and
no weaker:

| Step | Refused |
|---|---|
| manifest | anything but HTTPS `github.com`, over 64 KB, not JSON, no version |
| asset | a URL outside this repository's releases, a malformed hash, an implausible size |
| download | a length that disagrees with the manifest, a SHA-256 that does not match |
| archive | zip64, encryption, unknown compression, a bad CRC, any path that leaves the directory |
| apply | a staged directory without `sta.exe` |

A signature (an ed25519 key in CI, the public half compiled in) would add: a release that GitHub
itself serves but the key did not sign is refused. It is a change to `sta_core::update` and to the
workflow, nothing else.

## 4. Platforms

Windows x64 and macOS arm64, from the same tag: the workflow's matrix has a job for each, and every
step after the build is shared. The manifest is keyed by platform (`windows-x86_64`,
`darwin-aarch64`, …), `package-release.mjs` has a `PAYLOAD` entry per platform and the updater picks
its own key (`sta_core::update::platform_key`).

What a new platform needs: a `PAYLOAD` entry, a matrix row, and — for the update to *apply* — the
answer to "what does an install look like", which `update.rs` asks through `install_dir` and
`exe_in` (a directory of files on Windows, an app bundle on macOS). Linux has neither a shell half
in `crates/sta/src/platform/` nor a payload yet.
