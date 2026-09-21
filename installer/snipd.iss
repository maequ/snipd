; Snipd installer.
;
; Builds a standard Windows setup wizard with five extra pages that collect the
; user's preferences, and writes them to first-run.json beside the executable.
; The app copies that into %APPDATA%\Snipd\config.json the first time it starts,
; so the choices made here are already in effect on first launch.
;
; Why a seed file rather than writing %APPDATA% directly: setup can be run
; elevated as a different account than the one that will actually use the app,
; in which case {userappdata} is the *installer's* profile, not the user's. The
; app resolving it at first run is the only way to be certain it lands in the
; right profile.
;
; Build with:
;   "%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe" installer\snipd.iss
;
; Expects snipd.exe to already be built:
;   npm run tauri build -- --no-bundle

#define AppName       "Snipd"
#define AppVersion    "0.1.0"
#define AppPublisher  "Snipd Contributors"
#define AppUrl        "https://github.com/snipd-app/snipd"
#define AppExe        "snipd.exe"

[Setup]
AppId={{8D3F1A62-5C4E-4E2B-9A77-2E9B6F0C71D4}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}
VersionInfoVersion={#AppVersion}

; Per-user install by default: it needs no elevation, which keeps setup and the
; app running as the same account. The user can still choose an all-users
; install from the dialog if they want one.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
AllowNoIcons=yes

LicenseFile=..\LICENSE
OutputDir=Output
OutputBaseFilename={#AppName}-Setup-{#AppVersion}
SetupIconFile=..\src-tauri\icons\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"

[Files]
Source: "..\src-tauri\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "Launch {#AppName} now"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; The seed file is generated at install time, so setup does not track it.
Type: files; Name: "{app}\first-run.json"
Type: files; Name: "{app}\first-run.applied.json"

[Code]
var
  SaveDirPage:    TInputDirWizardPage;
  NamingPage:     TWizardPage;
  NamingByDate:   TNewRadioButton;
  NamingByPrefix: TNewRadioButton;
  PrefixEdit:     TNewEdit;
  PreviewLabel:   TNewStaticText;
  FormatPage:     TInputOptionWizardPage;
  ClipboardPage:  TInputOptionWizardPage;
  StartupPage:    TInputOptionWizardPage;

{ ---------------------------------------------------------------------------
  Helpers
  --------------------------------------------------------------------------- }

{ Escape a string for embedding in JSON. Windows paths are full of backslashes,
  so this is not optional. }
function JsonEscape(const Value: string): string;
var
  I: Integer;
  Ch: Char;
begin
  Result := '';
  for I := 1 to Length(Value) do
  begin
    Ch := Value[I];
    { Written as if/else rather than a case statement on purpose: a case label
      like #9 starts the line with a hash, which the Inno preprocessor reads as
      a directive and refuses to compile. }
    if Ch = '\' then
      Result := Result + '\\'
    else if Ch = '"' then
      Result := Result + '\"'
    else if Ord(Ch) >= 32 then
      Result := Result + Ch;
    { Control characters are dropped. They cannot legally appear in a Windows
      path or filename prefix, and emitting one raw would break the JSON. }
  end;
end;

function JsonBool(const Value: Boolean): string;
begin
  if Value then Result := 'true' else Result := 'false';
end;

{ Where captures go unless the user picks somewhere else.

  Inno has no constant for the Pictures folder, so this asks the shell for it —
  CSIDL 39 is CSIDL_MYPICTURES. Going through the shell rather than assuming
  %USERPROFILE%\Pictures means a relocated Pictures folder is respected, which
  is also how the app itself resolves it. }
function DefaultSaveFolder: string;
var
  Pictures: string;
begin
  Pictures := GetShellFolderByCSIDL(39, False);
  if Pictures = '' then
    Pictures := ExpandConstant('{%USERPROFILE}\Pictures');
  Result := AddBackslash(Pictures) + 'Snipd';
end;

function ChosenExtension: string;
begin
  { Page C has not necessarily been visited yet when the preview is first
    drawn, but its default is already set, so this is always meaningful. }
  if FormatPage.SelectedValueIndex = 1 then
    Result := 'jpg'
  else
    Result := 'png';
end;

{ Show the user exactly what a filename will look like. The app renders the same
  preview in its Settings screen from the code that actually names files. }
procedure UpdatePreview;
var
  Prefix: string;
begin
  if PreviewLabel = nil then
    Exit;

  if NamingByPrefix.Checked then
  begin
    Prefix := Trim(PrefixEdit.Text);
    if Prefix = '' then
      Prefix := 'Screenshot';
    PreviewLabel.Caption := 'Example:  ' + Prefix + '_001.' + ChosenExtension;
  end
  else
    PreviewLabel.Caption :=
      'Example:  Screenshot_' + GetDateTimeString('yyyy-mm-dd_hh-nn-ss', '-', '-') +
      '.' + ChosenExtension;
end;

procedure NamingChanged(Sender: TObject);
begin
  PrefixEdit.Enabled := NamingByPrefix.Checked;
  UpdatePreview;
end;

{ ---------------------------------------------------------------------------
  Wizard pages
  --------------------------------------------------------------------------- }

procedure InitializeWizard;
begin
  { Page A — where captures are saved. Browsing an existing folder or typing a
    new one both work; Inno creates it if it does not exist. }
  SaveDirPage := CreateInputDirPage(
    wpSelectDir,
    'Choose where screenshots are saved',
    'Snipd saves every capture here automatically.',
    'Every screenshot you take is written to this folder the instant it is taken,' + #13#10 +
    'so nothing is ever lost. You can change this later in Settings.',
    False, '');
  SaveDirPage.Add('');
  SaveDirPage.Values[0] := DefaultSaveFolder;

  { Page B — naming. Built by hand rather than with CreateInputOptionPage so the
    prefix box and the live preview can sit with the radio buttons. }
  NamingPage := CreateCustomPage(
    SaveDirPage.ID,
    'Choose how files are named',
    'Snipd names every capture for you.');

  NamingByDate := TNewRadioButton.Create(WizardForm);
  NamingByDate.Parent := NamingPage.Surface;
  NamingByDate.Top := ScaleY(4);
  NamingByDate.Width := NamingPage.SurfaceWidth;
  NamingByDate.Caption := 'Name by date and time';
  NamingByDate.Checked := True;
  NamingByDate.OnClick := @NamingChanged;

  NamingByPrefix := TNewRadioButton.Create(WizardForm);
  NamingByPrefix.Parent := NamingPage.Surface;
  NamingByPrefix.Top := NamingByDate.Top + NamingByDate.Height + ScaleY(12);
  NamingByPrefix.Width := NamingPage.SurfaceWidth;
  NamingByPrefix.Caption := 'Use my own prefix, numbered automatically';
  NamingByPrefix.OnClick := @NamingChanged;

  PrefixEdit := TNewEdit.Create(WizardForm);
  PrefixEdit.Parent := NamingPage.Surface;
  PrefixEdit.Top := NamingByPrefix.Top + NamingByPrefix.Height + ScaleY(6);
  PrefixEdit.Left := ScaleX(18);
  PrefixEdit.Width := ScaleX(200);
  PrefixEdit.Text := 'Screenshot';
  PrefixEdit.Enabled := False;
  PrefixEdit.OnChange := @NamingChanged;

  PreviewLabel := TNewStaticText.Create(WizardForm);
  PreviewLabel.Parent := NamingPage.Surface;
  PreviewLabel.Top := PrefixEdit.Top + PrefixEdit.Height + ScaleY(18);
  PreviewLabel.Width := NamingPage.SurfaceWidth;
  PreviewLabel.Caption := '';

  { Page C — file format. }
  FormatPage := CreateInputOptionPage(
    NamingPage.ID,
    'Choose a file format',
    'This is the format Snipd saves in by default.',
    'You can change this later in Settings.',
    True, False);
  FormatPage.Add('PNG — lossless, sharper text, larger files');
  FormatPage.Add('JPEG — smaller files, slight quality loss');
  FormatPage.SelectedValueIndex := 0;

  { Page D — clipboard. }
  ClipboardPage := CreateInputOptionPage(
    FormatPage.ID,
    'Clipboard',
    'Snipd can put every capture straight on the clipboard.',
    'With this on, a capture is ready to paste the moment you take it.',
    False, False);
  ClipboardPage.Add('Automatically copy every capture to the clipboard');
  ClipboardPage.Values[0] := True;

  { Page E — startup. }
  StartupPage := CreateInputOptionPage(
    ClipboardPage.ID,
    'Startup',
    'Snipd runs quietly in the system tray.',
    'It needs to be running for the capture shortcut to work.',
    False, False);
  StartupPage.Add('Start Snipd when Windows starts');
  StartupPage.Add('When started by Windows, start minimised to the tray');
  StartupPage.Values[0] := True;
  StartupPage.Values[1] := True;

  UpdatePreview;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := False;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  { The format choice feeds the filename preview, so refresh it on the way back. }
  if CurPageID = NamingPage.ID then
    UpdatePreview;
end;

{ ---------------------------------------------------------------------------
  Writing the answers
  --------------------------------------------------------------------------- }

procedure WriteSeedFile;
var
  Json: string;
  Prefix: string;
  Mode: string;
  Format: string;
begin
  if NamingByPrefix.Checked then
    Mode := 'prefix'
  else
    Mode := 'datetime';

  Prefix := Trim(PrefixEdit.Text);
  if Prefix = '' then
    Prefix := 'Screenshot';

  if FormatPage.SelectedValueIndex = 1 then
    Format := 'jpeg'
  else
    Format := 'png';

  { Shape must match config::Settings. Anything omitted falls back to the app's
    own defaults, because every field there is #[serde(default)]. }
  Json :=
    '{' + #13#10 +
    '  "version": 1,' + #13#10 +
    '  "saveDirectory": "' + JsonEscape(SaveDirPage.Values[0]) + '",' + #13#10 +
    '  "format": "' + Format + '",' + #13#10 +
    '  "naming": {' + #13#10 +
    '    "mode": "' + Mode + '",' + #13#10 +
    '    "prefix": "' + JsonEscape(Prefix) + '",' + #13#10 +
    '    "counter": 1' + #13#10 +
    '  },' + #13#10 +
    '  "clipboard": { "autoCopy": ' + JsonBool(ClipboardPage.Values[0]) + ' },' + #13#10 +
    '  "startup": {' + #13#10 +
    '    "launchOnLogin": ' + JsonBool(StartupPage.Values[0]) + ',' + #13#10 +
    '    "startMinimised": ' + JsonBool(StartupPage.Values[1]) + #13#10 +
    '  }' + #13#10 +
    '}' + #13#10;

  SaveStringToFile(ExpandConstant('{app}\first-run.json'), Json, False);
end;

procedure RegisterStartupEntry;
var
  Command: string;
begin
  { The --autostart flag is what tells the app that "start minimised" applies to
    this launch, as opposed to the user opening it deliberately. }
  Command := '"' + ExpandConstant('{app}\{#AppExe}') + '" --autostart';

  if StartupPage.Values[0] then
    RegWriteStringValue(HKEY_CURRENT_USER,
      'Software\Microsoft\Windows\CurrentVersion\Run', '{#AppName}', Command)
  else
    RegDeleteValue(HKEY_CURRENT_USER,
      'Software\Microsoft\Windows\CurrentVersion\Run', '{#AppName}');
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    ForceDirectories(SaveDirPage.Values[0]);
    WriteSeedFile;
    RegisterStartupEntry;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    { Leave captures and settings alone — removing the app should never remove
      the user's screenshots. Only the startup entry goes. }
    RegDeleteValue(HKEY_CURRENT_USER,
      'Software\Microsoft\Windows\CurrentVersion\Run', '{#AppName}');
end;

{ Summary shown on the Ready page, so the choices can be checked before install. }
function UpdateReadyMemo(const Space, NewLine, MemoUserInfoInfo, MemoDirInfo,
  MemoTypeInfo, MemoComponentsInfo, MemoGroupInfo, MemoTasksInfo: string): string;
var
  S: string;
begin
  S := MemoDirInfo + NewLine + NewLine;
  S := S + 'Screenshots folder:' + NewLine + Space + SaveDirPage.Values[0] + NewLine + NewLine;

  S := S + 'Naming:' + NewLine + Space;
  if NamingByPrefix.Checked then
    S := S + 'Prefix "' + Trim(PrefixEdit.Text) + '", numbered' + NewLine + NewLine
  else
    S := S + 'Date and time' + NewLine + NewLine;

  S := S + 'Format:' + NewLine + Space;
  if FormatPage.SelectedValueIndex = 1 then
    S := S + 'JPEG' + NewLine + NewLine
  else
    S := S + 'PNG' + NewLine + NewLine;

  S := S + 'Behaviour:' + NewLine;
  if ClipboardPage.Values[0] then
    S := S + Space + 'Copy every capture to the clipboard' + NewLine;
  if StartupPage.Values[0] then
    S := S + Space + 'Start with Windows' + NewLine;
  if StartupPage.Values[1] then
    S := S + Space + 'Start minimised to the tray' + NewLine;

  if MemoTasksInfo <> '' then
    S := S + NewLine + MemoTasksInfo;

  Result := S;
end;
