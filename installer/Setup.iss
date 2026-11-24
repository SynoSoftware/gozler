#if Ver < EncodeVer(7, 1, 0)
  #error Inno Setup 7.1 or later is required.
#endif
#ifndef AppVersion
  #error Run installer\Build.ps1, which supplies the version from Cargo.toml.
#endif

#define AppName "Gozler"
#define AppId "gozler"

; The service name, gozler_core::proto::SERVICE_NAME. It stays "gozler" across
; versions, and it is not "goz", the upstream service installed beside it.
#define ServiceName "gozler"
; The install folder and the folder of the service log that
; init_service_tracing in service.rs writes.
#define AppFolder "{autopf}\Gozler"
#define LogFolder "{win}\Logs\Gozler"
; The window's single-instance mutex, window class and exit request, and the
; answer that it exits: INSTANCE_MUTEX, WINDOW_CLASS, EXIT_MESSAGE and
; EXIT_CLOSES in gozler-ui's app module.
#define InstanceMutex "Local\gozler.ui"
#define WindowClass "gozler.ui"
#define ExitMessage "gozler.ui.exit"
#define ExitCloses 1

[Setup]
AppId={#AppId}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppCopyright=Copyright (C) 2026 Synapse N.V. and Mustafa Ahci
AppPublisherURL={#AppUrl}
AppUpdatesURL={#AppUrl}/releases/latest
; Installed apps shows this icon; without it, the uninstaller's generic one.
UninstallDisplayIcon={app}\gozler-ui.exe
VersionInfoVersion={#AppVersion}
DefaultDirName={#AppFolder}
UsePreviousAppDir=yes
; Installing the service and the machine PATH entry needs administrator rights.
PrivilegesRequired=admin
; The x86 setup engine is smaller; the installed binaries stay x64.
SetupArchitecture=x86
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
DisableDirPage=no
DirExistsWarning=no
DisableReadyPage=yes
AllowNetworkDrive=no
DisableProgramGroupPage=yes
DisableWelcomePage=yes
WizardStyle=modern dynamic
WizardSizePercent=120,150
OutputDir=..\target\installer
OutputBaseFilename=gozler-{#AppVersion}-x64-Setup
Compression=lzma2/max
SolidCompression=yes
ChangesEnvironment=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Messages]
DiskSpaceMBLabel=[mb] MB required.
UninstallAppRunningError={#AppName} is running. To close it, choose File > Exit in its window, or Exit on the menu of its icon in the notification area.%n%nThen click OK to continue, or Cancel to stop uninstalling.

[CustomMessages]
WhatsNew=What's new
CloseNotes=Close
KeepSettings=Keep current settings
ChangeSettings=Change settings
InstallTitle=Welcome to Gozler
UpdateTitle=Update Gozler
InstallDescription=Find files instantly on your PC. Install version {#AppVersion}.
UpdateDescription=Install version {#AppVersion}.
KeepHint=Your search index and settings will be kept. The search service will restart during installation.
ReadSettingsFailed=Setup couldn't read your settings. Your current settings will be kept. You can change them under File > Settings.
AvailableSpace=%1 MB available.
SpaceUnknown=Available space couldn't be read.
InstallFolder=Install folder
StandardInstall=Standard install (recommended)
AdvancedInstall=Advanced install
StandardNames=Weekly SSD write alerts%nStart at sign-in%nAfter closing%nShow Search shortcut
OnValue=On
KeepRunningValue=Keep running in the notification area
WindowsShortcutSummary=%1 (replaces Windows Search and Win+Shift+S)
ServiceSummary=File indexing continues when the app is closed.
StartupHeading=Startup and alerts
AlertHint=Alerts work only while Gozler is running.
StartAtSignIn=Start Gozler when I &sign in
SearchShortcut=&Use %1 to show Search (replaces Windows Search and Win+Shift+S)
SearchShortcutHint=For other shortcuts, open File > Settings > Keyboard.
KeepShortcut=Keep %1 unless you select %2.
KeepRunning=&Keep running in the notification area after I close the window
DiskWarning=&Warn me about heavy weekly SSD writes
SetupSettingsFailed=Your selected settings couldn't be saved.
CommandLineFailed=Command-line setup couldn't be completed.
ResetSettingsFailed=Your saved settings couldn't be removed.
ResetSharedFailed=The shared search settings couldn't be reset.
StartService=Starting the {#AppName} service...
ServiceFailed=The search service couldn't be started.
CleanupCaption=Keep or remove other programs
CleanupDescription={#AppName} works with or without these programs.
CleanupText=For each program you select, Setup deletes its files and removes its folder from PATH. Other files in that folder stay.
CleanGoz=Remove goz from %1
CleanOldGozler=Remove the other copy of {#AppName} from %1
CleaningUp=Finishing installation...
GozFailed=Setup couldn't remove goz (error code %1).%n%nTo remove goz's service, open a terminal as administrator and run:%n  pushd "%2"%n  .\gozd.exe uninstall%n%nIf the command succeeds, delete goz.exe and gozd.exe from that folder.
SeeEveryFileCaption=Search visibility
SeeEveryFileText=Includes names in folders you can't open. It doesn't give you access to those files.
SeeEveryFileItem=Let %1 see the names of every file on this PC
WindowStays={#AppName} is still running, so Setup can't replace it.%n%nExit {#AppName}, then try again.

[Files]
#ifdef WhatsNewFile
Source: "{#WhatsNewFile}"; Flags: dontcopy noencryption
#endif
Source: "..\target\release\gozlerd.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\gozler.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\gozler-ui.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\gozler-ui.exe"

[Run]
; The window runs as the person who started the setup, not as administrator,
; so its settings, its sign-in entry and its File Explorer command are that
; person's.
Filename: "{app}\gozler-ui.exe"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: postinstall nowait skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{app}\gozlerd.exe"; Parameters: "uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveService"

[UninstallDelete]
Type: filesandordirs; Name: "{#LogFolder}"

[Code]
const
  EnvironmentKey = 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';
  SMTO_ABORTIFHUNG = 2;
  SMTO_NOTIMEOUTIFNOTHUNG = 8;
  // The service's settings key and its list of users and groups who see
  // every file: SETTINGS_KEY and SEE_EVERY_FILE_VALUE in gozler-core's
  // include module.
  SettingsKey = 'SOFTWARE\Gozler';
  SeeEveryFileValue = 'SeeEveryFile';
  DefaultShortcut = 'Win+S';
  RowSpacing = 26;
  HeadingSpacing = 32;
  SectionSpacing = 20;
  WTS_CURRENT_SESSION = $FFFFFFFF;
  WTSUserName = 5;
  WTSDomainName = 7;

type
  // A goz or an earlier gozler that people run: one that a service runs or
  // that a PATH finds. Copies elsewhere, such as backups, are never found.
  TFound = record
    Folder: String;
    IsGoz: Boolean;
    // The program its service runs, or '' when only a PATH finds it.
    Daemon: String;
    // Its checkbox on CleanupPage.
    Item: Integer;
  end;

var
  Found: array of TFound;
  // Created only when something is found.
  CleanupPage: TInputOptionWizardPage;
  // Created only when the setup offers SignedInUser a place on the list of
  // users who see every file.
  SeeEveryFile: TNewCheckBox;
  VisibilityHeading, VisibilityHint: TNewStaticText;
  SignedInUser: String;
  StartAtSignIn, KeepRunning, DiskWarning, SearchShortcut: TNewCheckBox;
  AlertHint, ShortcutHint: TNewStaticText;
  SavedShortcut: String;
  Upgrade: Boolean;
  SettingsLoaded: Boolean;
  VisibilityTop: Integer;
  WizardShown: Boolean;
  SetupChoice: TNewComboBox;
  StartupHeading, KeepHint: TNewStaticText;
  StandardNames, StandardValues: array of TNewStaticText;
  SpaceLabel: TNewStaticText;
  SpaceTimer: NativeUInt;
  SpaceCallback: NativeInt;
#ifdef WhatsNewFile
  WhatsNewButton: TNewButton;
#endif

function SetTimer(Window: HWND; Id: NativeUInt; Interval: Cardinal; Callback: NativeInt): NativeUInt;
  external 'SetTimer@user32.dll stdcall';
function KillTimer(Window: HWND; Id: NativeUInt): BOOL;
  external 'KillTimer@user32.dll stdcall';
#ifdef WhatsNewFile
procedure ShowWhatsNew(Sender: TObject);
var
  Dialog: TSetupForm;
  Notes: TRichEditViewer;
  CloseButton: TNewButton;
  NotesFile: String;
  Content: AnsiString;
begin
  NotesFile := ExpandConstant('{tmp}\WhatsNew.rtf');
  if not FileExists(NotesFile) then
    ExtractTemporaryFile('WhatsNew.rtf');
  if not LoadStringFromFile(NotesFile, Content) then exit;
  Dialog := CreateCustomForm(720, 500, True, True);
  try
    Dialog.Caption := CustomMessage('WhatsNew');
    Dialog.BorderStyle := bsSizeable;
    Dialog.BorderIcons := [biSystemMenu, biMaximize];
    Dialog.Constraints.MinWidth := ScaleX(360);
    Dialog.Constraints.MinHeight := ScaleY(240);
    Dialog.Position := poMainFormCenter;
    CloseButton := TNewButton.Create(Dialog);
    CloseButton.Parent := Dialog;
    CloseButton.SetBounds(Dialog.ClientWidth - ScaleX(87), Dialog.ClientHeight - ScaleY(35),
      ScaleX(75), ScaleY(23));
    CloseButton.Anchors := [akRight, akBottom];
    CloseButton.Caption := CustomMessage('CloseNotes');
    CloseButton.ModalResult := mrOk;
    CloseButton.Default := True;
    CloseButton.Cancel := True;
    Notes := TRichEditViewer.Create(Dialog);
    Notes.UseRichEdit := True;
    Notes.Parent := Dialog;
    Notes.SetBounds(ScaleX(12), ScaleY(12), Dialog.ClientWidth - ScaleX(24),
      CloseButton.Top - ScaleY(24));
    Notes.Anchors := [akLeft, akTop, akRight, akBottom];
    Notes.ReadOnly := True;
    Notes.Font.Size := 11;
    Notes.ScrollBars := ssVertical;
    Notes.RTFText := Content;
    Dialog.ActiveControl := Notes;
    Dialog.ShowModal;
  finally
    Dialog.Free;
  end;
end;
#endif

function GetDriveType(Root: String): Cardinal;
  external 'GetDriveTypeW@kernel32.dll stdcall';

procedure SpaceTick(Window: HWND; Message: Cardinal; Id: NativeUInt; Time: DWORD);
var
  Folder, Parent, Available: String;
  Free, Total: Int64;
begin
  if SpaceTimer <> 0 then KillTimer(0, SpaceTimer);
  SpaceTimer := 0;
  Available := CustomMessage('SpaceUnknown');
  Folder := Trim(WizardForm.DirEdit.Text);
  if (Length(Folder) >= 3) and (Folder[2] = ':') and (Folder[3] = '\') then
    if GetDriveType(Copy(Folder, 1, 3)) in [2, 3, 6] then
    begin
      while not DirExists(Folder) do
      begin
        Parent := ExtractFileDir(RemoveBackslashUnlessRoot(Folder));
        if (Parent = '') or (Parent = Folder) then break;
        Folder := Parent;
      end;
      if GetSpaceOnDisk64(Folder, Free, Total) then
      begin
        Available := Format('%.0n', [(Free div 1048576) * 1.0]);
        Available := FmtMessage(CustomMessage('AvailableSpace'), [Available]);
      end;
    end;
  SpaceLabel.Caption := WizardForm.DiskSpaceLabel.Caption + '  ' + Available;
end;

procedure DirectoryChanged(Sender: TObject);
begin
  if SpaceTimer <> 0 then KillTimer(0, SpaceTimer);
  SpaceTimer := SetTimer(0, 0, 250, SpaceCallback);
end;

function ReadSetupChoices: Boolean;
var
  Code, SignIn, Running, Warning: Integer;
  SettingsOutput, Version: String;
  Installed, Minimum: Int64;
begin
  Result := False;
  if not GetVersionNumbersString(ExpandConstant('{app}\gozler-ui.exe'), Version) or
    not StrToVersion(Version, Installed) or not StrToVersion('0.6.2', Minimum) then exit;
  if ComparePackedVersion(Installed, Minimum) < 0 then exit;
  SettingsOutput := ExpandConstant('{tmp}') + '-settings.ini';
  if not SaveStringToFile(SettingsOutput, '', False) then exit;
  try
    try
      if not ExecAsOriginalUser(ExpandConstant('{app}\gozler-ui.exe'),
        '--read-setup-settings "' + SettingsOutput + '"', '', SW_HIDE,
        ewWaitUntilTerminated, Code) or (Code <> 0) then exit;
      SignIn := StrToIntDef(GetIniString('Setup', 'sign_in', '', SettingsOutput), -1);
      Running := StrToIntDef(GetIniString('Setup', 'keep_running', '', SettingsOutput), -1);
      Warning := StrToIntDef(GetIniString('Setup', 'disk_warning', '', SettingsOutput), -1);
      if (SignIn < 0) or (SignIn > 1) or (Running < 0) or (Running > 1) or
        (Warning < 0) or (Warning > 1) then exit;
      StartAtSignIn.Checked := SignIn = 1;
      KeepRunning.Checked := Running = 1;
      DiskWarning.Checked := Warning = 1;
      SavedShortcut := GetIniString('Setup', 'shortcut', '', SettingsOutput);
      SearchShortcut.Checked := SavedShortcut = DefaultShortcut;
      if (SavedShortcut <> '') and (SavedShortcut <> DefaultShortcut) then
      begin
        ShortcutHint.Caption := FmtMessage(CustomMessage('KeepShortcut'),
          [SavedShortcut, DefaultShortcut]) + #13#10 + CustomMessage('SearchShortcutHint');
        ShortcutHint.AdjustHeight;
      end;
      Result := True;
    except
      Log(GetExceptionMessage);
    end;
  finally
    DeleteFile(SettingsOutput);
  end;
end;

procedure SetupChanged(Sender: TObject);
var
  Expanded: Boolean;
  I: Integer;
begin
  Expanded := SetupChoice.ItemIndex = 1;
  if Upgrade and Expanded and not SettingsLoaded then
  begin
    SetupChoice.ItemIndex := 0;
    Expanded := False;
    MsgBox(CustomMessage('ReadSettingsFailed'), mbError, MB_OK);
  end;
  if Upgrade then
    KeepHint.Caption := CustomMessage('KeepHint');
  if not Upgrade then
  begin
    if not Expanded then
    begin
      DiskWarning.Checked := True;
      StartAtSignIn.Checked := True;
      KeepRunning.Checked := True;
      SearchShortcut.Checked := True;
      if SeeEveryFile <> nil then SeeEveryFile.Checked := False;
    end;
    KeepHint.Caption := CustomMessage('ServiceSummary');
    KeepHint.Top := StandardNames[GetArrayLength(StandardNames) - 1].Top + ScaleY(38);
  end;
  KeepHint.AdjustHeight;
  StartupHeading.Visible := Expanded or not Upgrade;
  for I := 0 to GetArrayLength(StandardNames) - 1 do
  begin
    StandardNames[I].Visible := not Upgrade and not Expanded;
    StandardValues[I].Visible := StandardNames[I].Visible;
  end;
  DiskWarning.Visible := Expanded;
  KeepHint.Visible := not Expanded;
  StartAtSignIn.Visible := Expanded;
  KeepRunning.Visible := Expanded;
  AlertHint.Visible := Expanded;
  SearchShortcut.Visible := Expanded;
  ShortcutHint.Visible := Expanded;
  if SeeEveryFile <> nil then
  begin
    VisibilityHeading.Visible := Expanded;
    SeeEveryFile.Visible := Expanded;
    VisibilityHint.Visible := Expanded;
  end;
end;

procedure DeinitializeSetup;
begin
  if SpaceTimer <> 0 then KillTimer(0, SpaceTimer);
end;

function SetupText(const Caption: String; Left, Top, Width: Integer): TNewStaticText;
begin
  Result := TNewStaticText.Create(WizardForm);
  Result.Parent := WizardForm.SelectDirPage;
  Result.AutoSize := False;
  Result.Caption := Caption;
  Result.SetBounds(Left, Top, Width, ScaleY(18));
end;

function SetupOption(const MessageName: String; Top: Integer): TNewCheckBox;
begin
  Result := TNewCheckBox.Create(WizardForm);
  Result.Parent := WizardForm.SelectDirPage;
  Result.Caption := CustomMessage(MessageName);
  Result.SetBounds(WizardForm.ReadyMemo.Left, Top, WizardForm.ReadyMemo.Width, ScaleY(19));
end;

procedure CreateSetupOptions;
var
  Left, Width, Top, I: Integer;
  Names, Values: TStringList;
begin
  Left := WizardForm.ReadyMemo.Left;
  Width := WizardForm.ReadyMemo.Width;
  WizardForm.SelectDirBitmapImage.Visible := False;
  WizardForm.SelectDirBrowseLabel.Visible := False;
  WizardForm.DiskSpaceLabel.Visible := False;
  WizardForm.SelectDirLabel.Caption := CustomMessage('InstallFolder');
  WizardForm.SelectDirLabel.Font.Style := [fsBold];
  WizardForm.SelectDirLabel.SetBounds(Left, 0, Width, ScaleY(18));
  WizardForm.SelectDirLabel.FocusControl := WizardForm.DirEdit;
  WizardForm.DirBrowseButton.SetBounds(Left + Width - ScaleX(75), ScaleY(20), ScaleX(75), WizardForm.DirEdit.Height);
  WizardForm.DirBrowseButton.Enabled := not Upgrade;
  WizardForm.DirEdit.SetBounds(Left, ScaleY(20), Width - ScaleX(85), WizardForm.DirEdit.Height);
  WizardForm.DirEdit.ReadOnly := Upgrade;
  SpaceCallback := CreateCallback(@SpaceTick);
  WizardForm.DirEdit.OnChange := @DirectoryChanged;
  SetupChoice := TNewComboBox.Create(WizardForm);
  SetupChoice.Parent := WizardForm.SelectDirPage;
  SetupChoice.Style := csDropDownList;
  SetupChoice.SetBounds(Left, ScaleY(58), Width, ScaleY(23));
  if Upgrade then
  begin
    SetupChoice.Items.Add(CustomMessage('KeepSettings'));
    SetupChoice.Items.Add(CustomMessage('ChangeSettings'));
  end
  else
  begin
    SetupChoice.Items.Add(CustomMessage('StandardInstall'));
    SetupChoice.Items.Add(CustomMessage('AdvancedInstall'));
  end;
  SetupChoice.ItemIndex := 0;
  SetupChoice.OnChange := @SetupChanged;
  Top := SetupChoice.Top + SetupChoice.Height + ScaleY(SectionSpacing);
  KeepHint := SetupText('', Left, Top, Width);
  KeepHint.WordWrap := True;
  StartupHeading := SetupText(CustomMessage('StartupHeading'), Left, Top, Width);
  StartupHeading.Font.Style := [fsBold];
  Top := Top + ScaleY(HeadingSpacing);
  Names := TStringList.Create;
  Values := TStringList.Create;
  try
    Names.Text := CustomMessage('StandardNames');
    Values.Text := CustomMessage('OnValue') + #13#10 + CustomMessage('OnValue') + #13#10 +
      CustomMessage('KeepRunningValue') + #13#10 +
      FmtMessage(CustomMessage('WindowsShortcutSummary'), [DefaultShortcut]);
    SetArrayLength(StandardNames, Names.Count);
    SetArrayLength(StandardValues, Names.Count);
    for I := 0 to Names.Count - 1 do
    begin
      StandardNames[I] := SetupText(Names[I], Left, Top + ScaleY(I * RowSpacing), ScaleX(160));
      StandardValues[I] := SetupText(Values[I], Left + ScaleX(170),
        Top + ScaleY(I * RowSpacing), Width - ScaleX(170));
    end;
  finally
    Names.Free;
    Values.Free;
  end;
  DiskWarning := SetupOption('DiskWarning', Top);
  StartAtSignIn := SetupOption('StartAtSignIn', Top + ScaleY(RowSpacing));
  KeepRunning := SetupOption('KeepRunning', Top + ScaleY(2 * RowSpacing));
  Top := KeepRunning.Top + KeepRunning.Height + ScaleY(8);
  AlertHint := SetupText(CustomMessage('AlertHint'), Left + ScaleX(18), Top, Width - ScaleX(18));
  AlertHint.AdjustHeight;
  Top := AlertHint.Top + AlertHint.Height + ScaleY(16);
  SearchShortcut := SetupOption('SearchShortcut', Top);
  SearchShortcut.Caption := FmtMessage(CustomMessage('SearchShortcut'), [DefaultShortcut]);
  ShortcutHint := SetupText(CustomMessage('SearchShortcutHint'), Left + ScaleX(18),
    Top + SearchShortcut.Height + ScaleY(4), Width - ScaleX(18));
  ShortcutHint.WordWrap := True;
  ShortcutHint.AdjustHeight;
  VisibilityTop := ShortcutHint.Top + ShortcutHint.Height + ScaleY(SectionSpacing);
  SpaceLabel := SetupText('', Left, WizardForm.SelectDirPage.Height - ScaleY(20), Width);
  SetupChanged(nil);
  SpaceTick(0, 0, 0, 0);
end;

function SaveSetupChoices: Boolean;
var
  Code: Integer;
  Shortcut, Parameters: String;
begin
  Result := True;
  if DiskWarning = nil then exit;
  if Upgrade then
    if (SetupChoice.ItemIndex = 0) or not SettingsLoaded then exit;
  Shortcut := '-';
  if SearchShortcut.Checked then
    Shortcut := DefaultShortcut
  else if (SavedShortcut <> '') and (SavedShortcut <> DefaultShortcut) then
    Shortcut := SavedShortcut;
  Parameters := Format('--setup-settings %d %d %d %s', [Ord(StartAtSignIn.Checked),
    Ord(KeepRunning.Checked), Ord(DiskWarning.Checked), Shortcut]);
  try
    Result := ExecAsOriginalUser(ExpandConstant('{app}\gozler-ui.exe'), Parameters, '', SW_HIDE,
      ewWaitUntilTerminated, Code) and (Code = 0);
  except
    Log(GetExceptionMessage);
    Result := False;
  end;
  if not Result then
    Log(Format('Saving setup choices failed: code=%d', [Code]));
end;

function NormalFolder(Folder: String): String;
begin
  Folder := Trim(Folder);
  StringChangeEx(Folder, '"', '', True);
  Result := RemoveBackslashUnlessRoot(Folder);
end;

function SameFolder(const A, B: String): Boolean;
begin
  Result := CompareText(NormalFolder(A), NormalFolder(B)) = 0;
end;

// Puts the install folder on the machine PATH just before the first folder
// there that holds another gozler.exe, or at the end when none does, so the
// gozler this setup installs is the one a terminal runs even when an older
// copy is kept. The machine PATH comes before the user PATH, so a copy on
// the user PATH never wins.
function AddAppToPath: Boolean;
var
  Original, Path, Kept, Entry, App: String;
  Semicolon: Integer;
  Placed: Boolean;
begin
  Result := False;
  App := ExpandConstant('{app}');
  if not RegQueryStringValue(HKLM, EnvironmentKey, 'Path', Original) then
    if RegValueExists(HKLM, EnvironmentKey, 'Path') then exit;
  Path := Original + ';';
  Kept := '';
  Placed := False;
  while Path <> '' do
  begin
    Semicolon := Pos(';', Path);
    Entry := Copy(Path, 1, Semicolon - 1);
    Delete(Path, 1, Semicolon);
    if (Entry = '') or SameFolder(Entry, App) then
      continue;
    if not Placed and (Pos('%', Entry) = 0) and
      FileExists(NormalFolder(Entry) + '\gozler.exe') then
    begin
      Kept := Kept + App + ';';
      Placed := True;
    end;
    Kept := Kept + Entry + ';';
  end;
  if not Placed then
    Kept := Kept + App + ';';
  Kept := Copy(Kept, 1, Length(Kept) - 1);
  Result := (Kept = Original) or
    RegWriteExpandStringValue(HKLM, EnvironmentKey, 'Path', Kept);
end;

// Removes every entry that names Folder from the PATH in RootKey: the machine
// PATH under HKLM or the user PATH under HKCU. An entry may quote the folder
// or end with a backslash; every other entry stays as written.
procedure RemoveFolderFromPath(const RootKey: Integer; const SubKey, Folder: String);
var
  Path, Kept, Entry: String;
  Semicolon: Integer;
  Changed: Boolean;
begin
  if not RegQueryStringValue(RootKey, SubKey, 'Path', Path) then exit;
  Kept := '';
  Changed := False;
  Path := Path + ';';
  while Path <> '' do
  begin
    Semicolon := Pos(';', Path);
    Entry := Copy(Path, 1, Semicolon - 1);
    Delete(Path, 1, Semicolon);
    if (Entry <> '') and SameFolder(Entry, Folder) then
      Changed := True
    else
      Kept := Kept + Entry + ';';
  end;
  if Changed then
    RegWriteExpandStringValue(RootKey, SubKey, 'Path', Copy(Kept, 1, Length(Kept) - 1));
end;

// The program the service Name runs, from an ImagePath such as
// "C:\Program Files\goz\gozd.exe" run --service, or '' when there is none.
function ServiceProgram(const Name: String): String;
var
  ImagePath: String;
  Quote: Integer;
begin
  Result := '';
  if not RegQueryStringValue(HKLM, 'SYSTEM\CurrentControlSet\Services\' + Name,
    'ImagePath', ImagePath) then exit;
  if Copy(ImagePath, 1, 1) = '"' then
  begin
    Delete(ImagePath, 1, 1);
    Quote := Pos('"', ImagePath);
    if Quote > 0 then
      Result := Copy(ImagePath, 1, Quote - 1);
  end
  else if Pos(' ', ImagePath) > 0 then
    Result := Copy(ImagePath, 1, Pos(' ', ImagePath) - 1)
  else
    Result := ImagePath;
  if not FileExists(Result) then
    Result := '';
end;

// Records a goz or an earlier gozler in Folder, unless it is this install, a
// Cargo build output such as target\release, which holds programs someone
// compiled rather than installed, or a folder already recorded. Daemon is
// the program its service runs, or ''.
procedure AddFound(const Folder: String; const IsGoz: Boolean; const Daemon: String);
var
  I: Integer;
begin
  if (Folder = '') or SameFolder(Folder, WizardDirValue) or
    DirExists(Folder + '\.fingerprint') then exit;
  for I := 0 to GetArrayLength(Found) - 1 do
    if SameFolder(Found[I].Folder, Folder) then exit;
  I := GetArrayLength(Found);
  SetArrayLength(Found, I + 1);
  Found[I].Folder := NormalFolder(Folder);
  Found[I].IsGoz := IsGoz;
  Found[I].Daemon := Daemon;
  Found[I].Item := -1;
end;

// Records the goz or gozler that a folder on the PATH in RootKey holds. Only
// the folder itself is looked at, never its subfolders. An entry built from
// an environment variable names a system folder, not a hand install.
procedure FindOnPath(const RootKey: Integer; const SubKey: String);
var
  Path, Entry: String;
  Semicolon: Integer;
begin
  if not RegQueryStringValue(RootKey, SubKey, 'Path', Path) then exit;
  Path := Path + ';';
  while Path <> '' do
  begin
    Semicolon := Pos(';', Path);
    Entry := NormalFolder(Copy(Path, 1, Semicolon - 1));
    Delete(Path, 1, Semicolon);
    if (Entry <> '') and (Pos('%', Entry) = 0) then
    begin
      if FileExists(Entry + '\gozler.exe') then
        AddFound(Entry, False, '')
      else if FileExists(Entry + '\goz.exe') then
        AddFound(Entry, True, '');
    end;
  end;
end;

// Buffer receives the address of the text. Pascal Script has no pointer
// type, so it is a pointer-sized integer, which only lstrlenW, lstrcpynW and
// WTSFreeMemory read.
function WTSQuerySessionInformation(Server: THandle; SessionId: DWORD; InfoClass: Integer;
  var Buffer: NativeInt; var Bytes: DWORD): BOOL;
  external 'WTSQuerySessionInformationW@wtsapi32.dll stdcall';
procedure WTSFreeMemory(Memory: NativeInt);
  external 'WTSFreeMemory@wtsapi32.dll stdcall';
function lstrlenW(Text: NativeInt): Integer;
  external 'lstrlenW@kernel32.dll stdcall';
function lstrcpynW(Target: String; Source: NativeInt; MaxLength: Integer): NativeInt;
  external 'lstrcpynW@kernel32.dll stdcall';

// The InfoClass text of the Windows session that runs the setup, or '' when
// Windows gives none.
function SessionText(const InfoClass: Integer): String;
var
  Buffer: NativeInt;
  Bytes: DWORD;
  Count: Integer;
begin
  Result := '';
  if not WTSQuerySessionInformation(0, WTS_CURRENT_SESSION, InfoClass, Buffer, Bytes) then
    exit;
  Count := lstrlenW(Buffer);
  if Count > 0 then
  begin
    SetLength(Result, Count);
    lstrcpynW(Result, Buffer, Count + 1);
  end;
  WTSFreeMemory(Buffer);
end;

// The user signed in to the session that runs the setup, as DOMAIN\name, or
// '' when nobody is, as in session 0. It is the session's user, not the
// account that approved the elevation, so an install that another
// administrator approved names the person who started it.
function SessionUser: String;
var
  Name, Domain: String;
begin
  Result := '';
  Name := SessionText(WTSUserName);
  if Name = '' then exit;
  Domain := SessionText(WTSDomainName);
  if Domain = '' then
    Result := Name
  else
    Result := Domain + '\' + Name;
end;

procedure CreateVisibilityChoice;
var
  Left, Width: Integer;
begin
  if WizardSilent or Upgrade or (SignedInUser = '') then exit;
  Left := WizardForm.ReadyMemo.Left;
  Width := WizardForm.ReadyMemo.Width;
  VisibilityHeading := SetupText(CustomMessage('SeeEveryFileCaption'), Left, VisibilityTop, Width);
  VisibilityHeading.Font.Style := [fsBold];
  VisibilityHeading.Visible := False;
  SeeEveryFile := TNewCheckBox.Create(WizardForm);
  SeeEveryFile.Parent := WizardForm.SelectDirPage;
  SeeEveryFile.Caption := FmtMessage(CustomMessage('SeeEveryFileItem'), [SignedInUser]);
  SeeEveryFile.SetBounds(Left, VisibilityTop + ScaleY(HeadingSpacing), Width, ScaleY(19));
  SeeEveryFile.Visible := False;
  VisibilityHint := SetupText(CustomMessage('SeeEveryFileText'), Left + ScaleX(18),
    SeeEveryFile.Top + SeeEveryFile.Height, Width - ScaleX(18));
  VisibilityHint.WordWrap := True;
  VisibilityHint.Visible := False;
  VisibilityHint.AdjustHeight;
end;

// The page that offers to remove each goz and earlier gozler in Found.
procedure CreateCleanupPage(After: Integer);
var
  I: Integer;
begin
  CleanupPage := CreateInputOptionPage(After, CustomMessage('CleanupCaption'),
    CustomMessage('CleanupDescription'), CustomMessage('CleanupText'), False, False);
  for I := 0 to GetArrayLength(Found) - 1 do
    if Found[I].IsGoz then
    begin
      Found[I].Item := CleanupPage.Add(
        FmtMessage(CustomMessage('CleanGoz'), [Found[I].Folder]));
      // Every setup starts with goz kept, because gozler runs beside it and
      // removing goz is a choice, never a step gozler needs.
      CleanupPage.Values[Found[I].Item] := False;
    end
    else
    begin
      Found[I].Item := CleanupPage.Add(
        FmtMessage(CustomMessage('CleanOldGozler'), [Found[I].Folder]));
      CleanupPage.Values[Found[I].Item] := True;
    end;
end;

procedure InitializeWizard;
var
  Daemon: String;
begin
  Upgrade := RegKeyExists(HKLM64, 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#AppId}_is1');
  SignedInUser := SessionUser;
#ifdef WhatsNewFile
  if not WizardSilent and Upgrade then
  begin
    WhatsNewButton := TNewButton.Create(WizardForm);
    WhatsNewButton.Parent := WizardForm;
    WhatsNewButton.SetBounds(ScaleX(12), WizardForm.NextButton.Top,
      ScaleX(92), WizardForm.NextButton.Height);
    WhatsNewButton.Anchors := [akLeft, akBottom];
    WhatsNewButton.Caption := CustomMessage('WhatsNew');
    WhatsNewButton.OnClick := @ShowWhatsNew;
    WhatsNewButton.Visible := False;
  end;
#endif
  if not WizardSilent then
    CreateSetupOptions;
  // Services first, so a folder that both a service and a PATH name keeps its
  // service.
  Daemon := ServiceProgram('goz');
  if Daemon <> '' then
    AddFound(ExtractFileDir(Daemon), True, Daemon);
  Daemon := ServiceProgram('{#ServiceName}');
  if Daemon <> '' then
    AddFound(ExtractFileDir(Daemon), False, Daemon);
  FindOnPath(HKLM, EnvironmentKey);
  FindOnPath(HKCU, 'Environment');
  if GetArrayLength(Found) > 0 then
    CreateCleanupPage(wpWelcome);
  CreateVisibilityChoice;
  if not WizardSilent and Upgrade then
    SettingsLoaded := ReadSetupChoices;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if not WizardSilent and not WizardShown then
  begin
    WizardShown := True;
    BringToFrontAndRestore;
  end;
  if (CurPageID = wpSelectDir) and (DiskWarning <> nil) then
  begin
    if Upgrade then
    begin
      WizardForm.PageNameLabel.Caption := CustomMessage('UpdateTitle');
      WizardForm.PageDescriptionLabel.Caption := CustomMessage('UpdateDescription');
    end
    else
    begin
      WizardForm.PageNameLabel.Caption := CustomMessage('InstallTitle');
      WizardForm.PageDescriptionLabel.Caption := CustomMessage('InstallDescription');
    end;
    WizardForm.NextButton.Caption := SetupMessage(msgButtonInstall);
    SetupChanged(nil);
    SpaceTick(0, 0, 0, 0);
  end;
#ifdef WhatsNewFile
  if WhatsNewButton <> nil then
    WhatsNewButton.Visible := (CurPageID = wpSelectDir) or (CurPageID = wpFinished);
#endif
end;

// Whether the person chose to remove Found[I]. A silent install removes
// nothing, so a script never removes a program by accident.
function CleanUp(const I: Integer): Boolean;
begin
  Result := False;
  if WizardSilent or SameFolder(Found[I].Folder, WizardDirValue) then exit;
  Result := CleanupPage.Values[Found[I].Item];
end;

// Removes a hand install from Folder: its folder from both PATHs, because a
// hand install may use either, then the files its zip holds, then the folder
// when nothing else is left in it.
procedure RemoveHandInstall(const Folder, Client, Daemon: String);
begin
  RemoveFolderFromPath(HKLM, EnvironmentKey, Folder);
  RemoveFolderFromPath(HKCU, 'Environment', Folder);
  DeleteFile(Folder + '\' + Client);
  DeleteFile(Folder + '\' + Daemon);
  DeleteFile(Folder + '\LICENSE');
  DeleteFile(Folder + '\README.md');
  RemoveDir(Folder);
end;

// Removes the goz in Found[I]. When its service runs from there, goz's own
// uninstall stops the service and waits for it first, and its logs go too.
procedure RemoveGoz(const I: Integer);
var
  Code: Integer;
begin
  if Found[I].Daemon <> '' then
  begin
    if not Exec(Found[I].Daemon, 'uninstall', '', SW_HIDE, ewWaitUntilTerminated, Code)
      or (Code <> 0) then
    begin
      SuppressibleMsgBox(FmtMessage(CustomMessage('GozFailed'), [IntToStr(Code),
        Found[I].Folder]), mbError, MB_OK, IDOK);
      exit;
    end;
    DelTree(ExpandConstant('{commonappdata}\goz'), True, True, True);
  end;
  RemoveHandInstall(Found[I].Folder, 'goz.exe', 'gozd.exe');
end;

// Gives an existing Folder the case of its name in Folder, such as Gozler for
// the gozler folder an earlier setup made. Windows keeps a folder's name when
// files are written into it, so only a rename changes it. A folder in use
// keeps its old name, which works the same.
procedure MatchFolderCase(const Folder: String);
var
  FindRec: TFindRec;
  Actual: String;
begin
  if not FindFirst(Folder, FindRec) then exit;
  Actual := FindRec.Name;
  FindClose(FindRec);
  if (Actual <> ExtractFileName(Folder)) and
    not RenameFile(ExtractFilePath(Folder) + Actual, Folder) then
    Log(Format('Could not rename %s%s to %s', [ExtractFilePath(Folder), Actual, Folder]));
end;

function RegisterWindowMessage(Name: String): Cardinal;
  external 'RegisterWindowMessageW@user32.dll stdcall';
function SendMessageTimeout(Window: HWND; Msg: Cardinal; WParam, LParam: Longint;
  Flags, Timeout: Cardinal; var Answer: Longint): Longint;
  external 'SendMessageTimeoutW@user32.dll stdcall';
function GetWindowThreadProcessId(Window: HWND; var ProcessId: Cardinal): Cardinal;
  external 'GetWindowThreadProcessId@user32.dll stdcall';
function AllowSetForegroundWindow(ProcessId: Cardinal): BOOL;
  external 'AllowSetForegroundWindow@user32.dll stdcall';

// A running window locks gozler-ui.exe and does not exit when the Restart
// Manager asks it to. So this asks it to exit as File > Exit does, the
// request a start of another build sends (DESIGN.md: Window lifetime): it
// exits at once, or asks the person first while file operations run. True
// when no window runs any more.
function CloseWindow: Boolean;
var
  Window: HWND;
  ProcessId: Cardinal;
  Answer: Longint;
  Waited: Integer;
begin
  Window := FindWindowByClassName('{#WindowClass}');
  if Window <> 0 then
  begin
    // Lets the window and its Exit question come in front of the setup.
    GetWindowThreadProcessId(Window, ProcessId);
    AllowSetForegroundWindow(ProcessId);
    // No timeout while the window answers other messages, so the person can
    // take their time over the Exit question.
    if (SendMessageTimeout(Window, RegisterWindowMessage('{#ExitMessage}'), 0, 0,
      SMTO_ABORTIFHUNG or SMTO_NOTIMEOUTIFNOTHUNG, 5000, Answer) <> 0) and
      (Answer = {#ExitCloses}) then
    begin
      // The process holds the mutex until it has joined its threads, which
      // can take longer than its window.
      Waited := 0;
      while CheckForMutexes('{#InstanceMutex}') and (Waited < 10000) do
      begin
        Sleep(100);
        Waited := Waited + 100;
      end;
    end;
  end;
  Result := not CheckForMutexes('{#InstanceMutex}');
end;

function RemoveUserSettings: String;
var
  Code: Integer;
begin
  Result := '';
  if not Exec(ExpandConstant('{app}\gozler-ui.exe'), '--remove-settings', '', SW_HIDE,
    ewWaitUntilTerminated, Code) or
    (Code <> 0) then
  begin
    Log(Format('Removing user settings failed: code=%d', [Code]));
    Result := CustomMessage('ResetSettingsFailed');
    if Code <> 1 then Result := Result + #13#10#13#10 + SysErrorMessage(Code);
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  // Gozler closes when the person selects Install, not when Setup starts. A
  // failure here keeps Back, so the person can exit Gozler and try again.
  if not CloseWindow then
  begin
    Result := CustomMessage('WindowStays');
    exit;
  end;
  // The running daemon locks gozlerd.exe, and "gozlerd install" does not
  // restart a service that already runs, so stop it before the files change.
  // net.exe waits for the stop; its error when nothing runs is expected.
  Exec(ExpandConstant('{sys}\net.exe'), 'stop {#ServiceName}', '', SW_HIDE,
    ewWaitUntilTerminated, Code);
  Log(Format('net stop {#ServiceName}: code=%d', [Code]));
  // The stopped service no longer holds its log open.
  MatchFolderCase(ExpandConstant('{app}'));
  MatchFolderCase(ExpandConstant('{#LogFolder}'));
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Parameters, Error: String;
  Code, I: Integer;
begin
  if CurStep <> ssPostInstall then exit;
  if not Upgrade and not WizardSilent then
  begin
    Error := RemoveUserSettings;
    if Error <> '' then
      SuppressibleMsgBox(Error, mbError, MB_OK, IDOK);
    if RegKeyExists(HKLM64, SettingsKey) then
      if not RegDeleteKeyIncludingSubkeys(HKLM64, SettingsKey) then
        SuppressibleMsgBox(CustomMessage('ResetSharedFailed'), mbError, MB_OK, IDOK);
  end;
  if not AddAppToPath then
    SuppressibleMsgBox(CustomMessage('CommandLineFailed'), mbError, MB_OK, IDOK);
  WizardForm.StatusLabel.Caption := CustomMessage('StartService');
  // "install" registers the service, or points an existing one at this
  // gozlerd.exe, and starts it. With --see-every-file it first stores the
  // visibility choice, so the first search follows it.
  Parameters := 'install';
  if (SeeEveryFile <> nil) and not RegValueExists(HKLM64, SettingsKey, SeeEveryFileValue) then
    if SeeEveryFile.Checked then
      Parameters := Parameters + ' --see-every-file "' + SignedInUser + '"'
    else
      Parameters := Parameters + ' --see-every-file none';
  if not Exec(ExpandConstant('{app}\gozlerd.exe'), Parameters, '', SW_HIDE,
    ewWaitUntilTerminated, Code) or (Code <> 0) then
  begin
    Log(Format('Installing service failed: code=%d', [Code]));
    SuppressibleMsgBox(CustomMessage('ServiceFailed'), mbError, MB_OK, IDOK);
    exit;
  end;
  if not SaveSetupChoices then
    SuppressibleMsgBox(CustomMessage('SetupSettingsFailed'), mbError, MB_OK, IDOK);
  WizardForm.StatusLabel.Caption := CustomMessage('CleaningUp');
  // The gozler service now runs this setup's gozlerd.exe, so an earlier
  // gozler is unused.
  for I := 0 to GetArrayLength(Found) - 1 do
    if CleanUp(I) then
      if Found[I].IsGoz then
        RemoveGoz(I)
      else
        RemoveHandInstall(Found[I].Folder, 'gozler.exe', 'gozlerd.exe');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Error: String;
begin
  // After the person confirms the uninstall. Abort ends it without a
  // message.
  if CurUninstallStep = usAppMutexCheck then
  begin
    while not CloseWindow do
      if SuppressibleMsgBox(SetupMessage(msgUninstallAppRunningError), mbError,
        MB_OKCANCEL, IDCANCEL) <> IDOK then
        Abort;
    if SessionUser <> '' then
    begin
      Error := RemoveUserSettings;
      if Error <> '' then
        SuppressibleMsgBox(Error, mbError, MB_OK, IDOK);
    end;
  end;
  if CurUninstallStep = usUninstall then
    RemoveFolderFromPath(HKLM, EnvironmentKey, ExpandConstant('{app}'));
  if CurUninstallStep = usPostUninstall then
    RegDeleteKeyIncludingSubkeys(HKLM64, SettingsKey);
end;
