<#
  sta-ocr.ps1 - Windows.Media.Ocr -> strict JSON on stdout. Windows PowerShell 5.1, no modules.

  Modes
    -Path <img> [-Lang <tag>] [-AllowFallback]     one image from a file
    -Stdin      [-Lang <tag>] [-AllowFallback]     one image, raw bytes on stdin
    -Batch      [-Lang <tag>] [-AllowFallback]     many images: one absolute path per line on
                                                   stdin (UTF-8). Amortises the ~250 ms process
                                                   start over every image; OCR itself is ~2-12 ms.
    -ListLanguages                                 which recognizers are installed

  Single-image success (exit 0), one line of UTF-8, no BOM, no trailing newline:
    {"lang":"en-US","width":640,"height":200,"fallback":false,
     "lines":[{"text":"Hello world","x":29,"y":32,"w":179,"h":27}]}

  Batch success (exit 0) - a per-image error never kills the batch:
    {"lang":"ko","fallback":false,"images":[
      {"path":"C:\\a.png","width":640,"height":200,"lines":[...]},
      {"path":"C:\\b.png","error":"...","code":"decode-failed"}]}

  Failure (exit != 0), same stream:
    {"error":"...","code":"no-recognizer","available":["en-US","ko"]}

  Exit codes: 0 ok | 2 usage | 3 no recognizer | 4 image/decode | 5 OCR/WinRT failed.

  Coordinates are the union bounding box of the line's words, in IMAGE PIXELS, origin floored and
  far edge ceiled, clamped to the image.

  This file must stay pure ASCII: Windows PowerShell 5.1 decodes a BOM-less .ps1 with the system
  ANSI codepage, so non-ASCII literals here would be mangled. Recognized text is output only, and
  is written as raw UTF-8 bytes, which is codepage-proof.
#>
param(
  [string]$Path,
  [string]$Lang,
  [switch]$Stdin,
  [switch]$Batch,
  [switch]$AllowFallback,
  [switch]$ListLanguages
)

$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------- output (raw UTF-8 bytes)

$script:StdOut = [Console]::OpenStandardOutput()

function Write-Json([string]$s) {
  $bytes = [System.Text.Encoding]::UTF8.GetBytes($s)
  $script:StdOut.Write($bytes, 0, $bytes.Length)
  $script:StdOut.Flush()
}

function Esc([string]$s) {
  if ($null -eq $s) { return '' }
  $s = $s -replace '\\', '\\'
  $s = $s -replace '"', '\"'
  $s = $s -replace "`r", '\r'
  $s = $s -replace "`n", '\n'
  $s = $s -replace "`t", '\t'
  $s = $s -replace '[\x00-\x1F]', ' '   # any other C0 control would be invalid raw JSON
  return $s
}

function Tags-Json([string[]]$tags) {
  $parts = @()
  foreach ($t in $tags) { $parts += ('"' + (Esc $t) + '"') }
  return ('[' + ($parts -join ',') + ']')
}

# Await() surfaces WinRT failures as MethodInvocationException -> AggregateException -> the real
# COM exception. Without unwrapping, every decode error reads "One or more errors occurred."
function Inner($e) {
  $guard = 0
  while ($null -ne $e.InnerException -and $guard -lt 8) { $e = $e.InnerException; $guard++ }
  return $e.Message
}

function Fail([string]$message, [string]$code, [int]$exit, [string[]]$available) {
  $json = '{"error":"' + (Esc $message) + '","code":"' + (Esc $code) + '"'
  if ($null -ne $available) { $json += ',"available":' + (Tags-Json $available) }
  Write-Json ($json + '}')
  exit $exit
}

# ---------------------------------------------------------------- WinRT plumbing

try {
  Add-Type -AssemblyName System.Runtime.WindowsRuntime | Out-Null

  # IAsyncOperation<T>.AsTask<T>() is the only way to await WinRT from Windows PowerShell 5.1.
  $script:AsTaskGeneric = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
  })[0]

  # Every one of these is load-bearing: without the Windows.Globalization line, for instance,
  # `New-Object Windows.Globalization.Language` fails with "Cannot find type".
  [Windows.Storage.StorageFile, Windows.Storage, ContentType=WindowsRuntime]                     | Out-Null
  [Windows.Graphics.Imaging.BitmapDecoder, Windows.Graphics.Imaging, ContentType=WindowsRuntime] | Out-Null
  [Windows.Media.Ocr.OcrEngine, Windows.Media, ContentType=WindowsRuntime]                       | Out-Null
  [Windows.Globalization.Language, Windows.Globalization, ContentType=WindowsRuntime]            | Out-Null
} catch {
  Fail ('Windows Runtime OCR is not available: ' + $_.Exception.Message) 'winrt-unavailable' 5 $null
}

