; Splitter — Windows Installer
; Build: iscc /DMyAppVersion=X.Y.Z installer\installer.iss
; Output: dist\splitter-setup.exe

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif

#define MyAppName      "Splitter"
#define MyAppPublisher "Bennekrouf"
#define MyAppURL       "https://github.com/bennekrouf/splitter"
#define MyAppExeName   "splitter.exe"

[Setup]
AppId={{82950AB2-0644-4D0B-9533-C311E3BEB643}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases/latest
; Per-user install — Splitter needs no elevation and should never require an
; admin prompt to run. Its webview data lives in %LOCALAPPDATA%\Splitter\
; (created at launch); cutlists are saved next to the recordings.
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
OutputDir=..\dist
OutputBaseFilename=splitter-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; Per-user by default, so someone on a locked-down machine installs with
; no UAC prompt at all. IT keeps the machine-wide path via the command line:
;   installer.exe /ALLUSERS /VERYSILENT
; One artifact serves both audiences instead of forcing everyone to elevate.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
UninstallDisplayName={#MyAppName} {#MyAppVersion}
CloseApplications=yes
; Branding — uses assets\icon.ico if present. Comment these out if the file
; doesn't exist yet (Inno will fail with a clear error otherwise).
#if FileExists(AddBackslash(SourcePath) + "..\crates\app\assets\icon.ico")
SetupIconFile=..\crates\app\assets\icon.ico
UninstallDisplayIcon={app}\icon.ico
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; \
  Description: "Create a &desktop shortcut"; \
  GroupDescription: "Additional shortcuts:"

[Files]
Source: "..\target\release\splitter.exe";        DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\WebView2Loader.dll";  DestDir: "{app}"; Flags: ignoreversion
; Ship the .ico alongside the .exe so shortcuts can point at it explicitly —
; otherwise some Windows builds fail to extract the embedded icon for shortcut
; display, leaving a generic icon on the Start menu / desktop.
#if FileExists(AddBackslash(SourcePath) + "..\crates\app\assets\icon.ico")
Source: "..\crates\app\assets\icon.ico";                    DestDir: "{app}"; Flags: ignoreversion
#endif

[Icons]
; {autodesktop}/{group} resolve per privilege level — matching {autopf} above.
; Hardcoding {autodesktop} here with PrivilegesRequired=lowest fails with
; "IPersistFile::Save failed; code 0x80070005" on every non-elevated install.
#if FileExists(AddBackslash(SourcePath) + "..\crates\app\assets\icon.ico")
Name: "{group}\{#MyAppName}";           Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\icon.ico"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}";   Filename: "{app}\{#MyAppExeName}"; IconFilename: "{app}\icon.ico"; Tasks: desktopicon
#else
Name: "{group}\{#MyAppName}";           Filename: "{app}\{#MyAppExeName}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}";   Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon
#endif

[Run]
Filename: "{app}\{#MyAppExeName}"; \
  Description: "Launch {#MyAppName}"; \
  Flags: nowait postinstall skipifsilent

; ── Upgrade detection ─────────────────────────────────────────────────────────
; Reads the previously installed version from the registry, shows a confirmation
; dialog with both version numbers, and silently removes the old install before
; copying new files. Settings/data are preserved (not managed by Inno).
[Code]

function GetInstalledVersion(): String;
var
  RegKey: String;
  Ver:    String;
begin
  RegKey := 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{82950AB2-0644-4D0B-9533-C311E3BEB643}_is1';
  if not RegQueryStringValue(HKLM, RegKey, 'DisplayVersion', Ver) then
    if not RegQueryStringValue(HKCU, RegKey, 'DisplayVersion', Ver) then
      Ver := '';
  Result := Ver;
end;

function GetUninstallString(): String;
var
  RegKey:    String;
  UninstStr: String;
begin
  RegKey := 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{82950AB2-0644-4D0B-9533-C311E3BEB643}_is1';
  if not RegQueryStringValue(HKLM, RegKey, 'QuietUninstallString', UninstStr) then
    if not RegQueryStringValue(HKCU, RegKey, 'QuietUninstallString', UninstStr) then
      UninstStr := '';
  Result := UninstStr;
end;

function InitializeSetup(): Boolean;
var
  InstalledVer: String;
  NewVer:       String;
  Msg:          String;
  UninstStr:    String;
  ResultCode:   Integer;
  NL:           String;
begin
  Result := True;
  NL := #13#10;

  InstalledVer := GetInstalledVersion();
  if InstalledVer = '' then
    Exit;   // fresh install

  NewVer := '{#MyAppVersion}';

  if InstalledVer = NewVer then
    Msg := 'Version ' + InstalledVer + ' of {#MyAppName} is already installed.' + NL + NL +
           'Do you want to reinstall it?'
  else
    Msg := '{#MyAppName} is already installed.' + NL + NL +
           '  Installed version:  ' + InstalledVer + NL +
           '  New version:        ' + NewVer + NL + NL +
           'The old version will be removed before installing the new one.' + NL +
           'Your settings and data will be preserved.' + NL + NL +
           'Continue?';

  if MsgBox(Msg, mbConfirmation, MB_YESNO) = IDNO then
  begin
    Result := False;
    Exit;
  end;

  // Silent uninstall of the previous version
  UninstStr := GetUninstallString();
  if UninstStr <> '' then
  begin
    Exec('>', UninstStr, '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Sleep(500);
  end;
end;
