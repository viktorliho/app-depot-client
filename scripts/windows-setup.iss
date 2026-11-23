[Setup]
AppId=io.github.viktorliho.AppDepot
AppName=App Depot Desktop
AppVersion={#AppDepotVersion}
AppPublisher=App Depot
DefaultDirName={localappdata}\Programs\App Depot Desktop
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64os
ArchitecturesInstallIn64BitMode=x64os
MinVersion=10.0.19041
UninstallDisplayIcon={app}\app-depot-desktop.exe
SetupIconFile={#AppDepotIcon}
OutputBaseFilename=AppDepot-Desktop-Windows-x64-Setup
VersionInfoProductName=App Depot Desktop
VersionInfoProductVersion={#AppDepotNumericVersion}
VersionInfoProductTextVersion={#AppDepotVersion}
VersionInfoVersion={#AppDepotNumericVersion}
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
CloseApplications=no
AppMutex=Local\io.github.viktorliho.AppDepot.Desktop
SetupMutex=Local\io.github.viktorliho.AppDepot.Setup

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#AppDepotExecutable}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\App Depot Desktop"; Filename: "{app}\app-depot-desktop.exe"; WorkingDir: "{app}"
Name: "{userdesktop}\App Depot Desktop"; Filename: "{app}\app-depot-desktop.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\app-depot-desktop.exe"; WorkingDir: "{app}"; Flags: nowait runasoriginaluser; Check: IsDesktopUpdate

[Code]
function IsDesktopUpdate: Boolean;
begin
  Result := ExpandConstant('{param:APPDEPOTUPDATE|0}') = '1';
end;

function InitializeSetup: Boolean;
var
  Attempt: Integer;
begin
  if IsDesktopUpdate then
    for Attempt := 1 to 50 do
    begin
      if not CheckForMutexes('Local\io.github.viktorliho.AppDepot.Desktop') then
        Break;
      Sleep(100);
    end;
  { The built-in AppMutex check follows this callback. A live supervisor still blocks Setup. }
  Result := True;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := IsDesktopUpdate and (PageID = wpSelectDir);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Installed: String;
begin
  Result := '';
  if CheckForMutexes('Local\io.github.viktorliho.AppDepot.Desktop') then
  begin
    Result := 'Close App Depot and let managed applications exit before continuing.';
    Exit;
  end;
  if IsDesktopUpdate then
  begin
    if not RegQueryStringValue(HKCU64,
      'Software\Microsoft\Windows\CurrentVersion\Uninstall\io.github.viktorliho.AppDepot_is1',
      'InstallLocation', Installed) or
      (CompareText(AddBackslash(Installed), AddBackslash(ExpandConstant('{app}'))) <> 0) or
      not FileExists(ExpandConstant('{app}\app-depot-desktop.exe')) then
      Result := 'Setup could not verify the existing per-user installation. Open App Depot from Start and try again.';
  end;
end;
