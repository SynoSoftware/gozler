#if Ver < EncodeVer(7, 1, 0)
  #error Inno Setup 7.1 or later is required.
#endif
#ifndef AppVersion
  #error Run installer\Build.ps1, which supplies the version from Cargo.toml.
#endif

#define AppName "Gozler"
#define AppId "gozler"
#define DefaultShortcut "Win+S"
; The first version whose gozler-ui.exe accepts --read-setup-settings.
#define SettingsReader "0.6.2"

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
UsePreviousSetupType=no
AlwaysShowComponentsList=no
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
WizardSizePercent=120,120
OutputDir=..\target\installer
OutputBaseFilename=gozler-{#AppVersion}-x64-Setup
Compression=lzma2/max
SolidCompression=yes
ChangesEnvironment=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Messages]
DiskSpaceMBLabel=[mb] MB required.
WizardSelectComponents=Choose settings
SelectComponentsDesc=Which settings do you want on?
SelectComponentsLabel2=Standard warns about heavy SSD writes, starts when you sign in, keeps running after you close the window and makes {#DefaultShortcut} show Search instead of Windows Search. Select Advanced to change these.
UninstallAppRunningError={#AppName} is running. To close it, choose File > Exit in its window, or Exit on the menu of its icon in the notification area.%n%nThen click OK to continue, or Cancel to stop uninstalling.

[CustomMessages]
WhatsNew=What's new
CloseNotes=Close
StandardInstall=Standard install (recommended)
AdvancedInstall=Advanced install
StartAtSignIn=Start when I sign in to Windows
SearchShortcut=Show Search with %1%n%1 no longer opens Windows Search, and Win+Shift+S stops working.%nChoose other shortcuts in File > Settings > Keyboard.
KeepShortcut=Another shortcut is set. It stays unless you select %1.
KeepRunning=Keep running in the notification area after I close the window
DiskWarning=Warn me about heavy weekly SSD writes%nWarnings come only while the app runs.
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
SeeEveryFileItem=Let %1 see every file name%nShows names in folders you can't open, without access to their files.
WindowStays={#AppName} is still running, so Setup can't replace it.%n%nExit {#AppName}, then try again.

[Types]
Name: "standard"; Description: "{cm:StandardInstall}"
Name: "advanced"; Description: "{cm:AdvancedInstall}"; Flags: iscustom

[Components]
Name: "alerts"; Description: "{cm:DiskWarning}"; Types: standard; Flags: disablenouninstallwarning
Name: "signin"; Description: "{cm:StartAtSignIn}"; Types: standard; Flags: disablenouninstallwarning
Name: "running"; Description: "{cm:KeepRunning}"; Types: standard; Flags: disablenouninstallwarning
Name: "shortcut"; Description: "{cm:SearchShortcut,{#DefaultShortcut}}"; Types: standard; Flags: disablenouninstallwarning
Name: "visibility"; Description: "{code:VisibilityCaption}"; Flags: disablenouninstallwarning; Check: OfferVisibility

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
  DefaultShortcut = '{#DefaultShortcut}';
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
  SignedInUser: String;
  // The choices that ReadSetupChoices read, which an update keeps unless the
  // person changes them.
  SavedSignIn, SavedRunning, SavedAlerts, SavedShortcut: Boolean;
  Upgrade: Boolean;
  SettingsLoaded: Boolean;
  WizardShown: Boolean;
#ifdef WhatsNewFile
  WhatsNewButton: TNewButton;
#endif

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

function ReadSetupChoices: Boolean;
var
  Code: Integer;
  Executable, Version: String;
  Installed, Minimum: Int64;
begin
  Result := False;
  // {app} is set only once the folder page is passed, so this reads the
  // folder that page shows.
  Executable := AddBackslash(WizardDirValue) + 'gozler-ui.exe';
  if not GetVersionNumbersString(Executable, Version) or not StrToVersion(Version, Installed) or
    not StrToVersion('{#SettingsReader}', Minimum) then exit;
  if ComparePackedVersion(Installed, Minimum) < 0 then exit;
  // The app answers in its exit code: 256 plus one bit for each saved choice
  // (setup_answer in gozler-ui's startup module). Any other code is a
  // failure. It writes no file, so no folder permission can block it.
  try
    if not ExecAsOriginalUser(Executable, '--read-setup-settings ' + DefaultShortcut, '',
      SW_HIDE, ewWaitUntilTerminated, Code) then exit;
  except
    Log(GetExceptionMessage);
    exit;
  end;
  if (Code < 256) or (Code > 511) then
  begin
    Log(Format('Reading setup choices failed: code=%d', [Code]));
    exit;
  end;
  SavedSignIn := (Code and 1) <> 0;
  SavedRunning := (Code and 2) <> 0;
  SavedAlerts := (Code and 4) <> 0;
  SavedShortcut := (Code and 8) <> 0;
  WizardSelectComponents('!alerts,!signin,!running,!shortcut');
  if SavedAlerts then WizardSelectComponents('alerts');
  if SavedSignIn then WizardSelectComponents('signin');
  if SavedRunning then WizardSelectComponents('running');
  if SavedShortcut then WizardSelectComponents('shortcut');
  WizardForm.TypesCombo.ItemIndex := 1;
  // Setting ItemIndex skips Inno's TypesComboChange, which shows the list for
  // Advanced, and an update always shows the saved choices.
  WizardForm.ComponentsList.Visible := True;
  WizardForm.ComponentsDiskSpaceLabel.Visible := True;
  // Another shortcut is saved.
  if (Code and 16) <> 0 then
  begin
    // The list holds the components in their [Components] order, so item 3
    // is shortcut; only visibility, which comes last, can be left out.
    WizardForm.ComponentsList.ItemCaption[3] :=
      FmtMessage(CustomMessage('SearchShortcut'), [DefaultShortcut]) + #13#10 +
      FmtMessage(CustomMessage('KeepShortcut'), [DefaultShortcut]);
  end;
  Result := True;
end;

function SetupValue(const Component: String; const Saved: Boolean): String;
var
  Selected: Boolean;
begin
  Selected := WizardIsComponentSelected(Component);
  if Upgrade and (Selected = Saved) then
    Result := 'keep'
  else
    Result := IntToStr(Ord(Selected));
end;

function SaveSetupChoices: Boolean;
var
  Code: Integer;
  Shortcut, Parameters: String;
begin
  Result := True;
  if WizardSilent or (Upgrade and not SettingsLoaded) then exit;
  Shortcut := SetupValue('shortcut', SavedShortcut);
  if Shortcut = '1' then
    Shortcut := DefaultShortcut
  else if Shortcut = '0' then
    Shortcut := '-';
  Parameters := Format('--setup-settings %s %s %s %s', [SetupValue('signin', SavedSignIn),
    SetupValue('running', SavedRunning), SetupValue('alerts', SavedAlerts), Shortcut]);
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

function InitializeSetup: Boolean;
begin
  Upgrade := RegKeyExists(HKLM64, 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#AppId}_is1');
  SignedInUser := SessionUser;
  Result := True;
end;

function OfferVisibility: Boolean;
begin
  Result := not WizardSilent and not Upgrade and (SignedInUser <> '');
end;

function VisibilityCaption(Param: String): String;
begin
  Result := FmtMessage(CustomMessage('SeeEveryFileItem'), [SignedInUser]);
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
  WizardForm.DirBrowseButton.Enabled := not Upgrade;
  WizardForm.DirEdit.ReadOnly := Upgrade;
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
  if not WizardSilent and Upgrade then
    SettingsLoaded := ReadSetupChoices;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := (PageID = wpSelectComponents) and Upgrade and not SettingsLoaded;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if not WizardSilent and not WizardShown then
  begin
    WizardShown := True;
    BringToFrontAndRestore;
  end;
#ifdef WhatsNewFile
  if WhatsNewButton <> nil then
    WhatsNewButton.Visible := (CurPageID = wpSelectDir) or
      (CurPageID = wpSelectComponents) or (CurPageID = wpFinished);
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
  if OfferVisibility and not RegValueExists(HKLM64, SettingsKey, SeeEveryFileValue) then
    if WizardIsComponentSelected('visibility') then
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
