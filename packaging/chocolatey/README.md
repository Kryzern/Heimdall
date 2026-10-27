# Chocolatey package

Package ID is `heimdall-launcher`. Community ID `heimdall` already belongs to Heimdall Suite, a different application. `authors` names Heimdall's software author; `owners` names the Chocolatey package owner. Update `owners` if the account submitting the package differs from `Kryzern`.

The package downloads the official release installer and pins its SHA-256. It uses the existing Inno Setup installer. Chocolatey must run elevated as the intended interactive user because the installer writes to that user's profile and autostart hive. Installation as LocalSystem is rejected. The Inno installer registers HeimdallIndexer separately; its own uninstall entry removes the app, service, shortcut, and autostart value while preserving `%APPDATA%\Heimdall` settings. Chocolatey's automatic uninstaller handles that registered Inno uninstaller.

## Build and test locally

Run from an elevated PowerShell session for the intended user on a clean Windows 10 or 11 x64 VM. The test script refuses to replace an existing Heimdall install, service, autostart entry, or settings file.

```powershell
pwsh -NoProfile -File .\packaging\chocolatey\build-package.ps1 -ReleaseTag v1.0.1
pwsh -NoProfile -File .\packaging\chocolatey\build-package.ps1 -ReleaseTag v1.0.1 -PreviousStable
pwsh -NoProfile -File .\packaging\chocolatey\test-package.ps1 -PackageDirectory .\dist\chocolatey -CurrentVersion 1.0.1 -PreviousVersion 1.0.0
```

Omit `-ReleaseTag` to package the latest stable GitHub release. The builder requires Chocolatey CLI and internet access. It checks the release asset name, size, URL, and GitHub SHA-256 digest when available; it computes SHA-256, generates the versioned nuspec, runs `choco pack`, and inspects the `.nupkg`. It downloads the installer only to compute and validate its hash; the installer is not bundled.

## Automation

`chocolatey.yml` validates pull requests, manual runs, and published stable GitHub releases on `windows-latest`. Pull requests need no secrets. The workflow finds the previous stable release when available, installs it, upgrades to the target package, and checks the registered app, indexer service, Start menu shortcut, autostart entry, and settings preservation through uninstall. It uploads the validated `.nupkg` artifact. A missing or incomplete release installer fails with its expected asset name; rerun after uploading the asset, or dispatch validation with its tag.

Publishing is separate and manual. After this change is merged to `main`:

1. Create a protected GitHub Actions environment named `chocolatey`; require a maintainer reviewer.
2. Create or verify a maintainer account on Chocolatey Community Repository, then add its API key as environment secret `CHOCOLATEY_API_KEY`.
3. Submit the first package through the `Publish Chocolatey package` workflow with `publish` enabled, then monitor Chocolatey moderation and answer moderator requests.

The publish workflow only runs from `main`, validates and tests again, checks whether that package version already exists, then pushes it. Its input defaults to false. Do not add the key to the validation workflow or expose it to pull requests.

If a release run fails because its asset is missing, upload the official installer and dispatch validation or publishing again with that stable tag. If moderation rejects a package, follow the moderator's findings and coordinate installer changes or VirusTotal false-positive review with the upstream maintainer. While a version is still under moderation, correct and resubmit that same version as directed; use a new package version after approval.

The v1.0.1 GitHub release reports four VirusTotal detections out of 70 vendors for its unsigned installer. Chocolatey Community Repository moderation runs VirusTotal scans, so first submission or updates may need manual review or may be rejected. Do not bypass scans or dismiss detections automatically. Coordinate with upstream on code signing or false-positive submissions: [v1.0.1 release notes](https://github.com/Kryzern/Heimdall/releases/tag/v1.0.1), [VirusTotal report](https://www.virustotal.com/gui/file/766643e8414fa4d2bcfa477a428447daba6b7d842a24f73ecf7dc3afb3d1557d).
