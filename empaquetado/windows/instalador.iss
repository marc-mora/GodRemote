; Instalador de GodRemote para Windows (Inno Setup 6).
; Lo llama empaquetado/empaquetar.sh, que define Version, Nombre, Ejecutable,
; Identificador, Origen, Icono, Salida y Fichero.

[Setup]
AppId=GodRemote.{#Identificador}
AppName={#Nombre}
AppVersion={#Version}
AppPublisher=GodRemote
AppPublisherURL=https://github.com/marc-mora/GodRemote
AppSupportURL=https://github.com/marc-mora/GodRemote/issues
AppUpdatesURL=https://github.com/marc-mora/GodRemote/releases
; Por usuario: no pide permisos de administrador.
PrivilegesRequired=lowest
DefaultDirName={autopf}\{#Nombre}
DefaultGroupName={#Nombre}
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\{#Ejecutable}
SetupIconFile={#Icono}
OutputDir={#Salida}
OutputBaseFilename={#Fichero}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64

[Languages]
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "escritorio"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Origen}\{#Ejecutable}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Origen}\LEEME.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#Nombre}"; Filename: "{app}\{#Ejecutable}"
Name: "{autodesktop}\{#Nombre}"; Filename: "{app}\{#Ejecutable}"; Tasks: escritorio

[Run]
Filename: "{app}\{#Ejecutable}"; Description: "{cm:LaunchProgram,{#Nombre}}"; Flags: nowait postinstall skipifsilent
