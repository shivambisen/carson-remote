; Windows installer for glenley (Inno Setup 6, free). Per-user: no admin rights, installs to
; %LOCALAPPDATA%\Programs\glenley, adds a Start menu entry and starts the app when done.
; Built by .github/workflows/release.yml:  iscc /DVersion=0.1.0 packaging\windows\glenley.iss
;
; The wizard asks for a name. The two signed binaries are installed under that name (e.g.
; "Acme Support.exe" and "Acme Support-agent.exe"), so the Start menu, Task Manager and the app
; window all show it. Renaming the files does not touch their bytes, so the code signature stays
; valid. The app reads its own file name to know what to call itself (see tray.rs `brand`).
;
; It also offers to pick a .ico file to use instead of the default mark: the Start menu shortcut,
; the taskbar and the uninstall entry use it directly (plain files Explorer reads, same as the
; name above, untouched by signing); the running app window and tray pick it up too (it's copied
; in as "logo.ico" next to the binaries, and tray.rs's `custom_logo` loads it from there, falling
; back to the default mark if it's missing or unreadable).
;
; One more thing it offers, opt-in: installing glenley-input-svc, a small Windows service that
; runs as SYSTEM so remote control still works when another elevated window has focus, or during
; a UAC prompt / the lock screen - a normal process (this app included) cannot send input there.
; The app and the rest of this install stay unprivileged either way; only that one step asks for
; its own, separate admin permission (via ShellExec's "runas"), and declining it just means
; glenley falls back to injecting input directly, as it always has.

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
UninstallDisplayIcon={code:GetIcon}
; The app is stopped by [Code] below; Windows' Restart Manager can wait forever on a copy that
; isn't responding, so don't use it.
CloseApplications=no
SetupIconFile=..\icons\glenley.ico

[Files]
Source: "..\..\agent\target\release\glenley.exe"; DestDir: "{app}"; DestName: "{code:GetExe}"; Flags: ignoreversion
Source: "..\..\agent\target\release\glenley-agent.exe"; DestDir: "{app}"; DestName: "{code:GetAgentExe}"; Flags: ignoreversion
Source: "..\..\agent\target\release\glenley-input-svc.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "..\icons\glenley.ico"; DestDir: "{app}"
; The chosen logo, if any: copied in by [Code] below (CurStepChanged), not listed here, since it
; comes from wherever the user's file picker pointed and isn't part of this installer.

[Icons]
Name: "{userprograms}\{code:GetDisplay}"; Filename: "{app}\{code:GetExe}"; IconFilename: "{code:GetIcon}"

[Run]
Filename: "{app}\{code:GetExe}"; Description: "Start now"; Flags: nowait postinstall

[UninstallRun]
Filename: "{app}\{code:GetAgentExe}"; Parameters: "service uninstall"; Flags: runhidden; RunOnceId: "LoginItem"
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM ""{code:GetExe}"" /IM ""{code:GetAgentExe}"""; Flags: runhidden; RunOnceId: "StopApp"
; glenley-input-svc.exe uninstall runs from [Code]'s CurUninstallStepChanged below instead of
; here: removing it needs its own elevation (ShellExec "runas"), which [UninstallRun] has no
; flag for.

[Code]
var
  NamePage: TInputQueryWizardPage;
  LogoPage: TInputFileWizardPage;
  ServicePage: TWizardPage;
  EnableServiceCheck: TNewCheckBox;

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

// Whether a usable .ico was chosen: it must end in ".ico" and exist (an empty field, the
// ordinary case, means "keep the default").
function HasCustomLogo: Boolean;
var
  f: String;
begin
  f := LogoPage.Values[0];
  Result := (f <> '') and (CompareText(Copy(f, Length(f) - 3, 4), '.ico') = 0) and FileExists(f);
end;

// The icon for the Start menu shortcut and the uninstall entry: the chosen logo once it's been
// copied in by CurStepChanged below, else the default mark.
function GetIcon(Param: String): String;
begin
  if HasCustomLogo then
    Result := ExpandConstant('{app}\logo.ico')
  else
    Result := ExpandConstant('{app}\glenley.ico');
end;

procedure InitializeWizard;
begin
  NamePage := CreateInputQueryPage(wpWelcome,
    'Name this app',
    'How should this app appear on this computer?',
    'This name is shown in the Start menu, the app window and Task Manager. Keep "glenley" or type your own, then click Next.');
  NamePage.Add('Name:', False);
  NamePage.Values[0] := 'glenley';

  LogoPage := CreateInputFilePage(NamePage.ID,
    'Pick a logo (optional)',
    'Use your own icon instead of the glenley mark?',
    'Shown in the Start menu, the taskbar and the app window. Needs to be a .ico file (most icon ' +
    'editors and online converters can make one from a PNG). Leave this blank to keep the glenley mark.');
  LogoPage.Add('Icon file (.ico):', 'Icon files|*.ico', '.ico');
  LogoPage.Values[0] := '';

  ServicePage := CreateCustomPage(LogoPage.ID,
    'Full remote control',
    'Let glenley control this computer even when it is not the focused app?');
  EnableServiceCheck := TNewCheckBox.Create(ServicePage);
  EnableServiceCheck.Parent := ServicePage.Surface;
  EnableServiceCheck.Width := ServicePage.SurfaceWidth;
  EnableServiceCheck.Caption := 'Enable full remote control (recommended)';
  EnableServiceCheck.Checked := True;
  with TNewStaticText.Create(ServicePage) do
  begin
    Parent := ServicePage.Surface;
    Top := EnableServiceCheck.Top + EnableServiceCheck.Height + ScaleY(8);
    Width := ServicePage.SurfaceWidth;
    WordWrap := True;
    Caption :=
      'Without this, remote control can silently stop working whenever another window on ' +
      'this computer is running "as Administrator", or during a Windows security prompt. ' +
      'This installs a small Windows service (running as SYSTEM) to fix that - Windows will ' +
      'ask for admin permission once, separately from the rest of this install, which stays ' +
      'unprivileged either way. Declining just means remote control keeps working as it does ' +
      'today, with that one limitation.';
  end;
end;

// Copies the chosen logo in under a fixed name, so GetIcon and the running app (tray.rs
// `custom_logo`) always know where to look. Also installs glenley-input-svc if the user kept
// that box checked - its own elevation request (ShellExec "runas"), separate from this whole
// install staying unprivileged. A decline or failure here is not fatal: glenley just falls
// back to injecting input directly, as it always has.
procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
begin
  if CurStep <> ssPostInstall then
    exit;
  if HasCustomLogo then
    FileCopy(LogoPage.Values[0], ExpandConstant('{app}\logo.ico'), False);
  // ewNoWait, not ewWaitUntilTerminated: the UAC prompt this triggers (or a Windows SmartScreen
  // check on this freshly-downloaded exe, which can precede it) must never be able to block the
  // rest of Setup. If it never gets answered, this step just never finishes installing the
  // service - the app and the rest of the install are already done and unaffected either way.
  if EnableServiceCheck.Checked then
    ShellExec('runas', ExpandConstant('{app}\glenley-input-svc.exe'), 'install', '',
      SW_SHOWNORMAL, ewNoWait, ResultCode);
end;

// Mirrors the install step: removing the service needs its own elevation too, and must not be
// able to block the uninstaller either. Quietly a no-op if it was never installed.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ResultCode: Integer;
begin
  if CurUninstallStep = usUninstall then
    ShellExec('runas', ExpandConstant('{app}\glenley-input-svc.exe'), 'uninstall', '',
      SW_SHOWNORMAL, ewNoWait, ResultCode);
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
