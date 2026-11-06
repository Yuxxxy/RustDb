# Package RustDb into a portable zip (Windows).
# ASCII only: PowerShell 5.1 decodes BOM-less UTF-8 as ANSI, non-ASCII breaks parsing.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\package.ps1 -Profile debug -SkipZip
param(
  [string]$Profile = "release",
  [string]$OutDir = "dist",
  [switch]$SkipBuild,
  [switch]$SkipZip
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

$version = (Select-String -Path "Cargo.toml" -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches.Groups[1].Value
if ([string]::IsNullOrEmpty($version)) { $version = "0.1.0" }

if (-not $SkipBuild) {
  Write-Host "[1/4] cargo build --profile $Profile -p rustdb --bin rustdb"
  $args = @("build", "-p", "rustdb", "--bin", "rustdb")
  if ($Profile -eq "release") { $args += "--release" }
  & cargo @args
  if ($LASTEXITCODE -ne 0) { throw "cargo build failed: $LASTEXITCODE" }
} else {
  Write-Host "[1/4] skip build"
}

$candidates = @()
if ($Profile -eq "release") { $candidates += "target\release\rustdb.exe" } else { $candidates += "target\debug\rustdb.exe" }
$exe = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if ($null -eq $exe) { throw "rustdb.exe not found, run without -SkipBuild first" }

$stage = Join-Path $OutDir ("RustDb-" + $version + "-windows-x64")
Write-Host "[2/4] staging -> $stage"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage | Out-Null

Copy-Item $exe (Join-Path $stage "rustdb.exe")
Copy-Item "README.md" $stage -ErrorAction SilentlyContinue
Copy-Item "docs\RustDb-需求开发文档.md" $stage -ErrorAction SilentlyContinue
Copy-Item "docs\M3-验证报告.md" $stage -ErrorAction SilentlyContinue
Copy-Item "docs\M4-验证报告.md" $stage -ErrorAction SilentlyContinue
Copy-Item "docs\M5-验证报告.md" $stage -ErrorAction SilentlyContinue

$sizeMb = [math]::Round((Get-Item (Join-Path $stage "rustdb.exe")).Length / 1MB, 1)
Write-Host "[3/4] rustdb.exe = $sizeMb MB"

if ($SkipZip) {
  Write-Host "[4/4] skip zip"
} else {
  $zip = $stage + ".zip"
  if (Test-Path $zip) { Remove-Item $zip -Force }
  Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip
  $zipMb = [math]::Round((Get-Item $zip).Length / 1MB, 1)
  Write-Host "[4/4] package -> $zip ($zipMb MB)"
}

Write-Host "done"
