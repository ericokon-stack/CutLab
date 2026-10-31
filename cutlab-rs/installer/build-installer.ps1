# Builds cutlab.exe in release mode, then packages it into dist\CutLab-Setup-<version>.exe
# Run from anywhere:  powershell -ExecutionPolicy Bypass -File installer\build-installer.ps1
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  throw "Rust (cargo) not found. Install it from https://rustup.rs and re-run."
}

$iscc = @(
  "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
  "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
  "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
) | Where-Object { Test-Path $_ } | Select-Object -First 1

if (-not $iscc) {
  Write-Host "Inno Setup not found. Installing via winget..."
  winget install --id JRSoftware.InnoSetup -e --accept-package-agreements --accept-source-agreements
  $iscc = @(
    "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
    "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
    "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
  ) | Where-Object { Test-Path $_ } | Select-Object -First 1
  if (-not $iscc) { throw "Inno Setup install failed. Get it from https://jrsoftware.org/isdl.php" }
}

Write-Host "Building release binary..."
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

Write-Host "Creating installer..."
& $iscc "$PSScriptRoot\cutlab.iss"
if ($LASTEXITCODE -ne 0) { throw "Inno Setup compile failed" }

Write-Host "`nDone. Installer is in: $root\dist"
