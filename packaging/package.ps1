# Builds release binaries and produces dist\Heimdall-<version>-x64.zip and,
# when Inno Setup 6 is installed, dist\Heimdall-<version>-x64-setup.exe
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
$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if ($iscc) {
    & $iscc /Q "/DAppVersion=$version" packaging\heimdall.iss
    if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }
    Write-Host "Wrote dist\Heimdall-$version-x64-setup.exe"
} else {
    Write-Host 'Inno Setup 6 not found; skipped the installer.'
}
Pop-Location
