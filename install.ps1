$ErrorActionPreference = "Stop"

$Repo = if ($env:AUTO_CODEX_REPO) { $env:AUTO_CODEX_REPO } else { "lauzhihao/scodex" }
$InstallBin = if ($env:INSTALL_BIN) { $env:INSTALL_BIN } else { Join-Path $HOME ".local\bin" }
$WrapperPath = Join-Path $InstallBin "scodex.exe"
$CompatWrapperPath = Join-Path $InstallBin "auto-codex.exe"
$OriginalWrapperPath = Join-Path $InstallBin "scodex-original.cmd"
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
  $needle = $InstallBin.TrimEnd('\')
  if (-not $userPath) {
    [Environment]::SetEnvironmentVariable("Path", $needle, "User")
    return
  }
  $parts = $userPath.Split(';') | Where-Object { $_ -ne "" }
  if ($parts -notcontains $needle) {
    [Environment]::SetEnvironmentVariable("Path", ($parts + $needle) -join ';', "User")
  }
}

function Install-OriginalWrapper {
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
    & $WrapperPath import-known | Out-Null
    & $WrapperPath refresh | Out-Null
  }
}

$target = Resolve-Target
$version = Resolve-Version
$asset = "scodex-$version-$target.zip"
$url = "https://github.com/$Repo/releases/download/$version/$asset"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("scodex-install-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
New-Item -ItemType Directory -Path $InstallBin -Force | Out-Null

$archivePath = Join-Path $tmp $asset
Invoke-WebRequest -Uri $url -OutFile $archivePath
Expand-Archive -Path $archivePath -DestinationPath $tmp -Force

$binaryPath = Join-Path $tmp "scodex.exe"
if (-not (Test-Path $binaryPath)) {
  throw "Release archive did not contain scodex.exe"
}

Copy-Item $binaryPath $WrapperPath -Force
Copy-Item $binaryPath $CompatWrapperPath -Force
Install-OriginalWrapper
Ensure-UserPath
Post-InstallImport

Write-Host "Installed to $WrapperPath"
Write-Host "Installed compatibility command to $CompatWrapperPath"
Write-Host "Installed passthrough helper to $OriginalWrapperPath"
Write-Host "If the current shell cannot find scodex yet, restart PowerShell or open a new terminal."
