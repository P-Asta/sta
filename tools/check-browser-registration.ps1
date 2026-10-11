# Validate the installer's registry contract without changing the machine's registrations.
$ErrorActionPreference = 'Stop'
[xml]$source = Get-Content -Raw (Join-Path $PSScriptRoot 'installer/sta.wxs')
$ns = [System.Xml.XmlNamespaceManager]::new($source.NameTable)
$ns.AddNamespace('w', 'http://wixtoolset.org/schemas/v4/wxs')
if (!$source.SelectSingleNode('//w:Feature/w:ComponentRef[@Id="BrowserRegistration"]', $ns)) {
    throw 'Browser registration is not included in the installed feature'
}
$values = @{}
foreach ($key in $source.SelectNodes('//w:RegistryKey', $ns)) {
    foreach ($value in $key.SelectNodes('w:RegistryValue', $ns)) {
        $values["$($key.Root)\$($key.Key)|$($value.GetAttribute('Name'))"] = [string]$value.Value
    }
}
function Expect-Value([string]$Key, [string]$Expected) {
    if (!$values.ContainsKey($Key) -or $values[$Key] -cne $Expected) {
        throw "Registry contract: $Key expected '$Expected', got '$($values[$Key])'"
    }
}
$caps = 'Software\Clients\StartMenuInternet\sta\Capabilities'
Expect-Value 'HKLM\Software\RegisteredApplications|sta' $caps
Expect-Value "HKLM\$caps|ApplicationName" 'sta'
foreach ($scheme in @('http', 'https')) {
    Expect-Value "HKLM\$caps\URLAssociations|$scheme" 'staHTML'
}
foreach ($extension in @('.htm', '.html')) {
    Expect-Value "HKLM\$caps\FileAssociations|$extension" 'staHTML'
}
Expect-Value 'HKLM\Software\Classes\staHTML\shell\open\command|' '"[#StaExe]" -- "%1"'
Expect-Value 'HKLM\Software\Classes\staHTML\DefaultIcon|' '"[#StaExe]",0'
if ($values.Keys | Where-Object { $_ -match 'UserChoice' }) { throw 'Installer must not override user defaults' }
Write-Output 'PASS: MSI browser registration, URL/file handlers, quoting, and default-choice preservation'
