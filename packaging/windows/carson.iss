; Windows installer for carson (Inno Setup 6, free). Per-user: no admin rights, installs to
; %LOCALAPPDATA%\Programs\carson, adds a Start menu entry and starts carson when done.
; Built by .github/workflows/release.yml:  iscc /DVersion=0.1.0 packaging\windows\carson.iss

#ifndef Version
  #define Version "0.0.0"
#endif

[Setup]
AppId={{8E0D3C61-6A0C-4C8E-9B8B-5C7C1C2E6A10}
AppName=carson
AppVersion={#Version}
AppPublisher=carson (open source)
AppPublisherURL=https://github.com/shivambisen/carson-remote
DefaultDirName={localappdata}\Programs\carson
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
PrivilegesRequired=lowest
OutputDir=..\..\dist
OutputBaseFilename=carson-setup-{#Version}-windows-x86_64
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayName=carson
UninstallDisplayIcon={app}\carson.exe
CloseApplications=force

[Files]
Source: "..\..\agent\target\release\carson.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\agent\target\release\carson-agent.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"

[Icons]
Name: "{userprograms}\carson"; Filename: "{app}\carson.exe"

[Run]
Filename: "{app}\carson.exe"; Description: "Start carson"; Flags: nowait postinstall

[UninstallRun]
Filename: "{app}\carson-agent.exe"; Parameters: "service uninstall"; Flags: runhidden; RunOnceId: "LoginItem"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM carson.exe"; Flags: runhidden; RunOnceId: "StopApp"

[Code]
// Stop a running carson before files are replaced (updates).
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM carson.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := '';
end;
