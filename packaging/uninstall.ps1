# Removes Heimdall, its autostart entry and (if present) the indexer service.
$dest = Join-Path $env:LOCALAPPDATA 'Programs\Heimdall'
Get-Process heimdall -ErrorAction SilentlyContinue | Stop-Process -Force
Remove-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name Heimdall -ErrorAction SilentlyContinue
if (Get-Service HeimdallIndexer -ErrorAction SilentlyContinue) {
    Start-Process (Join-Path $dest 'heimdall-indexer.exe') -ArgumentList 'uninstall' -Verb RunAs -Wait
}
Remove-Item $dest -Recurse -Force -ErrorAction SilentlyContinue
Write-Host 'Heimdall removed. Settings remain in %APPDATA%\Heimdall; delete that folder to forget everything.'
