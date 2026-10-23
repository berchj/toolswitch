; Windows installer, built with Inno Setup 6:
;   iscc /DVersion=0.1.0 /DBinary=path\to\toolswitch.exe packaging\windows\toolswitch.iss
; Installs for the current user only, so it needs no administrator rights.

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Binary
  #define Binary "..\..\target\release\toolswitch.exe"
#endif

[Setup]
AppId={{6F1C2B7E-4A0D-4E8B-9C5A-2D3F7B1E9A41}
AppName=toolswitch
AppVersion={#Version}
AppPublisher=berchj
AppPublisherURL=https://berchj.github.io
DefaultDirName={localappdata}\Programs\toolswitch
DisableProgramGroupPage=yes
DisableDirPage=yes
PrivilegesRequired=lowest
OutputDir=..\..
OutputBaseFilename=toolswitch-{#Version}-windows-x86_64-setup
SetupIconFile=..\..\assets\icon.ico
UninstallDisplayIcon={app}\toolswitch.ico
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=force

[Files]
Source: "{#Binary}"; DestDir: "{app}"; DestName: "toolswitch.exe"; Flags: ignoreversion
Source: "..\..\assets\icon.ico"; DestDir: "{app}"; DestName: "toolswitch.ico"

[Icons]
Name: "{userprograms}\toolswitch"; Filename: "{app}\toolswitch.exe"; IconFilename: "{app}\toolswitch.ico"

[Run]
Filename: "{app}\toolswitch.exe"; Description: "Open toolswitch"; Flags: nowait postinstall

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/F /IM toolswitch.exe"; Flags: runhidden; RunOnceId: "StopApp"

[Registry]
; The app adds itself here (start at login); this only takes it out on uninstall.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "toolswitch"; Flags: uninsdeletevalue dontcreatekey
