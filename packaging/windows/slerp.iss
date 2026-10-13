; Slerp's Windows installer, for Inno Setup 6 (https://jrsoftware.org/isinfo.php).
;
; It installs for the current user only, without administrator rights, into
; %LOCALAPPDATA%\Programs\Slerp, runs `slerp.exe register` to set up
; Explorer's thumbnails and "Open with" entry, and adds the viewer to the
; Start menu.  Uninstalling runs `slerp.exe unregister`, which also puts
; back any thumbnail handler Slerp replaced.  Both run silently with
; /VERYSILENT /SUPPRESSMSGBOXES /NORESTART, as winget runs them.
;
; Build it on Windows from the files in tools/build-windows.sh's zip:
;
;   iscc /DSource=C:\path\to\Slerp /OC:\output\folder packaging\windows\slerp.iss
;
; The version comes from slerp.exe's version information, unless
; /DAppVersion=... gives one.
;
; Explorer keeps slerp_shell.dll loaded once it has shown a thumbnail, and
; so do the thumbnail helper and the viewer (slerp.exe).  Windows can't
; overwrite or delete a loaded file, but it can rename one, and a per-user
; installer can't have files replaced at the next restart.  So installing
; renames programs that are in use to <name>.<n>.old before copying the new
; ones in, and uninstalling moves them into %TEMP%, on the same drive, so
; that the folder goes away.  Leftovers that nothing holds any more are
; deleted at the next install.  Explorer goes on using the old DLL until it
; lets go of it; nothing here restarts Explorer.

#ifndef Source
  #define Source "..\..\local\dist\Slerp"
#endif
#ifndef AppVersion
  #define AppVersion GetStringFileInfo(Source + "\slerp.exe", "ProductVersion")
#endif
#if AppVersion == ""
  #error slerp.exe has no version information; build it with tools/build-windows.sh
#endif

[Setup]
; The AppId ties versions together for upgrades and must never change.
AppId={{2434AC7F-0AC1-4A2E-8035-70EBCE4ED931}
AppName=Slerp
AppVersion={#AppVersion}
AppVerName=Slerp {#AppVersion}
AppPublisher=The Slerp authors
AppPublisherURL=https://github.com/eric-frost/slerp
AppSupportURL=https://github.com/eric-frost/slerp/issues
AppUpdatesURL=https://github.com/eric-frost/slerp/releases
VersionInfoVersion={#AppVersion}
VersionInfoDescription=Slerp Setup
VersionInfoProductName=Slerp
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\Slerp
DisableDirPage=yes
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
; Files in use are moved aside in [Code] below, so the Restart Manager
; must not offer to close Explorer for them.
CloseApplications=no
RestartApplications=no
SetupIconFile=..\..\data\windows\slerp.ico
UninstallDisplayIcon={app}\slerp.exe
UninstallDisplayName=Slerp
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
OutputDir=..\..\local\dist
OutputBaseFilename=Slerp-Setup-windows-x64
SetupLogging=yes

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"

[Messages]
FinishedLabel=Setup has finished installing [name].  Explorer now shows thumbnails of 3D models, textures and game files, and offers Slerp under "Open with".  Explorer windows that are already open may need a refresh (F5).  The viewer is in the Start menu.

[Files]
Source: "{#Source}\slerp.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\slerpw.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\slerp_shell.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\README.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Source}\THIRD-PARTY.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
; The viewer starts in the user's own folder rather than the installation's,
; so a running viewer doesn't keep that folder from being removed.
Name: "{autoprograms}\Slerp"; Filename: "{app}\slerpw.exe"; WorkingDir: "{%USERPROFILE}"; Comment: "View 3D models, textures and game files"

[UninstallDelete]
; The thumbnail handler's diagnostic log, when someone turned it on.
Type: files; Name: "{app}\shell.log"

[Code]
var
  { Whether a program of the version being replaced was in use, so that
    Explorer may still be using the old thumbnail handler. }
  OldStillLoaded: Boolean;

{ The files that running programs keep open: Explorer and the thumbnail
  helper load the DLL, and the helper and the viewer run slerp.exe. }
function Programs: TArrayOfString;
begin
  SetArrayLength(Result, 3);
  Result[0] := 'slerp.exe';
  Result[1] := 'slerpw.exe';
  Result[2] := 'slerp_shell.dll';
end;

{ Deletes the files matching Pattern in Dir that no program holds any more. }
procedure DeleteUnused(const Dir, Pattern: String);
var
  Found: TFindRec;
begin
  if FindFirst(AddBackslash(Dir) + Pattern, Found) then
  try
    repeat
      if DeleteFile(AddBackslash(Dir) + Found.Name) then
        Log('Deleted the leftover ' + Found.Name);
    until not FindNext(Found);
  finally
    FindClose(Found);
  end;
end;

{ Gets Path out of the way: deletes it, or when a running program holds it,
  renames it to Prefix + <n> + '.old', the first such name not taken.
  Returns False when it is still there. }
function MoveAside(const Path, Prefix: String): Boolean;
var
  N: Integer;
begin
  Result := True;
  if not FileExists(Path) or DeleteFile(Path) then
    exit;
  for N := 1 to 1000 do
    if not FileExists(Prefix + IntToStr(N) + '.old') then
    begin
      Result := RenameFile(Path, Prefix + IntToStr(N) + '.old');
      if Result then
        Log('Moved ' + Path + ', which is in use, to ' + Prefix + IntToStr(N) + '.old');
      exit;
    end;
  Result := False;
end;

{ Before copying files: the old programs are moved aside where they are in
  use, so the new ones can be copied in without closing anything. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Dir: String;
  Names: TArrayOfString;
  I: Integer;
begin
  Result := '';
  Dir := ExpandConstant('{app}');
  DeleteUnused(Dir, '*.old');
  DeleteUnused(GetTempDir, 'slerp-*.old');
  Names := Programs;
  for I := 0 to GetArrayLength(Names) - 1 do
  begin
    if FileExists(Dir + '\' + Names[I]) and not DeleteFile(Dir + '\' + Names[I]) then
    begin
      if not MoveAside(Dir + '\' + Names[I], Dir + '\' + Names[I] + '.') then
      begin
        Result := 'Setup can''t replace ' + Dir + '\' + Names[I] + ', which is in use.  ' +
          'Close Slerp, or sign out and back in, and run Setup again.';
        exit;
      end;
      if Names[I] = 'slerp_shell.dll' then
        OldStillLoaded := True;
    end;
  end;
end;

{ After copying files: registers the thumbnail handler and "Open with" entry
  for this user. }
procedure CurStepChanged(CurStep: TSetupStep);
var
  Code: Integer;
begin
  if CurStep <> ssPostInstall then
    exit;
  if not Exec(ExpandConstant('{app}\slerp.exe'), 'register', '', SW_HIDE, ewWaitUntilTerminated, Code) or (Code <> 0) then
    SuppressibleMsgBox('Slerp couldn''t set up its Explorer thumbnails (code ' + IntToStr(Code) + ').  ' +
      'Run "slerp.exe register" in a terminal in ' + ExpandConstant('{app}') + ' to see why.',
      mbError, MB_OK, IDOK)
  else
    Log('slerp.exe register succeeded');
end;

{ The finished page says so when Explorer still had the old thumbnail
  handler loaded.  The label is sized for the standard text, so it grows,
  and the line below it moves down. }
procedure CurPageChanged(CurPageID: Integer);
begin
  if (CurPageID = wpFinished) and OldStillLoaded then
  begin
    WizardForm.FinishedLabel.Caption := WizardForm.FinishedLabel.Caption + #13#10#13#10 +
      'Explorer may go on using the previous version''s thumbnails until you sign out and back in.';
    WizardForm.FinishedLabel.AdjustHeight;
    WizardForm.ClickFinishLabel.Top := WizardForm.FinishedLabel.Top + WizardForm.FinishedLabel.Height + ScaleY(12);
  end;
end;

{ Uninstalling: unregisters, then moves what is still in use into %TEMP%, so
  the folder can be removed.  Where %TEMP% is on another drive, the files
  stay in the folder under another name until the next install. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Dir: String;
  Names: TArrayOfString;
  Found: TFindRec;
  I, Code: Integer;
begin
  Dir := ExpandConstant('{app}');
  if CurUninstallStep = usUninstall then
  begin
    if FileExists(Dir + '\slerp.exe') then
    begin
      if Exec(Dir + '\slerp.exe', 'unregister', '', SW_HIDE, ewWaitUntilTerminated, Code) and (Code = 0) then
        Log('slerp.exe unregister succeeded')
      else
        Log('slerp.exe unregister failed with code ' + IntToStr(Code));
    end;
    Names := Programs;
    for I := 0 to GetArrayLength(Names) - 1 do
      if not MoveAside(Dir + '\' + Names[I], GetTempDir + 'slerp-' + Names[I] + '.') then
        MoveAside(Dir + '\' + Names[I], Dir + '\' + Names[I] + '.');
    if FindFirst(Dir + '\*.old', Found) then
    try
      repeat
        MoveAside(Dir + '\' + Found.Name, GetTempDir + 'slerp-' + Found.Name + '.');
      until not FindNext(Found);
    finally
      FindClose(Found);
    end;
  end;
  if CurUninstallStep = usPostUninstall then
    RemoveDir(Dir);
end;
