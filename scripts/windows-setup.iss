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

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#AppDepotExecutable}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\App Depot Desktop"; Filename: "{app}\app-depot-desktop.exe"; WorkingDir: "{app}"
Name: "{userdesktop}\App Depot Desktop"; Filename: "{app}\app-depot-desktop.exe"; WorkingDir: "{app}"; Tasks: desktopicon
