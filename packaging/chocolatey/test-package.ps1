[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$PackageDirectory,
    [Parameter(Mandatory = $true)]
    [string]$CurrentVersion,
    [string]$PreviousVersion
)

$ErrorActionPreference = 'Stop'
$packageId = 'heimdall-launcher'
$packageDirectory = (Resolve-Path -LiteralPath $PackageDirectory).Path
$appDirectory = Join-Path $env:LOCALAPPDATA 'Programs\Heimdall'
$appExecutable = Join-Path $appDirectory 'heimdall.exe'
$configPath = Join-Path $env:APPDATA 'Heimdall\config.toml'
$runKeyPath = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$startMenuLink = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Heimdall.lnk'
$serviceExecutable = Join-Path $env:ProgramFiles 'Heimdall\heimdall-indexer.exe'

function Invoke-Choco {
    param([string[]]$Arguments)

    & choco @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "choco $($Arguments -join ' ') failed with exit code $LASTEXITCODE."
    }
}

function Stop-HeimdallProcess {
    Get-Process -Name heimdall -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

function Assert-InstalledState {
    param(
        [string]$ExpectedVersion,
        [switch]$RequireIndexerService
    )

    if (-not (Test-Path -LiteralPath $appExecutable)) {
        throw "Heimdall executable is missing from '$appExecutable'."
    }
    $uninstallEntries = @(Get-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -eq 'Heimdall' })
    if ($uninstallEntries.Count -ne 1 -or $uninstallEntries[0].DisplayVersion -ne $ExpectedVersion) {
        throw "Expected one registered Heimdall installation at version $ExpectedVersion."
    }
    $runKey = Get-ItemProperty -Path $runKeyPath -ErrorAction SilentlyContinue
    if (-not $runKey -or [string]$runKey.Heimdall -notmatch [regex]::Escape($appExecutable)) {
        throw 'Heimdall autostart entry is missing or points to the wrong executable.'
    }
    if (-not (Test-Path -LiteralPath $startMenuLink)) {
        throw 'Heimdall Start menu shortcut is missing.'
    }
    if ($RequireIndexerService) {
        if (-not (Test-Path -LiteralPath $serviceExecutable)) {
            throw "HeimdallIndexer executable is missing from '$serviceExecutable'."
        }
        $services = @(Get-CimInstance -ClassName Win32_Service -Filter "Name='HeimdallIndexer'")
        if ($services.Count -ne 1) {
            throw "Expected one HeimdallIndexer service; found $($services.Count)."
        }
        $service = Get-Service -Name HeimdallIndexer -ErrorAction SilentlyContinue
        if (-not $service) {
            throw 'HeimdallIndexer service was not registered.'
        }
        $service.WaitForStatus([System.ServiceProcess.ServiceControllerStatus]::Running, [TimeSpan]::FromSeconds(30))
    }

    $packageNuspec = Join-Path $env:ChocolateyInstall "lib\$packageId\$packageId.nuspec"
    if (-not (Test-Path -LiteralPath $packageNuspec)) {
        throw 'Chocolatey package registration was not found after installation.'
    }
    [xml]$installedPackage = Get-Content -LiteralPath $packageNuspec -Raw
    if ($installedPackage.package.metadata.version -ne $ExpectedVersion) {
        throw "Chocolatey registered package version '$($installedPackage.package.metadata.version)', expected '$ExpectedVersion'."
    }
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run package integration tests from an elevated PowerShell session.'
}
if (Test-Path -LiteralPath $appDirectory) {
    throw "Refusing to overwrite existing Heimdall installation '$appDirectory'. Use a clean Windows VM."
}
if (Test-Path -LiteralPath $configPath) {
    throw "Refusing to overwrite existing Heimdall settings '$configPath'. Use a clean Windows VM."
}
$existingRunKey = Get-ItemProperty -Path $runKeyPath -ErrorAction SilentlyContinue
if ($existingRunKey -and $existingRunKey.PSObject.Properties.Name -contains 'Heimdall') {
    throw 'Refusing to replace an existing Heimdall autostart entry. Use a clean Windows VM.'
}
if (Get-Service -Name HeimdallIndexer -ErrorAction SilentlyContinue) {
    throw 'Refusing to replace an existing HeimdallIndexer service. Use a clean Windows VM.'
}

