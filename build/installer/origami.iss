; Windows installer for the Origami app. build.ps1 feeds it ORIGAMI_VERSION, ORIGAMI_NUM_VERSION,
; ORIGAMI_ARCH, ORIGAMI_STAGE and ORIGAMI_OUT through the environment; /DSign signs.

#define AppName "Origami"
#define AppVersion GetEnv("ORIGAMI_VERSION")
#define NumVersion GetEnv("ORIGAMI_NUM_VERSION")
#define Arch GetEnv("ORIGAMI_ARCH")
#define Stage GetEnv("ORIGAMI_STAGE")
#define OutDir GetEnv("ORIGAMI_OUT")

[Setup]
; The same id for both architectures, so either installs over the other.
AppId={{1EC46053-31E6-486A-8AE3-C2DE12CE825F}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=James Braid
AppPublisherURL=https://github.com/jamesbraid/origami
VersionInfoVersion={#NumVersion}
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\app\Origami.exe
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
MinVersion=10.0.17763
#if Arch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#else
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#endif
SetupIconFile=..\..\apps\windows\Origami.App\Origami.ico
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
ChangesEnvironment=yes
CloseApplications=yes
RestartApplications=no
OutputDir={#OutDir}
OutputBaseFilename=origami-{#AppVersion}-windows-{#Arch}-setup
#ifdef Sign
; Signs the setup and the uninstaller it carries, with the command build.ps1 passes as /Ssigntool.
SignTool=signtool
SignedUninstaller=yes
#endif

[Messages]
UninstalledAll=%1 was removed from your computer. Your machine library was not touched.

[Tasks]
Name: addtopath; Description: "Add the origami command to PATH"

[InstallDelete]
; Only over our own install: a chosen folder like C:\Tools may hold someone else's bin.
Type: filesandordirs; Name: "{app}\app"; Check: OurInstall
Type: filesandordirs; Name: "{app}\bin"; Check: OurInstall
Type: filesandordirs; Name: "{app}\libexec"; Check: OurInstall
Type: filesandordirs; Name: "{app}\share"; Check: OurInstall

[Files]
Source: "{#Stage}\*"; DestDir: "{app}"; Flags: recursesubdirs createallsubdirs ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\app\Origami.exe"; WorkingDir: "{app}\app"

[Run]
Filename: "{app}\app\Origami.exe"; WorkingDir: "{app}\app"; Description: "Start {#AppName}"; Flags: nowait postinstall skipifsilent

[Code]
function RegOpenKeyExW(Key: Cardinal; SubKey: String; Options, Sam: Cardinal; var Handle: Cardinal): Longint;
  external 'RegOpenKeyExW@advapi32.dll stdcall';
function RegQueryValueExW(Key: Cardinal; Name: String; Reserved: Cardinal; var ValueType: Cardinal; Data, Size: Cardinal): Longint;
  external 'RegQueryValueExW@advapi32.dll stdcall';
function RegCloseKey(Key: Cardinal): Longint;
  external 'RegCloseKey@advapi32.dll stdcall';

function OurInstall: Boolean;
begin
  Result := FileExists(ExpandConstant('{app}\unins000.exe'));
end;

function EnvKey: String;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

{ The registry type of Path (1 string, 2 expandable), or 2 when it has none. }
function PathType: Cardinal;
var
  Root, Key: Cardinal;
begin
  Result := 2;
  if IsAdminInstallMode then Root := $80000002 else Root := $80000001;
  if RegOpenKeyExW(Root, EnvKey, 0, $20019, Key) = 0 then
  begin
    if RegQueryValueExW(Key, 'Path', 0, Result, 0, 0) <> 0 then
      Result := 2;
    RegCloseKey(Key);
  end;
end;

function Norm(S: String): String;
begin
  StringChangeEx(S, '%LOCALAPPDATA%', GetEnv('LOCALAPPDATA'), True);
  Result := Lowercase(Trim(S));
  while (Length(Result) > 3) and (Result[Length(Result)] = '\') do
    Delete(Result, Length(Result), 1);
end;

{ Path without any entry equal to Dir; empty segments stay. }
function WithoutDir(Path, Dir: String): String;
var
  Rest, Item: String;
  P: Integer;
  Last: Boolean;
begin
  Result := '';
  Rest := Path;
  repeat
    P := Pos(';', Rest);
    Last := P = 0;
    if Last then
      Item := Rest
    else
    begin
      Item := Copy(Rest, 1, P - 1);
      Rest := Copy(Rest, P + 1, Length(Rest));
    end;
    if (Item = '') or (Norm(Item) <> Norm(Dir)) then
    begin
      if Result <> '' then
        Result := Result + ';';
      Result := Result + Item;
    end;
  until Last;
end;

procedure SetPathEntry(Add: Boolean);
var
  Old, New, Dir: String;
  Kind: Cardinal;
begin
  Dir := ExpandConstant('{app}\bin');
  if not RegQueryStringValue(HKA, EnvKey, 'Path', Old) then
    Old := '';
  Kind := PathType;
  New := WithoutDir(Old, Dir);
  if Add then
  begin
    if (New <> '') and (New[Length(New)] <> ';') then
      New := New + ';';
    New := New + Dir;
  end;
  if New = Old then
    Exit;
  if Kind = 1 then
    RegWriteStringValue(HKA, EnvKey, 'Path', New)
  else
    RegWriteExpandStringValue(HKA, EnvKey, 'Path', New);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    SetPathEntry(WizardIsTaskSelected('addtopath'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
    SetPathEntry(False);
end;
