; Inno Setup script for the Heimdall installer.
; Built by package.ps1:  ISCC /DAppVersion=1.0.0 packaging\heimdall.iss
; Installs per user, like install.ps1. The fast file indexer service is always
; installed too, which asks for one admin prompt.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{6F0C1E52-8B7A-4C3D-9E21-4D8A5B7C2F10}
AppName=Heimdall
AppVersion={#AppVersion}
AppVerName=Heimdall {#AppVersion}
AppPublisher=Ethan Dsouza (Kryzern)
AppPublisherURL=https://github.com/Kryzern/Heimdall
AppSupportURL=https://github.com/Kryzern/Heimdall/issues
AppUpdatesURL=https://github.com/Kryzern/Heimdall/releases
PrivilegesRequired=lowest
DefaultDirName={userpf}\Heimdall
DisableProgramGroupPage=yes
DisableReadyPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile=..\LICENSE
SetupIconFile=heimdall.ico
UninstallDisplayIcon={app}\heimdall.exe
UninstallDisplayName=Heimdall
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
OutputDir=..\dist
OutputBaseFilename=Heimdall-{#AppVersion}-x64-setup

[Files]
Source: "..\target\release\heimdall.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\heimdall-indexer.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\Heimdall"; Filename: "{app}\heimdall.exe"

[Registry]
; Heimdall manages this value itself from start_with_windows; set it here so the
; first sign-in after install starts it, and remove it on uninstall.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Heimdall"; ValueData: """{app}\heimdall.exe"" --background"; Flags: uninsdeletevalue

[Run]
Filename: "{app}\heimdall.exe"; Description: "Start Heimdall (press Alt+Space to open it)"; Flags: nowait postinstall

[Code]
procedure StopHeimdall;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM heimdall.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

function IndexerInstalled: Boolean;
var
  Code: Integer;
begin
  Result := Exec(ExpandConstant('{sys}\sc.exe'), 'query HeimdallIndexer', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopHeimdall;
  Result := '';
end;

// The indexer service needs admin. Ask from here rather than [Run], so saying
// no to the prompt doesn't show an error: Heimdall then indexes by itself.
procedure CurStepChanged(CurStep: TSetupStep);
var
  Code: Integer;
begin
  if CurStep = ssPostInstall then
  begin
    WizardForm.StatusLabel.Caption := 'Installing the file indexer service...';
    ShellExec('runas', ExpandConstant('{app}\heimdall-indexer.exe'), 'install', '', SW_HIDE, ewWaitUntilTerminated, Code);
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Code: Integer;
begin
  if CurUninstallStep = usUninstall then
  begin
    StopHeimdall;
    if IndexerInstalled then
      ShellExec('runas', ExpandConstant('{app}\heimdall-indexer.exe'), 'uninstall', '', SW_HIDE, ewWaitUntilTerminated, Code);
  end;
end;