if ([string]::IsNullOrWhiteSpace($PreviousVersion)) {
    Write-Host "Installing target package $CurrentVersion."
    Invoke-Choco @('install', $packageId, '--version', $CurrentVersion, '--source', $packageDirectory, '--yes', '--no-progress')
}
else {
    Write-Host "Installing previous stable package $PreviousVersion."
    Invoke-Choco @('install', $packageId, '--version', $PreviousVersion, '--source', $packageDirectory, '--yes', '--no-progress')
    Stop-HeimdallProcess
    Assert-InstalledState -ExpectedVersion $PreviousVersion
}

Stop-HeimdallProcess
New-Item -ItemType Directory -Path (Split-Path -Parent $configPath) -Force | Out-Null
$settingsSentinel = "hotkey = `"Ctrl+Shift+K`"`n# preserved by Chocolatey upgrade test"
[IO.File]::WriteAllText($configPath, $settingsSentinel, [Text.UTF8Encoding]::new($false))

Write-Host "Upgrading to package $CurrentVersion."
$upgradeArguments = @('upgrade', $packageId, '--version', $CurrentVersion, '--source', $packageDirectory, '--yes', '--no-progress')
if ([string]::IsNullOrWhiteSpace($PreviousVersion)) {
    $upgradeArguments += '--force'
}
Invoke-Choco $upgradeArguments
Stop-HeimdallProcess
Assert-InstalledState -ExpectedVersion $CurrentVersion -RequireIndexerService
if ([IO.File]::ReadAllText($configPath) -cne $settingsSentinel) {
    throw 'Chocolatey upgrade changed Heimdall user settings.'
}

Write-Host 'Uninstalling through Chocolatey automatic uninstaller.'
Invoke-Choco @('uninstall', $packageId, '--yes', '--no-progress')
Stop-HeimdallProcess
if (Test-Path -LiteralPath $appDirectory) {
    throw 'Chocolatey uninstall left the Heimdall application directory behind.'
}
if (Test-Path -LiteralPath $startMenuLink) {
    throw 'Chocolatey uninstall left the Heimdall Start menu shortcut behind.'
}
$remainingRunKey = Get-ItemProperty -Path $runKeyPath -ErrorAction SilentlyContinue
if ($remainingRunKey -and $remainingRunKey.PSObject.Properties.Name -contains 'Heimdall') {
    throw 'Chocolatey uninstall left the Heimdall autostart entry behind.'
}
$remainingEntries = @(Get-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue | Where-Object { $_.DisplayName -eq 'Heimdall' })
if ($remainingEntries.Count -gt 0) {
    throw 'Chocolatey uninstall left Heimdall registered in Apps and Features.'
}
$serviceDeadline = (Get-Date).AddSeconds(30)
do {
    $remainingService = Get-Service -Name HeimdallIndexer -ErrorAction SilentlyContinue
    if (-not $remainingService) { break }
    Start-Sleep -Seconds 1
} while ((Get-Date) -lt $serviceDeadline)
if ($remainingService) {
    throw 'Chocolatey uninstall left the HeimdallIndexer service registered.'
}
if (Test-Path -LiteralPath $serviceExecutable) {
    throw 'Chocolatey uninstall left the HeimdallIndexer executable behind.'
}
if (-not (Test-Path -LiteralPath $configPath) -or [IO.File]::ReadAllText($configPath) -cne $settingsSentinel) {
    throw 'Chocolatey uninstall removed or changed user settings.'
}

Write-Host 'Chocolatey install, upgrade, uninstall, service, autostart, and settings checks passed.'
