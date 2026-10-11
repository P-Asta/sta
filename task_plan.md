# STA Windows default browser

## Follow-up: macOS PNG build failure
- [x] Root cause: platform-independent write_png calls chunk, but chunk has a leftover Windows-only cfg.
- [x] Reproduce a non-Windows compilation failure and remove the misplaced cfg.
- [x] Run PNG tests and available project checks; macOS ARM64 release checks passed for the actual PNG module (with and without tests). Full macOS app build remains a CI check; Windows workspace retains the documented existing CDP test failure.

- [x] Inspect installer, settings, IPC, and external launch handling.
- [x] Add browser registration for MSI and explicit registration for portable builds.
- [x] Add a Windows-only settings action opening Windows Default Apps.
- [x] Verify registry contract, IPC integration, UI, and available build checks (one pre-existing CDP test failure documented).

Design: preserve Windows user choice; register HTTP/HTTPS and .htm/.html handlers with quoted executable and argument. MSI owns machine registration; ZIP registration uses a separate per-user ProgID to avoid shadowing MSI. Existing startup/relaunch handling already accepts URLs and files.

Errors: initial PowerShell brace-list syntax rejected; corrected to explicit paths. Context7 has no matching default-app documentation; Microsoft Learn used as primary source. XML comment double hyphen fixed; registry test switched to GetAttribute for unnamed values. Native compile caught SHChangeNotify signed event argument, fixed. Sandbox blocked CEF download, loopback access, and MCP pipe tests; normal-user reruns resolved these.
