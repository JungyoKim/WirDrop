; WirDrop's setup program (Inno Setup 6). It asks for administrator rights once, which WirDrop
; then never needs again: updates come through the engine service (src/update.rs).
;
;   iscc /DAppVersion=0.2.0 /DStage=<folder stage.ps1 filled> /O<output folder> WirDrop.iss
;
; Everything goes in Program Files\WirDrop (the engine service runs from there, so the folder is
; fixed). Then WirDrop.exe --setup, elevated: the engine service, the USB adapter on Windows' own
; WinUSB driver (if it's plugged in; else when it first is), and the Explorer menu. Uninstalling
; undoes those, gives the adapter back to Windows' Wi-Fi driver, and leaves the user's data
; (%LOCALAPPDATA%\WirDrop: settings, the Apple ID sign-in) in place.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef Stage
  #define Stage "..\..\target\stage"
#endif

[Setup]
AppId={{CB92A120-402F-4DA5-886C-E97B51B04397}
AppName=WirDrop
AppVersion={#AppVersion}
AppVerName=WirDrop {#AppVersion}
AppPublisher=WirDrop
AppPublisherURL=https://github.com/JungyoKim/WirDrop
AppSupportURL=https://github.com/JungyoKim/WirDrop/issues
AppUpdatesURL=https://github.com/JungyoKim/WirDrop/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={autopf}\WirDrop
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
PrivilegesRequired=admin
UsedUserAreasWarning=no
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.22000
OutputBaseFilename=WirDrop-Setup-{#AppVersion}
SetupIconFile=..\..\crates\app\res\wirdrop.ico
UninstallDisplayIcon={app}\WirDrop.exe
UninstallDisplayName=WirDrop
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ShowLanguageDialog=no
; A running WirDrop (the tray app, the engine, the Explorer menu's surrogate) holds its files.
CloseApplications=force
RestartApplications=no

[Languages]
Name: "korean"; MessagesFile: "compiler:Languages\Korean.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
korean.Autostart=Windows를 켤 때 WirDrop 실행
korean.Preparing=AirDrop 엔진과 USB 어댑터, 탐색기 메뉴를 준비하는 중...
korean.Launch=WirDrop 실행
english.Autostart=Start WirDrop with Windows
english.Preparing=Setting up the AirDrop engine, the USB adapter and the Explorer menu...
english.Launch=Start WirDrop

[Tasks]
Name: "autostart"; Description: "{cm:Autostart}"

[Files]
Source: "{#Stage}\*"; DestDir: "{app}"; Excludes: "*.old-*"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
; 끝낸 뒤 다시 켤 수 있게 시작 메뉴에 둡니다 (모든 사용자).
Name: "{autoprograms}\WirDrop"; Filename: "{app}\WirDrop.exe"; WorkingDir: "{app}"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "WirDrop"; ValueData: """{app}\WirDrop.exe"" --autostart"; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\WirDrop.exe"; Parameters: "--setup --service on --driver on --menu on"; StatusMsg: "{cm:Preparing}"; Flags: runhidden waituntilterminated
Filename: "{app}\WirDrop.exe"; Description: "{cm:Launch}"; Flags: postinstall nowait runasoriginaluser

[UninstallRun]
Filename: "{app}\WirDrop.exe"; Parameters: "--exit"; Flags: runhidden waituntilterminated; RunOnceId: "Exit"
Filename: "{app}\WirDrop.exe"; Parameters: "--setup --menu off --service off --driver off"; Flags: runhidden waituntilterminated; RunOnceId: "Teardown"

[UninstallDelete]
; Updates add and replace files the install didn't list.
Type: filesandordirs; Name: "{app}"

