# Installs Heimdall for the current user, plus the fast file indexer service.
#   .\install.ps1              copy, start, install the indexer service (asks for admin)
#   .\install.ps1 -NoIndexer   skip the indexer service
param([switch]$NoIndexer)
$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$dest = Join-Path $env:LOCALAPPDATA 'Programs\Heimdall'

Get-Process heimdall -ErrorAction SilentlyContinue | Stop-Process -Force
New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item (Join-Path $here 'heimdall.exe'), (Join-Path $here 'heimdall-indexer.exe') $dest -Force

Start-Process (Join-Path $dest 'heimdall.exe')
Write-Host "Heimdall installed to $dest. Press Alt+Space to open it."

if (-not $NoIndexer) {
    try {
        Start-Process (Join-Path $dest 'heimdall-indexer.exe') -ArgumentList 'install' -Verb RunAs -Wait
        Write-Host 'Indexer service installed.'
    } catch {
        Write-Host 'Skipped the indexer service. Heimdall will index your drives by itself.'
    }
}
