[CmdletBinding()]
param(
    [string]$ReleaseTag,
    [switch]$PreviousStable,
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$repository = 'Kryzern/Heimdall'
$apiRoot = "https://api.github.com/repos/$repository/releases"
$apiHeaders = @{
    Accept = 'application/vnd.github+json'
    'User-Agent' = 'Heimdall-Chocolatey-Packaging'
    'X-GitHub-Api-Version' = '2022-11-28'
}
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..\..'))

if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repoRoot 'dist\chocolatey'
}
$OutputDirectory = [IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null

function Get-GitHubRelease {
    param([string]$Tag)

    if ([string]::IsNullOrWhiteSpace($Tag)) {
        $uri = "$apiRoot/latest"
    }
    else {
        if ($Tag -notmatch '^v') { $Tag = "v$Tag" }
        $uri = "$apiRoot/tags/$([uri]::EscapeDataString($Tag))"
    }

    try {
        Invoke-RestMethod -Uri $uri -Headers $apiHeaders -TimeoutSec 30
    }
    catch {
        throw "Could not retrieve the official Heimdall release '$Tag': $($_.Exception.Message)"
    }
}

function Get-StableVersion {
    param($Release)

    if ($Release.draft -or $Release.prerelease) {
        throw "Release '$($Release.tag_name)' is a draft or prerelease. Only stable releases can be packaged."
    }
    if ($Release.tag_name -notmatch '^v(?<version>\d+\.\d+\.\d+(\.\d+)?)$') {
        throw "Release tag '$($Release.tag_name)' is not a supported stable version tag (expected vMAJOR.MINOR.PATCH)."
    }

    return $Matches.version
}

function Get-InstallerAsset {
    param($Release, [string]$Version)

    $assetName = "Heimdall-$Version-x64-setup.exe"
    $assets = @($Release.assets | Where-Object { $_.name -ceq $assetName })
    if ($assets.Count -ne 1) {
        throw "Release '$($Release.tag_name)' must contain exactly one uploaded asset named '$assetName'; found $($assets.Count)."
    }

    $asset = $assets[0]
    $expectedUrl = "https://github.com/$repository/releases/download/$($Release.tag_name)/$assetName"
    if ($asset.state -ne 'uploaded' -or $asset.size -le 0 -or $asset.browser_download_url -cne $expectedUrl) {
        throw "Release '$($Release.tag_name)' installer asset is not uploaded at its expected official URL: $expectedUrl"
    }

    return $asset
}

function Find-PreviousStableRelease {
    param([version]$CurrentVersion)

    try {
        $releases = @(Invoke-RestMethod -Uri "${apiRoot}?per_page=100" -Headers $apiHeaders -TimeoutSec 30)
    }
    catch {
        throw "Could not list Heimdall releases to find the prior stable package: $($_.Exception.Message)"
    }

    $candidates = foreach ($release in $releases) {
        if ($release.draft -or $release.prerelease -or $release.tag_name -notmatch '^v(?<version>\d+\.\d+\.\d+(\.\d+)?)$') {
            continue
        }

        $candidateVersion = [version]$Matches.version
        if ($candidateVersion -ge $CurrentVersion) { continue }

        $assetName = "Heimdall-$($Matches.version)-x64-setup.exe"
        $asset = @($release.assets | Where-Object { $_.name -ceq $assetName -and $_.state -eq 'uploaded' -and $_.size -gt 0 })
        if ($asset.Count -eq 1) {
            [pscustomobject]@{
                Release = $release
                Version = $candidateVersion
            }
        }
    }

    return $candidates | Sort-Object Version -Descending | Select-Object -First 1
}

$release = Get-GitHubRelease -Tag $ReleaseTag
$versionText = Get-StableVersion -Release $release
if ($PreviousStable) {
    $previous = Find-PreviousStableRelease -CurrentVersion ([version]$versionText)
    if (-not $previous) {
        Write-Warning "No earlier stable release with an installer was found before '$($release.tag_name)'."
        return
    }
    $release = $previous.Release
    $versionText = $previous.Version.ToString()
}

$asset = Get-InstallerAsset -Release $release -Version $versionText
$assetName = $asset.name
$installerUrl = $asset.browser_download_url
$stageRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
$stageDirectory = Join-Path $stageRoot "heimdall-chocolatey-$([guid]::NewGuid().ToString('N'))"
$packageDirectory = Join-Path $stageDirectory 'package'
$toolsDirectory = Join-Path $packageDirectory 'tools'
New-Item -ItemType Directory -Path $toolsDirectory -Force | Out-Null

try {
    $installerPath = Join-Path $stageDirectory $assetName
    try {
        Invoke-WebRequest -Uri $installerUrl -OutFile $installerPath -UseBasicParsing -TimeoutSec 300 -Headers @{ 'User-Agent' = 'Heimdall-Chocolatey-Packaging' }
    }
    catch {
        throw "Could not download official release asset '$installerUrl': $($_.Exception.Message)"
    }

    $downloadedFile = Get-Item -LiteralPath $installerPath
    if ($downloadedFile.Length -ne [long]$asset.size) {
        throw "Downloaded installer size $($downloadedFile.Length) does not match GitHub release size $($asset.size)."
    }
    $checksum = (Get-FileHash -LiteralPath $installerPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($asset.digest) {
        if ($asset.digest -notmatch '^sha256:(?<digest>[A-Fa-f0-9]{64})$') {
            throw "GitHub returned an invalid SHA-256 digest for '$assetName': '$($asset.digest)'."
        }
        if ($checksum -cne $Matches.digest.ToLowerInvariant()) {
            throw "Downloaded installer SHA-256 does not match the GitHub release digest for '$assetName'."
        }
    }
    else {
        Write-Warning "GitHub release API supplied no asset digest for '$assetName'; using the downloaded file's SHA-256."
    }

    $sourceNuspec = Join-Path $PSScriptRoot 'heimdall-launcher.nuspec'
    $sourceInstallScript = Join-Path $PSScriptRoot 'tools\chocolateyInstall.ps1'
    if (-not (Test-Path -LiteralPath $sourceNuspec) -or -not (Test-Path -LiteralPath $sourceInstallScript)) {
        throw 'Chocolatey package source is missing its nuspec or installation script.'
    }

    $nuspecText = [IO.File]::ReadAllText($sourceNuspec).Replace('__PACKAGE_VERSION__', $versionText)
    $installText = [IO.File]::ReadAllText($sourceInstallScript).Replace('__INSTALLER_SHA256__', $checksum)
    if ($nuspecText -match '__[A-Z0-9_]+__' -or $installText -match '__[A-Z0-9_]+__') {
        throw 'A package template token was not replaced.'
    }
    [IO.File]::WriteAllText((Join-Path $packageDirectory 'heimdall-launcher.nuspec'), $nuspecText, [Text.UTF8Encoding]::new($false))
    [IO.File]::WriteAllText((Join-Path $toolsDirectory 'chocolateyInstall.ps1'), $installText, [Text.UTF8Encoding]::new($false))

    [xml]$nuspecXml = $nuspecText
    $metadata = $nuspecXml.package.metadata
    if ($metadata.id -cne 'heimdall-launcher' -or $metadata.version -cne $versionText) {
        throw "NuSpec ID or version does not match the target release '$($release.tag_name)'."
    }
    if ($metadata.authors -notmatch 'Ethan Dsouza' -or [string]::IsNullOrWhiteSpace($metadata.owners)) {
        throw 'NuSpec must identify the software author and Chocolatey package owner separately.'
    }
    $installerUrlTemplate = $installerUrl.Replace($versionText, '$version')
    if ($installText -notmatch [regex]::Escape($installerUrlTemplate) -or $installText -notmatch [regex]::Escape($checksum)) {
        throw 'Install script URL or SHA-256 does not match the downloaded official installer.'
    }
    if ($installText -notmatch '\[Environment\]::Is64BitOperatingSystem' -or $installText -notmatch 'url64bit\s*=') {
        throw 'Install script does not enforce Windows x64 support.'
    }
    if ("$nuspecText`n$installText" -match '(?i)([A-Z]:\\Users\\|/home/[^/\s]+/|CHOCOLATEY_API_KEY|gh[pousr]_[A-Za-z0-9]{20,})') {
        throw 'Package source contains a local machine path or credential-like value.'
    }

    $choco = Get-Command choco -ErrorAction Stop
    $packagePath = Join-Path $OutputDirectory "heimdall-launcher.$versionText.nupkg"
    if (Test-Path -LiteralPath $packagePath) {
        Remove-Item -LiteralPath $packagePath -Force
    }
    Push-Location $packageDirectory
    try {
        & $choco.Source pack (Join-Path $packageDirectory 'heimdall-launcher.nuspec') --outputdirectory $OutputDirectory | Out-Host
        if ($LASTEXITCODE -ne 0) {
            throw "choco pack failed with exit code $LASTEXITCODE."
        }
    }
    finally {
        Pop-Location
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($packagePath)
    try {
        $entries = @($archive.Entries | ForEach-Object { $_.FullName -replace '\\', '/' })
        if ($entries -notcontains 'tools/chocolateyInstall.ps1' -or $entries -notcontains 'heimdall-launcher.nuspec') {
            throw 'Generated package is missing its nuspec or installation script.'
        }
        if ($entries | Where-Object { $_ -match '\.exe$' }) {
            throw 'Generated package unexpectedly contains an executable.'
        }
        $nuspecEntry = $archive.Entries | Where-Object { ($_.FullName -replace '\\', '/') -ceq 'heimdall-launcher.nuspec' }
        $nuspecStream = $nuspecEntry.Open()
        try {
            $nuspecReader = [IO.StreamReader]::new($nuspecStream, [Text.Encoding]::UTF8)
            try { $packedNuspecText = $nuspecReader.ReadToEnd() }
            finally { $nuspecReader.Dispose() }
        }
        finally {
            $nuspecStream.Dispose()
        }
        [xml]$packedNuspecXml = $packedNuspecText
        if ($packedNuspecXml.package.metadata.id -cne 'heimdall-launcher' -or $packedNuspecXml.package.metadata.version -cne $versionText) {
            throw 'Generated package nuspec ID or version does not match the target release.'
        }
        $scriptEntry = $archive.Entries | Where-Object { ($_.FullName -replace '\\', '/') -ceq 'tools/chocolateyInstall.ps1' }
        $stream = $scriptEntry.Open()
        try {
            $reader = [IO.StreamReader]::new($stream, [Text.Encoding]::UTF8)
            try { $packedInstallText = $reader.ReadToEnd() }
            finally { $reader.Dispose() }
        }
        finally {
            $stream.Dispose()
        }
        if ($packedInstallText -match '__[A-Z0-9_]+__' -or $packedInstallText -notmatch [regex]::Escape($checksum)) {
            throw 'Generated package contains an unresolved token or the wrong installer checksum.'
        }
        if ("$packedNuspecText`n$packedInstallText" -match '(?i)([A-Z]:\\Users\\|/home/[^/\s]+/|CHOCOLATEY_API_KEY|gh[pousr]_[A-Za-z0-9]{20,})') {
            throw 'Generated package contains a local machine path or credential-like value.'
        }
    }
    finally {
        $archive.Dispose()
    }

    Write-Host "Built $packagePath from $installerUrl"
    Write-Host "Installer SHA-256: $checksum"
    return [pscustomobject]@{
        PackagePath = $packagePath
        PackageId = 'heimdall-launcher'
        ReleaseTag = $release.tag_name
        Version = $versionText
        InstallerUrl = $installerUrl
        InstallerSha256 = $checksum
    }
}
finally {
    $resolvedStage = [IO.Path]::GetFullPath($stageDirectory)
    if (-not $resolvedStage.StartsWith($stageRoot, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Temporary package path escaped the system temp directory: $resolvedStage"
    }
    Remove-Item -LiteralPath $resolvedStage -Recurse -Force -ErrorAction SilentlyContinue
}
