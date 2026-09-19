# Builds sta, signs the binary with a code-signing certificate from the Windows certificate store,
# and starts it — `cargo run` for a machine where something checks who is calling.
#
#   pwsh tools/run-signed.ps1                 # debug build
#   pwsh tools/run-signed.ps1 -Release        # release build
#   pwsh tools/run-signed.ps1 -NoRun          # build and sign only
#   pwsh tools/run-signed.ps1 -- --sta-data-dir=C:\tmp\profile    # arguments for sta
#
# Every build rewrites sta.exe, and a rewritten file has no signature, so a plain `cargo run` is
# always unsigned. Programs that verify the browser before they talk to it — 1Password's desktop
# app — then refuse it ("No signature was present in the subject"; docs/STATUS.md,
# docs/RELEASING.md). The certificate is the one named by -Thumbprint / STA_SIGN_THUMBPRINT, or the
# only "sta local development" code-signing certificate in Cert:\CurrentUser\My. A self-signed one
# counts only on a machine whose owner added it to the Trusted Root store; this script never
# touches that store.

[CmdletBinding()]
param(
    [switch]$Release,
    [switch]$NoRun,
    [string]$Thumbprint = $env:STA_SIGN_THUMBPRINT,
    [Parameter(ValueFromRemainingArguments = $true)][string[]]$StaArgs
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

$certs = @(Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert)
if ($Thumbprint) {
    $wanted = $Thumbprint -replace '\s', ''
    $cert = $certs | Where-Object { $_.Thumbprint -eq $wanted } | Select-Object -First 1
    if (-not $cert) { throw "no code-signing certificate $wanted in Cert:\CurrentUser\My" }
} else {
    $mine = @($certs | Where-Object { $_.Subject -like 'CN=sta local development*' -and $_.NotAfter -gt (Get-Date) })
    if ($mine.Count -ne 1) { throw "expected one 'sta local development' code-signing certificate in Cert:\CurrentUser\My, found $($mine.Count): pass -Thumbprint" }
    $cert = $mine[0]
}

$cargo = @('build', '-p', 'sta', '-p', 'sta-mcp')
if ($Release) { $cargo += '--release' }
Push-Location $root
try {
    & cargo @cargo
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
}

$profileDir = if ($Release) { 'release' } else { 'debug' }
$dir = Join-Path $root "target\$profileDir"
foreach ($name in 'sta.exe', 'sta-mcp.exe') {
    $file = Join-Path $dir $name
    # The timestamp lets the signature outlive the certificate; without a network it is skipped.
    $signed = Set-AuthenticodeSignature -FilePath $file -Certificate $cert -HashAlgorithm SHA256 -TimestampServer 'http://timestamp.digicert.com' -ErrorAction SilentlyContinue
    if (-not $signed -or -not $signed.SignerCertificate) {
        $signed = Set-AuthenticodeSignature -FilePath $file -Certificate $cert -HashAlgorithm SHA256
    }
    if (-not $signed.SignerCertificate) { throw "could not sign $file ($($signed.StatusMessage))" }
    $trusted = if ($signed.Status -eq 'Valid') { 'trusted on this machine' } else { "NOT trusted on this machine ($($signed.Status)): its certificate is not in a trusted root store" }
    Write-Host "run-signed: $name signed by $($cert.Subject) - $trusted"
}

if ($NoRun) { return }
$exe = Join-Path $dir 'sta.exe'
Write-Host "run-signed: starting $exe $StaArgs"
& $exe @StaArgs
