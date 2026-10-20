# Installs Heimdall for the current user.
#   .\install.ps1              copy, register autostart, start
#   .\install.ps1 -Indexer     also install the fast file indexer service (asks for admin)
param([switch]$Indexer)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$dest = Join-Path $env:LOCALAPPDATA 'Programs\Heimdall'

Get-Process heimdall -ErrorAction SilentlyContinue | Stop-Process -Force
New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item (Join-Path $here 'heimdall.exe'), (Join-Path $here 'heimdall-indexer.exe') $dest -Force

Start-Process (Join-Path $dest 'heimdall.exe')
Write-Host "Heimdall installed to $dest. Press F5 to open it."

if ($Indexer) {
    Start-Process (Join-Path $dest 'heimdall-indexer.exe') -ArgumentList 'install' -Verb RunAs -Wait
    Write-Host 'Indexer service installed.'
}
