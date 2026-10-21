; Windows installer for glenley (Inno Setup 6, free). Per-user: no admin rights, installs to
; %LOCALAPPDATA%\Programs\glenley, adds a Start menu entry and starts the app when done.
; Built by .github/workflows/release.yml:  iscc /DVersion=0.1.0 packaging\windows\glenley.iss
;
; The wizard asks for a name. The two signed binaries are installed under that name (e.g.
; "Acme Support.exe" and "Acme Support-agent.exe"), so the Start menu, Task Manager and the app
; window all show it. Renaming the files does not touch their bytes, so the code signature stays
; valid. The app reads its own file name to know what to call itself (see tray.rs `brand`).

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
UninstallDisplayName={code:GetDisplay}
UninstallDisplayIcon={app}\{code:GetExe}
; The app is stopped by [Code] below; Windows' Restart Manager can wait forever on a copy that
; isn't responding, so don't use it.
CloseApplications=no
SetupIconFile=..\icons\glenley.ico

[Files]
Source: "..\..\agent\target\release\glenley.exe"; DestDir: "{app}"; DestName: "{code:GetExe}"; Flags: ignoreversion
Source: "..\..\agent\target\release\glenley-agent.exe"; DestDir: "{app}"; DestName: "{code:GetAgentExe}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "..\icons\glenley.ico"; DestDir: "{app}"

[Icons]
Name: "{userprograms}\{code:GetDisplay}"; Filename: "{app}\{code:GetExe}"; IconFilename: "{app}\glenley.ico"

[Run]
Filename: "{app}\{code:GetExe}"; Description: "Start now"; Flags: nowait postinstall

[UninstallRun]
Filename: "{app}\{code:GetAgentExe}"; Parameters: "service uninstall"; Flags: runhidden; RunOnceId: "LoginItem"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM ""{code:GetExe}"" /IM ""{code:GetAgentExe}"""; Flags: runhidden; RunOnceId: "StopApp"

[Code]
var
  NamePage: TInputQueryWizardPage;

// Keep only characters that are safe in a Windows file name, cap the length, and don't let the
// name end in "-agent" (that suffix tells the app which binary is the background one).
function Sanitize(S: String): String;
var
  i: Integer;
  c: Char;
  r: String;
begin
  r := '';
  for i := 1 to Length(S) do
  begin
    c := S[i];
    if ((c >= 'a') and (c <= 'z')) or ((c >= 'A') and (c <= 'Z')) or
       ((c >= '0') and (c <= '9')) or (c = ' ') or (c = '-') or (c = '_') then
      r := r + c;
  end;
  r := Trim(r);
  if Length(r) > 40 then
    r := Trim(Copy(r, 1, 40));
  if (Length(r) >= 6) and (CompareText(Copy(r, Length(r) - 5, 6), '-agent') = 0) then
    r := Trim(Copy(r, 1, Length(r) - 6));
  Result := r;
end;

function GetDisplay(Param: String): String;
begin
  Result := Sanitize(NamePage.Values[0]);
  if Result = '' then
    Result := 'glenley';
end;

function GetExe(Param: String): String;
begin
  Result := GetDisplay('') + '.exe';
end;

function GetAgentExe(Param: String): String;
begin
  Result := GetDisplay('') + '-agent.exe';
end;

procedure InitializeWizard;
begin
  NamePage := CreateInputQueryPage(wpWelcome,
    'Name this app',
    'How should this app appear on this computer?',
    'This name is shown in the Start menu, the app window and Task Manager. Keep "glenley" or type your own, then click Next.');
  NamePage.Add('Name:', False);
  NamePage.Values[0] := 'glenley';
end;

// Stop every running copy (the app and its background binary) before files are replaced. End the
// logon task first (that stops whatever it launched, under any earlier name), then kill the chosen
// name and the default names.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\schtasks.exe'), '/End /TN glenley-agent', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{sys}\taskkill.exe'),
    '/F /T /IM "' + GetExe('') + '" /IM "' + GetAgentExe('') + '" /IM glenley.exe /IM glenley-agent.exe',
    '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(500);
  Result := '';
end;