function Await($op, $type) {
  $task = $script:AsTaskGeneric.MakeGenericMethod($type).Invoke($null, @($op))
  $task.Wait(-1) | Out-Null
  return $task.Result
}

# Windows.Media.Ocr collections arrive in Windows PowerShell 5.1 as bare System.__ComObject: the
# IVectorView<T> -> IReadOnlyList<T> projection is NOT applied. All measured on this machine:
#   * $col.Count             -> member enumeration over the ELEMENTS, i.e. @(1,1) for two lines,
#                               which is where a naive "lines: " + $result.Lines.Count prints "1 1"
#   * @($line.Words)         -> wraps the collection in a 1-element array instead of enumerating,
#                               so [0].BoundingRect is empty / throws
#   * [IReadOnlyList[T]]$col -> InvalidCastException
# `foreach` DOES enumerate them correctly, so every collection is drained through this once.
function Drain($collection) {
  $list = New-Object System.Collections.ArrayList
  foreach ($item in $collection) { [void]$list.Add($item) }
  return $list
}

function Available-Tags {
  $tags = @()
  foreach ($l in [Windows.Media.Ocr.OcrEngine]::AvailableRecognizerLanguages) { $tags += $l.LanguageTag }
  return $tags
}

# ---------------------------------------------------------------- -ListLanguages

if ($ListLanguages) {
  Write-Json ('{"available":' + (Tags-Json (Available-Tags)) + '}')
  exit 0
}

# ---------------------------------------------------------------- argument check

$modes = @($Path, $Stdin.IsPresent, $Batch.IsPresent) | Where-Object { $_ }
if (-not $Path -and -not $Stdin -and -not $Batch) { Fail 'Pass -Path <image>, -Stdin or -Batch.' 'usage' 2 $null }
if ($modes.Count -gt 1) { Fail 'Pass exactly one of -Path, -Stdin, -Batch.' 'usage' 2 $null }

# ---------------------------------------------------------------- engine

$engine = $null
$fallback = $false

if ($Lang) {
  $language = $null
  try {
    $language = New-Object Windows.Globalization.Language $Lang
  } catch {
    Fail ("'" + $Lang + "' is not a valid BCP-47 language tag.") 'bad-language-tag' 2 (Available-Tags)
  }
  try { $engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromLanguage($language) } catch { $engine = $null }
  if ($null -eq $engine) {
    if (-not $AllowFallback) {
      Fail ("No Windows OCR recognizer is installed for '" + $Lang + "'.") 'no-recognizer' 3 (Available-Tags)
    }
    try { $engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromUserProfileLanguages() } catch { $engine = $null }
    $fallback = $true
  }
} else {
  try { $engine = [Windows.Media.Ocr.OcrEngine]::TryCreateFromUserProfileLanguages() } catch { $engine = $null }
}

if ($null -eq $engine) {
  Fail 'No Windows OCR recognizer is installed for any of your languages.' 'no-recognizer' 3 (Available-Tags)
}

$engineTag = $engine.RecognizerLanguage.LanguageTag
$maxDim = [int][Windows.Media.Ocr.OcrEngine]::MaxImageDimension

# ---------------------------------------------------------------- image -> decoder

function Read-StdinBytes {
  $raw = [Console]::OpenStandardInput()
  $mem = New-Object System.IO.MemoryStream
  $raw.CopyTo($mem)
  $buf = $mem.ToArray()
  # A .NET Framework parent that touches Process.StandardInput gets a StreamWriter with
  # AutoFlush = true, which emits a UTF-8 preamble ahead of our bytes. Rust's Stdio::piped() does
  # not, but dropping a leading BOM is free and unambiguous: no image format starts with EF BB BF.
  if ($buf.Length -ge 3 -and $buf[0] -eq 0xEF -and $buf[1] -eq 0xBB -and $buf[2] -eq 0xBF) {
    $trimmed = New-Object byte[] ($buf.Length - 3)
    [Array]::Copy($buf, 3, $trimmed, 0, $buf.Length - 3)
    return ,$trimmed
  }
  return ,$buf
}

