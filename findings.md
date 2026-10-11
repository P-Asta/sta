# Findings

- WiX 5 installer currently registers only InstallDir and Version; Settings General has startup only.
- Native platform uses windows-sys 0.61 and ShellExecuteW; no new dependencies needed.
- External URLs/files are already handled by app::startup_urls and forwarded to an existing instance.
- Microsoft documentation: https://learn.microsoft.com/en-us/windows/win32/shell/default-programs
- Settings URI: https://learn.microsoft.com/en-us/windows/apps/develop/launch/launch-default-apps-settings (registeredAppMachine / registeredAppUser; older Windows falls back to generic Default Apps).
