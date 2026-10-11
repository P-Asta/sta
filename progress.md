# Progress

## macOS PNG follow-up
- Removed only the incorrect Windows cfg from chunk in png.rs.
- Existing PNG roundtrip, cropping, and upscaling tests: 3 passed on Windows.
- Actual png.rs and its tests: aarch64-apple-darwin release cargo check passed in target/png-portability-check (same flate2 1.1.10). Full macOS application linking/runtime still requires macOS CI.
- `cargo test --workspace --offline --no-fail-fast`: 463 passed, 1 ignored, 1 existing failure (automation::cdp::tests::only_allowlisted_methods_appear_in_automation_sources; CRLF scanner issue described below). Full Windows test compilation succeeded. No other code changed; Cargo.lock automatic version refresh restored.

- Implementation complete: MSI registry, portable registry, trusted async app.setDefaultBrowser IPC, Windows-only Settings button, inline status/error/retry.
- Test-first portable registry and MSI contract checks: failed without registration, passed after implementation.
- Native Windows module compiled against windows-sys 0.61.2 (CEF-independent harness).
- sta-core / sta-mcp tests passed outside sandbox. Sandbox run had four pipe-test failures due to low integrity: cancellation_sends_cancel, forwards_calls_after_welcome, refusals_and_final_bye, untrusted_pipes_get_no_hello; normal-user rerun resolved all four.
- Playwright: real Settings UI checked with mock IPC at 1100px/640px; success, pending disabled, failure/retry/error clearing, unsupported platform hidden. Only console error is existing favicon.ico 404. Screenshot target/default-browser-settings.png.
- Mock-command contract and no-console checks pass (two pre-existing untranslated mock reducer warnings).
- Independent review: no actionable findings. CEF download and complete STA test compilation succeeded outside sandbox.
- Full STA unit tests: 140 passed, 1 existing failure: automation::cdp::tests::only_allowlisted_methods_appear_in_automation_sources. Its unchanged scanner splits at LF-only `#[cfg(test)]\nmod tests`; this checkout uses CRLF, so it scans its own deliberately denied-method test literals. No automation source was changed for this task.
- sta-core + sta-mcp: 323 passed, 1 ignored. Actual MSI installation/uninstallation and changing Windows user defaults were not performed.
- No user default associations have been changed.
- `cargo build -p sta --offline`: passed. Built target/debug/sta.exe. Cargo's automatic lockfile version refresh was restored to avoid an unrelated change.
- Test servers stopped; browser artifacts moved into ignored target/default-browser-playwright. MSI toolchain is not installed, so MSI packaging/install/uninstall was not executed.
