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

   The workflow's first step fails the build if the tag and `Cargo.toml` disagree — a release can
   never claim a version its binaries do not report.
3. Watch the run. It builds `--release`, refuses a binary that still carries the MCP test surface
   (`tools/check-release-clean.mjs`), stages the payload (`tools/package-release.mjs stage`), zips
   it, hashes it and leaves a **draft** release holding:

   - `sta-<version>-windows-x64.zip` — the browser, its CEF runtime and `sta-mcp.exe`;
   - `latest.json` — `{version, pubDate, notes, platforms: {"windows-x86_64": {url, sha256, size}}}`.
4. Edit the draft's notes if the generated changelog needs it, then **publish** it. Nothing updates
   before that: `releases/latest/download/…` is served from published releases only, which is what
   makes the draft a safe place to look at a build first.

`workflow_dispatch` runs the same thing for a tag that already exists (a re-run after a CI fix).

To build the archive locally exactly as CI does:

```bash
cargo build --release -p sta -p sta-mcp
node tools/package-release.mjs stage --target-dir target/release --out dist
# dist/sta-<version>-windows-x64/ — run its sta.exe to check it starts, then zip it
```

## 2. What the archive holds

`tools/package-release.mjs` copies a fixed list (`PAYLOAD`): `sta.exe`, `sta-mcp.exe`, the CEF
runtime (`libcef.dll`, the `.pak` files, `icudtl.dat`, `v8_context_snapshot.bin`, the GPU DLLs) and
`locales/`. It **fails** when one of them is missing rather than publishing an archive that cannot
start. `*.pdb`, `*.lib`, `*.d` and CMake leftovers are deliberately not in it. The UI (`ui/`) is
embedded in the binary in release builds, so it is not a file in the archive.

There is no installer and no code signature: unpack it anywhere and run `sta.exe`. SmartScreen will
warn once. Adding signing later is a step in the workflow, not a change to any of this.

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

Windows x64 only, today: the shell is Win32 throughout (`docs/STATUS.md` "Windows only"). The rest
of the pipeline is not — the manifest is keyed by platform (`windows-x86_64`, `darwin-aarch64`, …),
`package-release.mjs` has a `PAYLOAD` table per platform, the updater picks its own key, and the
workflow's matrix has the macOS job written out and commented. Porting the shell is what is
missing; when it lands, a release covers both from the same tag.
