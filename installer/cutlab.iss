; Inno Setup 6.3+ script for CutLab. Build with build-installer.ps1 (or open in the Inno Setup IDE after: cargo build --release)
#define AppName "CutLab"
#define AppVersion "0.1.0"

[Setup]
AppId={{5B869FF4-527D-40C1-BF89-DFEF6A2F83BE}
AppName={#AppName}
AppVersion={#AppVersion}
AppPublisher=CutLab
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
OutputDir=..\dist
OutputBaseFilename=CutLab-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
WizardStyle=modern
UninstallDisplayIcon={app}\cutlab.exe

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "..\target\release\cutlab.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\cutlab.exe"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\cutlab.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\cutlab.exe"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent
