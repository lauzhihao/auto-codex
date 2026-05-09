$ErrorActionPreference = "Stop"

$Repo = if ($env:AUTO_CODEX_REPO) { $env:AUTO_CODEX_REPO } else { "lauzhihao/scodex" }
$ScodexHome = if ($env:SCODEX_HOME) { $env:SCODEX_HOME } else { Join-Path $HOME ".scodex" }
$BinDir = Join-Path $ScodexHome "bin"
$ShimPath = Join-Path $HOME ".local\bin\scodex.cmd"
$CompatShimPath = Join-Path $HOME ".local\bin\auto-codex.cmd"
$LegacyExeShimPath = Join-Path $HOME ".local\bin\scodex.exe"
$LegacyCompatExeShimPath = Join-Path $HOME ".local\bin\auto-codex.exe"
$OriginalWrapperPath = Join-Path $HOME ".local\bin\scodex-original.cmd"
$Version = $env:AUTO_CODEX_VERSION

function Resolve-Version {
  if ($Version) {
    return $Version
  }
  $api = "https://api.github.com/repos/$Repo/releases/latest"
  $release = Invoke-RestMethod -Uri $api
  if (-not $release.tag_name) {
    throw "Failed to resolve latest release tag from $api"
  }
  return $release.tag_name
}

function Resolve-Target {
  switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { return "x86_64-pc-windows-msvc" }
    "ARM64" { throw "Windows ARM64 release assets are not published yet. Build from source with cargo for now." }
    default { throw "Unsupported Windows architecture: $env:PROCESSOR_ARCHITECTURE" }
  }
}

function Ensure-UserPath {
  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  $needle = (Join-Path $HOME ".local\bin").TrimEnd('\')
  if (-not $userPath) {
    [Environment]::SetEnvironmentVariable("Path", $needle, "User")
    return
  }
  $parts = $userPath.Split(';') | Where-Object { $_ -ne "" }
  if ($parts -notcontains $needle) {
    [Environment]::SetEnvironmentVariable("Path", ($parts + $needle) -join ';', "User")
  }
}

function Remove-LegacyExeShim {
  param([string]$Path)

  if (-not (Test-Path $Path)) {
    return
  }

  $bytes = [IO.File]::ReadAllBytes($Path)
  if ($bytes.Length -ge 2 -and $bytes[0] -eq 0x4D -and $bytes[1] -eq 0x5A) {
    return
  }

  $prefixLength = [Math]::Min($bytes.Length, 4096)
  $prefix = [Text.Encoding]::ASCII.GetString($bytes, 0, $prefixLength)
  if ($prefix.Contains("@echo off") -and $prefix.Contains("SCODEX_HOME")) {
    Remove-Item -Path $Path -Force
  }
}

function Install-ShimScripts {
  New-Item -ItemType Directory -Path (Split-Path $ShimPath) -Force | Out-Null
  Remove-LegacyExeShim $LegacyExeShimPath
  Remove-LegacyExeShim $LegacyCompatExeShimPath

  $shimContent = @"
@echo off
rem # scodex shim v1
setlocal enabledelayedexpansion
set "SCODEX_HOME=%SCODEX_HOME%"
if not defined SCODEX_HOME (
  set "SCODEX_HOME=%USERPROFILE%\.scodex"
)
"%SCODEX_HOME%\bin\scodex.exe" %*
"@
  Set-Content -Path $ShimPath -Value $shimContent -Encoding ASCII

  Copy-Item $ShimPath $CompatShimPath -Force
}

function Install-OriginalWrapper {
  New-Item -ItemType Directory -Path (Split-Path $OriginalWrapperPath) -Force | Out-Null
  @"
@echo off
where codex >nul 2>nul
if %errorlevel% neq 0 (
  echo codex not found on PATH. 1>&2
  exit /b 1
)
codex %*
"@ | Set-Content -Path $OriginalWrapperPath -Encoding ASCII
}

function Post-InstallImport {
  $authPath = Join-Path $HOME ".codex\auth.json"
  if (Test-Path $authPath) {
    & (Join-Path $BinDir "scodex.exe") import-known | Out-Null
    & (Join-Path $BinDir "scodex.exe") refresh | Out-Null
  }
}

$target = Resolve-Target
$version = Resolve-Version
$asset = "scodex-$version-$target.zip"
$url = "https://github.com/$Repo/releases/download/$version/$asset"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("scodex-install-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
New-Item -ItemType Directory -Path $BinDir -Force | Out-Null

$archivePath = Join-Path $tmp $asset
Invoke-WebRequest -Uri $url -OutFile $archivePath
Expand-Archive -Path $archivePath -DestinationPath $tmp -Force

$binaryPath = Join-Path $tmp "scodex.exe"
if (-not (Test-Path $binaryPath)) {
  throw "Release archive did not contain scodex.exe"
}

Copy-Item $binaryPath (Join-Path $BinDir "scodex.exe") -Force
Copy-Item $binaryPath (Join-Path $BinDir "auto-codex.exe") -Force
Install-ShimScripts
Install-OriginalWrapper
Ensure-UserPath
Post-InstallImport

Write-Host "Installed binary to $(Join-Path $BinDir 'scodex.exe')"
Write-Host "Installed shim to $ShimPath"
Write-Host "Installed compatibility command to $CompatShimPath"
Write-Host "Installed passthrough helper to $OriginalWrapperPath"
Write-Host "If the current shell cannot find scodex yet, restart PowerShell or open a new terminal."
