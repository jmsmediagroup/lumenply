; Inno Setup script for the Windows installer (built by .github/workflows/release.yml).
; Expects the app in dist\win\ (Lumenply.exe and any DLLs beside it) and the
; version in the LUMENPLY_VERSION environment variable.

#define AppVersion GetEnv("LUMENPLY_VERSION")

[Setup]
AppId={{6B3E2A41-7C1D-4F0E-9A8B-4C5D2E7F9013}
AppName=Lumenply
AppVersion={#AppVersion}
AppVerName=Lumenply {#AppVersion}
AppPublisher=Lumenply contributors
AppPublisherURL=https://github.com/jmsmediagroup/lumenply
AppSupportURL=https://github.com/jmsmediagroup/lumenply/issues
DefaultDirName={autopf}\Lumenply
DefaultGroupName=Lumenply
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\Lumenply.exe
SetupIconFile=..\..\crates\app\assets\lumenply.ico
OutputDir=..\..\dist
OutputBaseFilename=Lumenply-Windows-Setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Install for the current user without asking for admin rights; the
; wizard offers "all users" too.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ChangesAssociations=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "..\..\dist\win\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\Lumenply"; Filename: "{app}\Lumenply.exe"
Name: "{autodesktop}\Lumenply"; Filename: "{app}\Lumenply.exe"; Tasks: desktopicon

[Registry]
; .lumen projects open in Lumenply; Photoshop, OpenRaster and common images
; get Lumenply under "Open with".
Root: HKA; Subkey: "Software\Classes\.lumen"; ValueType: string; ValueName: ""; ValueData: "Lumenply.Project"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\Lumenply.Project"; ValueType: string; ValueName: ""; ValueData: "Lumenply project"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Lumenply.Project\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\Lumenply.exe,0"
Root: HKA; Subkey: "Software\Classes\Lumenply.Project\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\Lumenply.exe"" ""%1"""
Root: HKA; Subkey: "Software\Classes\Lumenply.Image"; ValueType: string; ValueName: ""; ValueData: "Image"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Lumenply.Image\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\Lumenply.exe,0"
Root: HKA; Subkey: "Software\Classes\Lumenply.Image\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\Lumenply.exe"" ""%1"""
Root: HKA; Subkey: "Software\Classes\.psd\OpenWithProgids"; ValueType: string; ValueName: "Lumenply.Image"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.psb\OpenWithProgids"; ValueType: string; ValueName: "Lumenply.Image"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.ora\OpenWithProgids"; ValueType: string; ValueName: "Lumenply.Image"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.png\OpenWithProgids"; ValueType: string; ValueName: "Lumenply.Image"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.jpg\OpenWithProgids"; ValueType: string; ValueName: "Lumenply.Image"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.jpeg\OpenWithProgids"; ValueType: string; ValueName: "Lumenply.Image"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.tif\OpenWithProgids"; ValueType: string; ValueName: "Lumenply.Image"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.tiff\OpenWithProgids"; ValueType: string; ValueName: "Lumenply.Image"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.webp\OpenWithProgids"; ValueType: string; ValueName: "Lumenply.Image"; ValueData: ""; Flags: uninsdeletevalue

[Run]
Filename: "{app}\Lumenply.exe"; Description: "{cm:LaunchProgram,Lumenply}"; Flags: nowait postinstall skipifsilent
