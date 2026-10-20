# Builds release binaries and produces dist\Heimdall-<version>-x64.zip
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Push-Location $root
cargo build --release
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$stage = Join-Path $root "dist\Heimdall-$version"
Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null
Copy-Item target\release\heimdall.exe, target\release\heimdall-indexer.exe, target\release\heimdall-bench.exe, `
          packaging\install.ps1, packaging\uninstall.ps1, README.md, LICENSE $stage
$zip = Join-Path $root "dist\Heimdall-$version-x64.zip"
Remove-Item $zip -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage\*" -DestinationPath $zip
Write-Host "Wrote $zip"
Pop-Location
