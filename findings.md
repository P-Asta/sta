# Findings

## macOS PNG build failure
- The shared write_png function called chunk on every platform, but chunk retained #[cfg(windows)] from its former capture-only location. It uses only portable Vec/flate2 operations.
- Compiling the actual png.rs via an isolated temporary crate targeting aarch64-apple-darwin reproduced all three E0425 errors before the fix. Removing the cfg resolves them without new dependencies or API changes.

- WiX 5 installer currently registers only InstallDir and Version; Settings General has startup only.
- Native platform uses windows-sys 0.61 and ShellExecuteW; no new dependencies needed.
- External URLs/files are already handled by app::startup_urls and forwarded to an existing instance.
- Microsoft documentation: https://learn.microsoft.com/en-us/windows/win32/shell/default-programs
- Settings URI: https://learn.microsoft.com/en-us/windows/apps/develop/launch/launch-default-apps-settings (registeredAppMachine / registeredAppUser; older Windows falls back to generic Default Apps).
