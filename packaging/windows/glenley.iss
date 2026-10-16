; Windows installer for glenley (Inno Setup 6, free). Per-user: no admin rights, installs to
; %LOCALAPPDATA%\Programs\glenley, adds a Start menu entry and starts glenley when done.
; Built by .github/workflows/release.yml:  iscc /DVersion=0.1.0 packaging\windows\glenley.iss

#ifndef Version
  #define Version "0.0.0"
#endif

[Setup]
AppId={{8E0D3C61-6A0C-4C8E-9B8B-5C7C1C2E6A10}
AppName=glenley
AppVersion={#Version}
AppPublisher=glenley (open source)
AppPublisherURL=https://github.com/shivambisen/carson-remote
DefaultDirName={localappdata}\Programs\glenley
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
PrivilegesRequired=lowest
OutputDir=..\..\dist
OutputBaseFilename=glenley-setup-{#Version}-windows-x86_64
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayName=glenley
UninstallDisplayIcon={app}\glenley.exe
; glenley is stopped by [Code] below; Windows' Restart Manager can wait forever on a copy that
; isn't responding, so don't use it.
CloseApplications=no
SetupIconFile=..\icons\glenley.ico

[Files]
Source: "..\..\agent\target\release\glenley.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\agent\target\release\glenley-agent.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "..\icons\glenley.ico"; DestDir: "{app}"

[Icons]
Name: "{userprograms}\glenley"; Filename: "{app}\glenley.exe"; IconFilename: "{app}\glenley.ico"

[Run]
Filename: "{app}\glenley.exe"; Description: "Start glenley"; Flags: nowait postinstall

[UninstallRun]
Filename: "{app}\glenley-agent.exe"; Parameters: "service uninstall"; Flags: runhidden; RunOnceId: "LoginItem"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM glenley.exe /IM glenley-agent.exe"; Flags: runhidden; RunOnceId: "StopApp"

[Code]
// Stop every running glenley (app and command-line agent) before files are replaced.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /T /IM glenley.exe /IM glenley-agent.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(500);
  Result := '';
end;
