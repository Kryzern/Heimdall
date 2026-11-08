$ErrorActionPreference = 'Stop'

$packageName = 'heimdall-launcher'
$version = $env:ChocolateyPackageVersion
$installerSha256 = '__INSTALLER_SHA256__'

if ($version -notmatch '^\d+\.\d+\.\d+(\.\d+)?$') {
    throw "Unsupported Heimdall package version: '$version'."
}

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'Heimdall Chocolatey package supports Windows x64 only.'
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
if ($identity.User.Value -eq 'S-1-5-18') {
    throw 'Heimdall installs per user. Run Chocolatey elevated as the intended interactive user, not as LocalSystem.'
}

if ($installerSha256 -notmatch '^[A-Fa-f0-9]{64}$') {
    throw 'The Heimdall installer checksum was not generated.'
}

$installerUrl = "https://github.com/Kryzern/Heimdall/releases/download/v$version/Heimdall-$version-x64-setup.exe"
$packageArgs = @{
    packageName    = $packageName
    fileType       = 'exe'
    url64bit       = $installerUrl
    checksum64     = $installerSha256
    checksumType64 = 'sha256'
    silentArgs     = '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART'
    validExitCodes = @(0)
}

Install-ChocolateyPackage @packageArgs
