# Build the Windows installer: cargo build --release + Inno Setup -> dist\VDM-Setup-<version>.exe
$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent

Write-Host "== cargo build --release ==" -ForegroundColor Cyan
cargo build --release --manifest-path "$root\Cargo.toml"
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

$iscc = @("${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe", "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 not found — install with: winget install JRSoftware.InnoSetup" }

Write-Host "== compiling installer ==" -ForegroundColor Cyan
& $iscc "$root\installer\vdm.iss"
if ($LASTEXITCODE -ne 0) { throw "ISCC failed" }

Get-ChildItem "$root\dist\VDM-Setup-*.exe" | ForEach-Object { Write-Host "OK: $($_.FullName)" -ForegroundColor Green }