function Decoder-FromBytes([byte[]]$bytes) {
  $mem = New-Object System.IO.MemoryStream
  $mem.Write($bytes, 0, $bytes.Length)
  [void]$mem.Seek(0, [System.IO.SeekOrigin]::Begin)
  $ras = [System.IO.WindowsRuntimeStreamExtensions]::AsRandomAccessStream($mem)
  return (Await ([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($ras)) ([Windows.Graphics.Imaging.BitmapDecoder]))
}

function Decoder-FromPath([string]$p) {
  $full = (Resolve-Path -LiteralPath $p).ProviderPath
  $file = Await ([Windows.Storage.StorageFile]::GetFileFromPathAsync($full)) ([Windows.Storage.StorageFile])
  $stream = Await ($file.OpenAsync(0)) ([Windows.Storage.Streams.IRandomAccessStream])
  return (Await ([Windows.Graphics.Imaging.BitmapDecoder]::CreateAsync($stream)) ([Windows.Graphics.Imaging.BitmapDecoder]))
}

# ---------------------------------------------------------------- recognize -> JSON

function Lines-Json($decoder) {
  $width = [int]$decoder.PixelWidth
  $height = [int]$decoder.PixelHeight
  $bitmap = Await ($decoder.GetSoftwareBitmapAsync()) ([Windows.Graphics.Imaging.SoftwareBitmap])
  $result = Await ($engine.RecognizeAsync($bitmap)) ([Windows.Media.Ocr.OcrResult])
  $out = @()
  foreach ($line in (Drain $result.Lines)) {
    $words = Drain $line.Words
    if ($words.Count -eq 0) { continue }
    $x1 = [double]::MaxValue; $y1 = [double]::MaxValue
    $x2 = [double]::MinValue; $y2 = [double]::MinValue
    foreach ($w in $words) {
      $r = $w.BoundingRect
      if ($r.X -lt $x1) { $x1 = [double]$r.X }
      if ($r.Y -lt $y1) { $y1 = [double]$r.Y }
      if (($r.X + $r.Width)  -gt $x2) { $x2 = [double]($r.X + $r.Width) }
      if (($r.Y + $r.Height) -gt $y2) { $y2 = [double]($r.Y + $r.Height) }
    }
    $lx = [int][Math]::Max(0, [Math]::Floor($x1))
    $ly = [int][Math]::Max(0, [Math]::Floor($y1))
    $rx = [int][Math]::Min($width,  [Math]::Ceiling($x2))
    $ry = [int][Math]::Min($height, [Math]::Ceiling($y2))
    $out += ('{"text":"' + (Esc $line.Text) + '","x":' + $lx + ',"y":' + $ly +
             ',"w":' + [Math]::Max(0, $rx - $lx) + ',"h":' + [Math]::Max(0, $ry - $ly) + '}')
  }
  return ('"width":' + $width + ',"height":' + $height + ',"lines":[' + ($out -join ',') + ']')
}

function Check-Size($decoder) {
  $w = [int]$decoder.PixelWidth; $h = [int]$decoder.PixelHeight
  if ($w -gt $maxDim -or $h -gt $maxDim) {
    throw ("Image is " + $w + "x" + $h + "; Windows OCR accepts at most " + $maxDim + " pixels per side.")
  }
}

$fallbackJson = $(if ($fallback) { 'true' } else { 'false' })

# ---------------------------------------------------------------- -Batch

if ($Batch) {
  $bytes = Read-StdinBytes
  $text = [System.Text.Encoding]::UTF8.GetString($bytes)
  $paths = @()
  foreach ($l in ($text -split "`r?`n")) { $t = $l.Trim(); if ($t) { $paths += $t } }
  $images = @()
  foreach ($p in $paths) {
    try {
      if (-not (Test-Path -LiteralPath $p -PathType Leaf)) { throw ('No such image: ' + $p) }
      $d = Decoder-FromPath $p
      Check-Size $d
      $images += ('{"path":"' + (Esc $p) + '",' + (Lines-Json $d) + '}')
    } catch {
      $images += ('{"path":"' + (Esc $p) + '","error":"' + (Esc (Inner $_.Exception)) + '","code":"image-failed"}')
    }
  }
  Write-Json ('{"lang":"' + (Esc $engineTag) + '","fallback":' + $fallbackJson +
              ',"images":[' + ($images -join ',') + ']}')
  exit 0
}

# ---------------------------------------------------------------- single image

$decoder = $null
try {
  if ($Stdin) {
    $bytes = Read-StdinBytes
    if ($bytes.Length -eq 0) { Fail 'No image bytes arrived on stdin.' 'empty-stdin' 4 $null }
    $decoder = Decoder-FromBytes $bytes
  } else {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { Fail ('No such image: ' + $Path) 'no-such-file' 4 $null }
    $decoder = Decoder-FromPath $Path
  }
} catch {
  Fail ('Could not decode the image: ' + (Inner $_.Exception)) 'decode-failed' 4 $null
}

try { Check-Size $decoder } catch { Fail $_.Exception.Message 'image-too-large' 4 $null }

try {
  $body = Lines-Json $decoder
} catch {
  Fail ('OCR failed: ' + (Inner $_.Exception)) 'recognize-failed' 5 $null
}

Write-Json ('{"lang":"' + (Esc $engineTag) + '",' + $body + ',"fallback":' + $fallbackJson + '}')
exit 0
