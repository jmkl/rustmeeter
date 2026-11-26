#define MyAppExeName "RustMeeter"
#define MyAppName "rustmeeter.exe"
#define MyAppVersion "0.0.1"


[Setup]
AppName=rustmeeter
AppVersion={#MyAppVersion}
DefaultDirName={autopf}\rustmeeter
DefaultGroupName=rustmeeter
OutputBaseFilename=rustmeeter_{#MyAppVersion}_setup
Compression=lzma2
SolidCompression=yes
ChangesEnvironment=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=crates\rustmeeter\assets\app.ico
[Files]
Source: "target\x86_64-pc-windows-gnullvm\release\rustmeeter.exe"; DestDir: "{app}"; Flags: ignoreversion
[Icons]
Name: "{group}\rustmeeter"; Filename: "{app}\rustmeeter.exe"
Name: "{autodesktop}\rustmeeter"; Filename: "{app}\rustmeeter.exe"; Tasks: desktopicon
[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked
Name: "startup"; Description: "Run {#MyAppName} automatically when Windows starts"; GroupDescription: "Additional options:"

[Registry]
; Runs for the current user only (no admin rights required)
Root: HKCU; Subkey: "SOFTWARE\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#MyAppName}"; ValueData: """{app}\{#MyAppExeName}"""; Flags: uninsdeletevalue; Tasks: startup
