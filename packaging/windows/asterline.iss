#ifndef MyAppVersion
  #error MyAppVersion must be provided with /DMyAppVersion=<version>
#endif

#ifndef SourceDir
  #error SourceDir must be provided with /DSourceDir=<absolute path>
#endif

[Setup]
AppId={{155DFC95-76E4-4677-ABC3-BF4CC7D9C589}
AppName=Asterline
AppVersion={#MyAppVersion}
AppPublisher=Asterline contributors
AppPublisherURL=https://github.com/song0705/Asterline
AppSupportURL=https://github.com/song0705/Asterline/issues
AppUpdatesURL=https://github.com/song0705/Asterline/releases/latest
DefaultDirName={localappdata}\Programs\Asterline
DefaultGroupName=Asterline
DisableProgramGroupPage=yes
LicenseFile=..\..\LICENSE
OutputDir=..\..\dist
OutputBaseFilename=asterline-{#MyAppVersion}-x86_64-windows-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
ChangesEnvironment=yes
CloseApplications=yes
RestartApplications=no
UninstallDisplayName=Asterline
UninstallDisplayIcon={app}\asterline.exe

[Files]
Source: "{#SourceDir}\asterline.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\ast.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "installer-managed"; DestDir: "{app}"; DestName: ".asterline-installer-managed"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Code]
const
  UserEnvironmentKey = 'Environment';

function NormalizePathEntry(Value: String): String;
begin
  Result := Trim(Value);
  if (Length(Result) >= 2) and (Result[1] = '"') and
     (Result[Length(Result)] = '"') then
  begin
    Delete(Result, Length(Result), 1);
    Delete(Result, 1, 1);
  end;
  StringChangeEx(Result, '/', '\', True);
  while (Length(Result) > 3) and (Result[Length(Result)] = '\') do
    Delete(Result, Length(Result), 1);
end;

function TakePathEntry(var Value: String): String;
var
  Separator: Integer;
begin
  Separator := Pos(';', Value);
  if Separator = 0 then
  begin
    Result := Value;
    Value := '';
  end
  else
  begin
    Result := Copy(Value, 1, Separator - 1);
    Delete(Value, 1, Separator);
  end;
end;

function PathContains(const PathValue, Entry: String): Boolean;
var
  Remaining: String;
begin
  Result := False;
  Remaining := PathValue;
  while Remaining <> '' do
  begin
    if NormalizePathEntry(TakePathEntry(Remaining)) =
       NormalizePathEntry(Entry) then
    begin
      Result := True;
      Exit;
    end;
  end;
end;

procedure AddToUserPath;
var
  AppDir: String;
  PathValue: String;
begin
  AppDir := ExpandConstant('{app}');
  if not RegQueryStringValue(HKEY_CURRENT_USER, UserEnvironmentKey, 'Path',
     PathValue) then
    PathValue := '';

  if PathContains(PathValue, AppDir) then
    Exit;

  if PathValue = '' then
    PathValue := AppDir
  else if PathValue[Length(PathValue)] = ';' then
    PathValue := PathValue + AppDir
  else
    PathValue := PathValue + ';' + AppDir;

  if not RegWriteExpandStringValue(HKEY_CURRENT_USER, UserEnvironmentKey,
     'Path', PathValue) then
    RaiseException('Could not add Asterline to the user Path.');
end;

procedure RemoveFromUserPath;
var
  AppDir: String;
  Entry: String;
  PathValue: String;
  Remaining: String;
  Updated: String;
begin
  AppDir := ExpandConstant('{app}');
  if not RegQueryStringValue(HKEY_CURRENT_USER, UserEnvironmentKey, 'Path',
     PathValue) then
    Exit;

  Remaining := PathValue;
  Updated := '';
  while Remaining <> '' do
  begin
    Entry := TakePathEntry(Remaining);
    if (Entry <> '') and
       (NormalizePathEntry(Entry) <> NormalizePathEntry(AppDir)) then
    begin
      if Updated <> '' then
        Updated := Updated + ';';
      Updated := Updated + Entry;
    end;
  end;

  if Updated <> PathValue then
    RegWriteExpandStringValue(HKEY_CURRENT_USER, UserEnvironmentKey, 'Path',
      Updated);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
  WaitPid: Integer;
begin
  Result := '';
  WaitPid := StrToIntDef(ExpandConstant('{param:WAITPID|0}'), 0);
  if WaitPid > 0 then
    Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
      '-NoLogo -NoProfile -NonInteractive -Command "Wait-Process -Id ' +
      IntToStr(WaitPid) + ' -ErrorAction SilentlyContinue"', '', SW_HIDE,
      ewWaitUntilTerminated, ResultCode);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    AddToUserPath;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RemoveFromUserPath;
end;
