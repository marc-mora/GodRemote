//! Ventana principal (`src/ui/mod.rs`): barra de herramientas, árbol de
//! equipos, pestañas, barra de estado, paleta de órdenes y diálogos.
//! (Véase `mod.rs`.)

pub(super) const T: super::Table = &[
    // --- Avisos generales ---
    (
        "No se pudieron leer las sesiones: {error}",
        "Could not read the sessions: {error}",
    ),
    (
        "No se pudieron guardar las sesiones: {error}",
        "Could not save the sessions: {error}",
    ),
    (
        "No se pudo guardar en el baúl: {error}",
        "Could not save to the vault: {error}",
    ),
    (
        "Credencial guardada en el baúl",
        "Credential saved in the vault",
    ),
    (
        "No se pudo registrar la sesión: {error}",
        "Could not log the session: {error}",
    ),
    (
        "No se pudo abrir la shell local «{command}»: {error}",
        "Could not open the local shell “{command}”: {error}",
    ),
    ("{name} (copia)", "{name} (copy)"),
    (
        "No se puede mover una carpeta dentro de sí misma",
        "A folder cannot be moved into itself",
    ),
    (
        "Contraseña del proxy borrada del baúl",
        "Proxy password deleted from the vault",
    ),
    (
        "Contraseña del proxy guardada en el baúl",
        "Proxy password saved in the vault",
    ),
    // --- Túneles del gestor y reconexión ---
    ("El túnel ya no existe", "The tunnel no longer exists"),
    (
        "El equipo SSH del túnel ya no existe",
        "The tunnel's SSH host no longer exists",
    ),
    ("Túnel «{name}»: {error}", "Tunnel “{name}”: {error}"),
    (
        "Conexión perdida. Reintento {n} de {max} en {secs} s…",
        "Connection lost. Retry {n} of {max} in {secs} s…",
    ),
    (
        "Reconectando… intento {n} de {max}",
        "Reconnecting… attempt {n} of {max}",
    ),
    (
        "[Reconectando… intento {attempt} de {max}]",
        "[Reconnecting… attempt {attempt} of {max}]",
    ),
    ("[Reconectando…]", "[Reconnecting…]"),
    // --- RDP ---
    (
        "Esta edición no incluye el cliente RDP integrado",
        "This edition does not include the built-in RDP client",
    ),
    (
        "Los saltos solo funcionan con el cliente RDP integrado: ábrelo integrado o quita los \
         saltos de la sesión",
        "Jump hosts only work with the built-in RDP client: open it built-in or remove the jump \
         hosts from the session",
    ),
    (
        "Cliente RDP lanzado para {name}",
        "RDP client launched for {name}",
    ),
    (
        "RDP «{name}» terminó con error. {detail}",
        "RDP “{name}” ended with an error. {detail}",
    ),
    (
        "Sesión RDP «{name}» finalizada",
        "RDP session “{name}” ended",
    ),
    (
        "El cliente RDP integrado está desactivado en Ajustes",
        "The built-in RDP client is turned off in Settings",
    ),
    ("(usuario)", "(user)"),
    ("Cliente: {client}", "Client: {client}"),
    // --- Barra de herramientas ---
    ("➕ Sesión", "➕ Session"),
    (
        "Nueva sesión SSH / SFTP / RDP",
        "New SSH / SFTP / RDP session",
    ),
    ("📁 Carpeta", "📁 Folder"),
    ("Nueva carpeta de equipos", "New folder of hosts"),
    ("💻 Terminal local", "💻 Local terminal"),
    (
        "Abrir un terminal local con otra shell",
        "Open a local terminal with another shell",
    ),
    ("🔑 Claves", "🔑 Keys"),
    ("Gestionar claves SSH", "Manage SSH keys"),
    ("🔀 Túneles", "🔀 Tunnels"),
    ("🔀 Túneles ({n})", "🔀 Tunnels ({n})"),
    (
        "Túneles SSH que se inician y se detienen sin abrir un terminal",
        "SSH tunnels that are started and stopped without opening a terminal",
    ),
    ("🎨 Temas", "🎨 Themes"),
    (
        "Cambiar o personalizar el aspecto",
        "Change or customize the appearance",
    ),
    ("⚙ Ajustes", "⚙ Settings"),
    (
        "Cliente RDP, barra de información del servidor y otras preferencias",
        "RDP client, server info bar and other preferences",
    ),
    ("🔐 Baúl", "🔐 Vault"),
    ("Bloquear", "Lock"),
    ("Ver lo guardado…", "View stored secrets…"),
    ("Cambiar contraseña maestra…", "Change master password…"),
    ("Desbloquear…", "Unlock…"),
    ("Crear baúl…", "Create vault…"),
    // --- Snippets ---
    ("⚡ Snippets", "⚡ Snippets"),
    ("Aún no hay snippets", "No snippets yet"),
    ("Abre un terminal conectado", "Open a connected terminal"),
    ("➕ Nuevo snippet…", "➕ New snippet…"),
    ("⏹ Detener y guardar…", "⏹ Stop and save…"),
    (
        "⏺ Grabar tecleando en el terminal activo",
        "⏺ Record by typing in the active terminal",
    ),
    (
        "Gestionar snippets y atajos…",
        "Manage snippets and shortcuts…",
    ),
    (
        "Textos y órdenes que se envían al terminal desde aquí o con un atajo",
        "Texts and commands sent to the terminal from here or with a shortcut",
    ),
    ("Guardar snippet", "Save snippet"),
    (
        "Nombre de lo que acabas de grabar:",
        "Name for what you have just recorded:",
    ),
    ("Descartar", "Discard"),
    (
        "No se pudo guardar el snippet: {error}",
        "Could not save the snippet: {error}",
    ),
    (
        "Abre un terminal conectado para lanzar el snippet",
        "Open a connected terminal to run the snippet",
    ),
    ("Lanzar «{name}»", "Run “{name}”"),
    ("Grabando snippet", "Recording snippet"),
    (
        "No se tecleó nada: no se ha guardado ningún snippet",
        "Nothing was typed: no snippet has been saved",
    ),
    // --- Barra de estado ---
    (
        "🔓 Baúl abierto · {n} secreto(s)",
        "🔓 Vault open · {n} secret(s)",
    ),
    ("🔒 Baúl bloqueado", "🔒 Vault locked"),
    (
        "⬆ Versión {version} disponible",
        "⬆ Version {version} available",
    ),
    (
        "Abre la página de descargas en el navegador",
        "Opens the downloads page in the browser",
    ),
    // --- Árbol de equipos ---
    ("🔍 Buscar equipo…", "🔍 Search hosts…"),
    ("Aún no hay equipos.", "No hosts yet."),
    (
        "Crea una sesión con «➕ Sesión».",
        "Create a session with “➕ Session”.",
    ),
    ("Sin resultados.", "No results."),
    ("➕ Nueva sesión", "➕ New session"),
    ("📁 Nueva carpeta", "📁 New folder"),
    ("Mover a", "Move to"),
    ("Conectar", "Connect"),
    ("📂 Abrir SFTP", "📂 Open SFTP"),
    ("💻 Abrir terminal SSH", "💻 Open SSH terminal"),
    (
        "🖥 Abrir con el cliente externo",
        "🖥 Open with the external client",
    ),
    (
        "🖥 Abrir integrado en una pestaña",
        "🖥 Open built-in, in a tab",
    ),
    ("✏ Editar…", "✏ Edit…"),
    ("Duplicar", "Duplicate"),
    ("(vacía)", "(empty)"),
    ("➕ Nueva sesión aquí", "➕ New session here"),
    ("📁 Nueva subcarpeta", "📁 New subfolder"),
    ("✏ Editar carpeta…", "✏ Edit folder…"),
    (
        "▶ Ejecutar una orden en sus equipos…",
        "▶ Run a command on its hosts…",
    ),
    (
        "Lanza la misma orden en todos sus equipos SSH y recoge lo que contesta cada uno",
        "Runs the same command on all its SSH hosts and collects what each one answers",
    ),
    (
        "Vigila a la vez todos sus equipos SSH: conexión, CPU, memoria, disco y alertas",
        "Watches all its SSH hosts at once: connection, CPU, memory, disk and alerts",
    ),
    (
        "Se eliminará la carpeta «{name}» con las {n} sesión(es) que contiene y sus credenciales \
         guardadas.",
        "The folder “{name}” will be deleted, together with the {n} session(s) it contains and \
         their saved credentials.",
    ),
    (
        "Se eliminará la sesión «{name}» y sus credenciales guardadas.",
        "The session “{name}” and its saved credentials will be deleted.",
    ),
    (
        "Esta acción no se puede deshacer.",
        "This action cannot be undone.",
    ),
    // --- Vista dividida ---
    ("⊞ Dividida", "⊞ Split view"),
    ("⊞ Dividir", "⊞ Split"),
    (
        "Ver varios terminales a la vez",
        "View several terminals at once",
    ),
    (
        "Los terminales se reparten en mosaico. Un clic en uno le da el teclado; doble clic en su \
         título lo deja a pantalla completa.",
        "The terminals are laid out as tiles. A click on one gives it the keyboard; a \
         double-click on its title shows it alone, full size.",
    ),
    ("Todos los terminales abiertos", "All open terminals"),
    ("Solo los que elija:", "Only the ones I choose:"),
    ("Columnas", "Columns"),
    ("Auto", "Auto"),
    (
        "Ver varios terminales a la vez, en mosaico",
        "View several terminals at once, as tiles",
    ),
    ("Ver solo este terminal", "View only this terminal"),
    ("Reconectar", "Reconnect"),
    // --- Pestañas ---
    (
        "Recibe lo que se escriba con la difusión activa",
        "Receives what is typed while broadcast is on",
    ),
    (
        "Ha sonado la campana en este terminal",
        "The bell has rung in this terminal",
    ),
    ("Hay salida nueva sin ver", "There is new unseen output"),
    (
        "Arrastra para cambiarla de sitio; doble clic para renombrarla",
        "Drag to move it; double-click to rename it",
    ),
    ("Mover al principio", "Move to the start"),
    ("Mover al final", "Move to the end"),
    ("Cerrar las demás", "Close the others"),
    ("Cerrar las de la derecha", "Close the ones to the right"),
    ("Cerrar pestaña", "Close tab"),
    ("¿Cerrar «{title}»?", "Close “{title}”?"),
    (
        "Tiene una sesión conectada o transferencias a medias: lo que esté en marcha se \
         interrumpirá.",
        "It has a connected session or transfers in progress: whatever is running will be \
         interrupted.",
    ),
    ("¿Cerrar {total} pestañas?", "Close {total} tabs?"),
    (
        "{n} de ellas tienen sesiones conectadas o transferencias a medias; lo que esté en marcha \
         se interrumpirá:",
        "{n} of them have connected sessions or transfers in progress; whatever is running will \
         be interrupted:",
    ),
    ("  … y {n} más", "  … and {n} more"),
    // --- Salir ---
    ("¿Salir de GodRemote?", "Quit GodRemote?"),
    (
        "1 pestaña con una sesión o transferencias en curso",
        "1 tab with a session or transfers in progress",
    ),
    (
        "{n} pestañas con sesiones o transferencias en curso",
        "{n} tabs with sessions or transfers in progress",
    ),
    ("1 túnel en marcha", "1 running tunnel"),
    ("{n} túneles en marcha", "{n} running tunnels"),
    ("{sessions} y {tunnels}", "{sessions} and {tunnels}"),
    ("Hay {what}: se cerrarán.", "{what} will be closed."),
    ("Salir", "Quit"),
    // --- Paleta de órdenes ---
    ("Pestaña", "Tab"),
    ("Acción", "Action"),
    ("Snippet", "Snippet"),
    ("Túnel", "Tunnel"),
    ("Abrir un terminal local", "Open a local terminal"),
    ("Ctrl+Mayús+T", "Ctrl+Shift+T"),
    ("Cerrar la pestaña", "Close the tab"),
    ("Ctrl+Mayús+W", "Ctrl+Shift+W"),
    ("Ctrl+Mayús+F", "Ctrl+Shift+F"),
    ("Desactivar la difusión", "Turn off broadcast"),
    (
        "Activar la difusión: escribir en varios terminales",
        "Turn on broadcast: type in several terminals",
    ),
    ("Quitar la vista dividida", "Turn off split view"),
    (
        "Vista dividida: varios terminales a la vez",
        "Split view: several terminals at once",
    ),
    ("Nuevo equipo…", "New host…"),
    ("Nueva carpeta…", "New folder…"),
    (
        "Ejecutar una orden en los equipos de una carpeta…",
        "Run a command on the hosts in a folder…",
    ),
    (
        "Panel de estado de los equipos de una carpeta",
        "Status dashboard for the hosts in a folder",
    ),
    (
        "CPU, memoria, disco y alertas",
        "CPU, memory, disk and alerts",
    ),
    ("gestor de túneles", "tunnel manager"),
    ("Snippets y atajos", "Snippets and shortcuts"),
    (
        "avisar o contestar cuando aparezca un texto",
        "notify or answer when a text appears",
    ),
    (
        "Terminar la grabación de esta sesión",
        "End the recording of this session",
    ),
    ("Grabar esta sesión", "Record this session"),
    ("Reproducir una grabación…", "Play a recording…"),
    ("las ejecutadas en cada equipo", "the ones run on each host"),
    ("aspecto, colores y letra", "appearance, colors and font"),
    ("preferencias", "preferences"),
    (
        "Importar equipos o hacer una copia de seguridad",
        "Import hosts or make a backup",
    ),
    (
        "Sincronización entre mis equipos",
        "Sync between my computers",
    ),
    ("carpeta cifrada, conflictos", "encrypted folder, conflicts"),
    (
        "Carpetas compartidas con compañeros",
        "Folders shared with teammates",
    ),
    (
        "publicar una carpeta de equipos o seguir la de otro",
        "publish a folder of hosts or follow someone else's",
    ),
    ("Servidor X: abrir su pestaña", "X server: open its tab"),
    ("aplicaciones gráficas", "graphical applications"),
    ("Bloquear el baúl", "Lock the vault"),
    ("contraseñas guardadas", "saved passwords"),
    (
        "Ver lo guardado en el baúl",
        "View what is stored in the vault",
    ),
    ("Desbloquear el baúl…", "Unlock the vault…"),
    ("Detener {name}", "Stop {name}"),
    ("Iniciar {name}", "Start {name}"),
    // --- Bienvenida y conexión rápida ---
    (
        "Gestor de conexiones remotas · SSH · SFTP · RDP · VNC · Telnet · Serie",
        "Remote connection manager · SSH · SFTP · RDP · VNC · Telnet · Serial",
    ),
    ("Conexión rápida", "Quick connect"),
    ("usuario@host[:puerto]", "user@host[:port]"),
    (
        "Doble clic en un equipo del árbol para conectar.",
        "Double-click a host in the tree to connect.",
    ),
    (
        "Clic derecho para editar, mover, duplicar o abrir SFTP.",
        "Right-click to edit, move, duplicate or open SFTP.",
    ),
    // --- Cabecera del terminal ---
    ("Conectado", "Connected"),
    ("Desconectado", "Disconnected"),
    (
        "Reconectando en {secs} s (intento {n} de {max})",
        "Reconnecting in {secs} s (attempt {n} of {max})",
    ),
    ("No se pudo reconectar", "Could not reconnect"),
    ("⟳ Reconectar", "⟳ Reconnect"),
    (
        "Explorar ficheros sobre esta misma conexión",
        "Browse files over this same connection",
    ),
    ("Activo: {what}", "Active: {what}"),
    ("Error: {what}: {error}", "Error: {what}: {error}"),
    ("Pendiente: {what}", "Pending: {what}"),
    ("Túneles {active}/{total}", "Tunnels {active}/{total}"),
    ("Letra más grande", "Larger text"),
    ("Letra más pequeña", "Smaller text"),
    (
        "Buscar en el terminal y su historial (Ctrl+Mayús+F)",
        "Search the terminal and its scrollback (Ctrl+Shift+F)",
    ),
    ("⏺ Registrando", "⏺ Logging"),
    (
        "Guardando lo que muestra este terminal en\n{ruta}\nPulsa para dejar de registrar.",
        "Saving what this terminal shows to\n{ruta}\nClick to stop logging.",
    ),
    ("⏺ Registrar", "⏺ Log"),
    (
        "Guardar en un fichero de texto lo que muestre este terminal",
        "Save what this terminal shows to a text file",
    ),
    ("Registro guardado en {ruta}", "Log saved to {ruta}"),
    ("Registrando en {ruta}", "Logging to {ruta}"),
    // --- Difusión ---
    ("📡 Difusión", "📡 Broadcast"),
    (
        "📡 Escribiendo en {receivers} terminales",
        "📡 Typing in {receivers} terminals",
    ),
    (
        "📡 Difusión: no hay más terminales",
        "📡 Broadcast: no other terminals",
    ),
    (
        "📡 Difusión activa (este terminal no)",
        "📡 Broadcast on (not this terminal)",
    ),
    (
        "Escribir a la vez en varios terminales",
        "Type in several terminals at once",
    ),
    (
        "Lo que teclees, pegues o lances con un snippet en uno de ellos se envía también a los \
         demás.",
        "What you type, paste or run with a snippet in one of them is also sent to the others.",
    ),
    ("En todos los terminales abiertos", "In all open terminals"),
    ("Solo en los que elija:", "Only in the ones I choose:"),
    ("{title} (este)", "{title} (this one)"),
    (
        "{title} (este) · sin conexión",
        "{title} (this one) · not connected",
    ),
    ("{title} · sin conexión", "{title} · not connected"),
    (
        "Enviar lo que escribes a varios terminales a la vez",
        "Send what you type to several terminals at once",
    ),
    // --- Servidor X y escritorios remotos ---
    ("Servidor X", "X server"),
    ("Servidor X integrado", "Built-in X server"),
    ("sin aplicaciones", "no applications"),
    ("1 aplicación", "1 application"),
    ("{n} aplicaciones", "{n} applications"),
    ("· programas locales:", "· local programs:"),
    (
        "Copiar «export DISPLAY=…» al portapapeles",
        "Copy “export DISPLAY=…” to the clipboard",
    ),
    ("Copiado al portapapeles", "Copied to the clipboard"),
    (
        "· lanza una aplicación gráfica desde un terminal SSH con el reenvío X11 activado",
        "· launch a graphical application from an SSH terminal with X11 forwarding enabled",
    ),
    ("Ctrl+Alt+Supr", "Ctrl+Alt+Del"),
    (
        "Envía Ctrl+Alt+Supr al equipo remoto",
        "Sends Ctrl+Alt+Del to the remote host",
    ),
    (
        "Pulsa la tecla Windows en el equipo remoto",
        "Presses the Windows key on the remote host",
    ),
    // --- Baúl ---
    ("🔐 Desbloquear baúl", "🔐 Unlock vault"),
    (
        "Introduce la contraseña maestra para usar las credenciales guardadas.",
        "Enter the master password to use the saved credentials.",
    ),
    ("Desbloquear", "Unlock"),
    ("Continuar sin baúl", "Continue without the vault"),
    (
        "🔐 Crear baúl de credenciales",
        "🔐 Create credential vault",
    ),
    (
        "Elige una contraseña maestra. Con ella se cifran las contraseñas y passphrases guardadas \
         (Argon2id + XChaCha20-Poly1305).",
        "Choose a master password. It is used to encrypt the saved passwords and passphrases \
         (Argon2id + XChaCha20-Poly1305).",
    ),
    (
        "Si la olvidas, los secretos guardados no se pueden recuperar.",
        "If you forget it, the saved secrets cannot be recovered.",
    ),
    ("contraseña maestra", "master password"),
    ("Crear baúl", "Create vault"),
    (
        "La contraseña no puede estar vacía",
        "The password cannot be empty",
    ),
    ("Las contraseñas no coinciden", "The passwords do not match"),
    ("Ahora no", "Not now"),
    ("Cambiar contraseña maestra", "Change master password"),
    ("nueva contraseña maestra", "new master password"),
    ("Cambiar", "Change"),
    ("El baúl está bloqueado", "The vault is locked"),
    ("Baúl desbloqueado", "Vault unlocked"),
    (
        "Baúl bloqueado por inactividad",
        "Vault locked due to inactivity",
    ),
    ("Contraseña maestra cambiada", "Master password changed"),
    // --- Preguntas de las conexiones ---
    ("Host desconocido", "Unknown host"),
    (
        "No se puede verificar la autenticidad de {host}:{port}. Es la primera vez que te \
         conectas a este servidor.",
        "The authenticity of {host}:{port} cannot be verified. This is the first time you are \
         connecting to this server.",
    ),
    (
        "Huella del certificado TLS:",
        "TLS certificate fingerprint:",
    ),
    (
        "Huella de la clave {algorithm}:",
        "{algorithm} key fingerprint:",
    ),
    (
        "Comprueba la huella con el administrador del servidor. Si confías, se recordará y la \
         conexión se rechazará si cambia.",
        "Check the fingerprint with the server administrator. If you trust it, it will be \
         remembered and the connection will be refused if it changes.",
    ),
    (
        "Comprueba la huella con el administrador del servidor. Si confías, se guardará en \
         known_hosts y se avisará si cambia.",
        "Check the fingerprint with the server administrator. If you trust it, it will be saved \
         in known_hosts and you will be warned if it changes.",
    ),
    ("Confiar y conectar", "Trust and connect"),
    (
        "Baúl bloqueado: no se guardará.",
        "Vault locked: it will not be saved.",
    ),
    ("Aceptar", "OK"),
    // --- Ajustes: lo que añadió la sincronización (`src/ui/settings_view.rs`) ---
    (
        "Sincronización entre mis equipos…",
        "Sync between my computers…",
    ),
    (
        "Mantener la configuración al día en todos tus equipos a través de una carpeta que ya \
         repliques (Dropbox, Syncthing…), cifrada",
        "Keep the configuration up to date on all your computers through a folder you already \
         replicate (Dropbox, Syncthing…), encrypted",
    ),
    (
        "Carpetas compartidas con compañeros…",
        "Folders shared with teammates…",
    ),
    (
        "Publicar una carpeta de equipos (sin secretos) para tus compañeros, o seguir las que \
         publiquen ellos",
        "Publish a folder of hosts (without secrets) for your teammates, or follow the ones they \
         publish",
    ),
];
