//! Textos de lo que no es interfaz: mensajes de estado y de error de las
//! conexiones, etiquetas de los tipos del modelo, avisos del panel de estado,
//! lo que el terminal pinta por su cuenta… Van agrupados por el fichero donde
//! aparecen por primera vez.

pub(super) const T: super::Table = &[
    // --- src/agent.rs ---
    (
        "Automático (OpenSSH y, si no, Pageant)",
        "Automatic (OpenSSH, otherwise Pageant)",
    ),
    ("Agente de OpenSSH", "OpenSSH agent"),
    ("el agente de OpenSSH", "the OpenSSH agent"),
    ("{agent} no contesta", "{agent} is not answering"),
    (
        "no hay agente SSH (variable SSH_AUTH_SOCK sin definir)",
        "there is no SSH agent (SSH_AUTH_SOCK is not set)",
    ),
    (
        "no se pudo conectar con el agente SSH (¿SSH_AUTH_SOCK?)",
        "could not connect to the SSH agent (SSH_AUTH_SOCK?)",
    ),
    (
        "no se pudo conectar con el agente de OpenSSH (¿servicio ssh-agent?)",
        "could not connect to the OpenSSH agent (ssh-agent service?)",
    ),
    (
        "no hay agente SSH en este sistema",
        "there is no SSH agent on this system",
    ),
    (
        "no se pudo conectar con Pageant (¿está en marcha?)",
        "could not connect to Pageant (is it running?)",
    ),
    (
        "Pageant solo existe en Windows",
        "Pageant only exists on Windows",
    ),
    (
        "{agent} no devolvió sus claves: {e}",
        "{agent} did not return its keys: {e}",
    ),
    (
        "no se pudo preparar la consulta al agente",
        "could not prepare the agent query",
    ),
    // --- src/backdrop.rs ---
    ("Ninguno", "None"),
    ("Texto", "Text"),
    ("Arte ASCII", "ASCII art"),
    ("Imagen", "Image"),
    ("Centro", "Center"),
    ("Arriba", "Top"),
    ("Abajo", "Bottom"),
    ("Izquierda", "Left"),
    ("Derecha", "Right"),
    ("Arriba a la izquierda", "Top left"),
    ("Arriba a la derecha", "Top right"),
    ("Abajo a la izquierda", "Bottom left"),
    ("Abajo a la derecha", "Bottom right"),
    ("Tamaño original", "Original size"),
    ("Ajustar (sin recortar)", "Fit (no cropping)"),
    ("Rellenar (recortando)", "Fill (cropping)"),
    ("Estirar", "Stretch"),
    ("Mosaico (repetir)", "Tile (repeat)"),
    ("No se pudo leer la imagen", "Could not read the image"),
    (
        "La imagen no se pudo interpretar (se admiten PNG, JPEG, GIF, BMP y WebP): {e}",
        "The image could not be decoded (PNG, JPEG, GIF, BMP and WebP are supported): {e}",
    ),
    ("No se pudo leer {path}", "Could not read {path}"),
    ("{path} no es un fichero", "{path} is not a file"),
    (
        "{path} es demasiado grande (máximo {max} MB)",
        "{path} is too large (maximum {max} MB)",
    ),
    (
        "No se pudo preparar la imagen",
        "Could not prepare the image",
    ),
    (
        "La imagen sigue siendo demasiado grande después de reducirla",
        "The image is still too large after shrinking it",
    ),
    (
        "{path} es demasiado grande para un fondo de texto",
        "{path} is too large for a text background",
    ),
    (
        "La imagen del fondo no está en base64",
        "The background image is not base64",
    ),
    // --- src/backup.rs ---
    (
        "«{path}» está dañado en la copia",
        "“{path}” is damaged in the backup",
    ),
    (
        "«{path}» no tiene contenido en la copia",
        "“{path}” has no content in the backup",
    ),
    (
        "La copia contiene una ruta no admitida: {path}",
        "The backup contains a path that is not allowed: {path}",
    ),
    ("No se pudo escribir {path}", "Could not write {path}"),
    (
        "{path} es demasiado grande para la copia",
        "{path} is too large for the backup",
    ),
    (
        "Todavía no hay nada que guardar",
        "There is nothing to back up yet",
    ),
    (
        "{path} es demasiado grande para ser una copia",
        "{path} is too large to be a backup",
    ),
    (
        "{path} no es una copia de seguridad de GodRemote",
        "{path} is not a GodRemote backup",
    ),
    (
        "La copia es de una versión más reciente de GodRemote",
        "The backup comes from a newer version of GodRemote",
    ),
    (
        "La copia no contiene ficheros válidos",
        "The backup contains no valid files",
    ),
    ("los equipos", "hosts"),
    ("el baúl cifrado", "encrypted vault"),
    ("los ajustes", "settings"),
    ("los snippets", "snippets"),
    ("los túneles", "tunnels"),
    ("{n} tema(s)", "{n} theme(s)"),
    ("{n} fichero(s) de claves", "{n} key file(s)"),
    (
        "Copia del {date}: {contents}",
        "Backup from {date}: {contents}",
    ),
    // --- src/cast.rs ---
    (
        "La grabación {path} ha llegado al tamaño máximo ({max} MB) y se ha detenido",
        "The recording {path} reached the maximum size ({max} MB) and has stopped",
    ),
    (
        "No se pudo escribir en {path}: {e}",
        "Could not write to {path}: {e}",
    ),
    ("No se pudo abrir {path}", "Could not open {path}"),
    (
        "La grabación ocupa {size} MB; se abren como mucho {max} MB",
        "The recording is {size} MB; at most {max} MB can be opened",
    ),
    ("El fichero está vacío", "The file is empty"),
    (
        "No es una grabación asciicast: la cabecera es demasiado larga",
        "Not an asciicast recording: the header is too long",
    ),
    (
        "No es una grabación asciicast: falta la cabecera",
        "Not an asciicast recording: the header is missing",
    ),
    (
        "Es una grabación asciicast de la versión 1, que no se admite",
        "This is an asciicast version 1 recording, which is not supported",
    ),
    (
        "Versión de asciicast desconocida: {other}",
        "Unknown asciicast version: {other}",
    ),
    (
        "No es una grabación asciicast: la cabecera no dice su versión",
        "Not an asciicast recording: the header does not state its version",
    ),
    (
        "La grabación tiene demasiados eventos (más de {max})",
        "The recording has too many events (more than {max})",
    ),
    // --- src/cmd_history.rs ---
    ("ahora", "just now"),
    ("hace {n} min", "{n} min ago"),
    ("hace {n} h", "{n} h ago"),
    ("ayer", "yesterday"),
    ("hace {n} días", "{n} days ago"),
    // --- src/dashboard.rs ---
    ("Uso de CPU", "CPU usage"),
    ("Carga media", "Load average"),
    ("inalcanzable", "unreachable"),
    ("sin acceso", "no access"),
    ("carga", "load"),
    ("memoria", "memory"),
    ("disco", "disk"),
    (" (umbral superado)", " (threshold exceeded)"),
    (
        " ({n} muestras seguidas por encima del umbral)",
        " ({n} consecutive samples above the threshold)",
    ),
    ("CPU al {percent} %{streak}", "CPU at {percent}%{streak}"),
    (
        "carga {load} con {cores} núcleo(s){streak}",
        "load {load} with {cores} core(s){streak}",
    ),
    (
        "memoria al {percent} % ({used} de {total})",
        "memory at {percent}% ({used} of {total})",
    ),
    (
        "disco {mount} al {percent} % (quedan {free})",
        "disk {mount} at {percent}% ({free} left)",
    ),
    ("vuelve a responder", "responding again"),
    ("se ha podido entrar", "login succeeded"),
    ("no responde: {why}", "not responding: {why}"),
    ("no se vigila: {why}", "not monitored: {why}"),
    ("en cola", "queued"),
    ("conectando", "connecting"),
    ("sin datos", "no data"),
    ("en línea", "online"),
    ("sin conexión", "offline"),
    ("falta credencial", "credential missing"),
    ("acceso rechazado", "access rejected"),
    (
        "falta una contraseña o passphrase que no está en el baúl",
        "a password or passphrase that is not in the vault is missing",
    ),
    ("Alerta: {name} — {detail}", "Alert: {name} — {detail}"),
    ("Resuelto: {name} — {detail}", "Resolved: {name} — {detail}"),
    ("y {n} más", "and {n} more"),
    ("{n} alertas nuevas: {list}", "{n} new alerts: {list}"),
    (
        "{n} alertas resueltas: {list}",
        "{n} alerts resolved: {list}",
    ),
    (
        "hay que confirmar la clave del servidor",
        "the server's key has to be confirmed",
    ),
    ("Esperando tu respuesta…", "Waiting for your answer…"),
    (
        "el equipo no terminó de conectar a tiempo",
        "the host did not finish connecting in time",
    ),
    ("Conectando…", "Connecting…"),
    (
        "el servidor dejó de contestar",
        "the server stopped answering",
    ),
    (
        "se perdió la conexión con el servidor",
        "the connection to the server was lost",
    ),
    ("el panel se cerró", "the dashboard was closed"),
    // --- src/fleet.rs ---
    ("código {code}", "exit code {code}"),
    ("terminado", "finished"),
    ("error: {error}", "error: {error}"),
    ("cancelado", "cancelled"),
    ("sin terminar", "not finished"),
    ("[salida recortada]", "[output truncated]"),
    ("no terminó en {secs} s", "did not finish within {secs} s"),
    (
        "el servidor no aceptó ejecutar la orden",
        "the server refused to run the command",
    ),
    // --- src/fonts.rs ---
    ("No se pudo leer {path}: {e}", "Could not read {path}: {e}"),
    (
        "{path} no es un tipo de letra válido",
        "{path} is not a valid font",
    ),
    (
        "El tipo de letra «{choice}» no está instalado en este equipo",
        "The font “{choice}” is not installed on this computer",
    ),
    // --- src/highlight.rs ---
    ("Rojo", "Red"),
    ("Amarillo", "Yellow"),
    ("Verde", "Green"),
    ("Azul", "Blue"),
    ("Magenta", "Magenta"),
    ("Cian", "Cyan"),
    ("Propio", "Custom"),
    ("color desconocido: {text}", "unknown color: {text}"),
    // --- src/import.rs ---
    ("PuTTY (fichero .reg)", "PuTTY (.reg file)"),
    (
        "MobaXterm (.mxtsessions o MobaXterm.ini)",
        "MobaXterm (.mxtsessions or MobaXterm.ini)",
    ),
    ("Importado de OpenSSH", "Imported from OpenSSH"),
    ("Importado de PuTTY", "Imported from PuTTY"),
    ("Importado de MobaXterm", "Imported from MobaXterm"),
    ("{path} es demasiado grande", "{path} is too large"),
    (
        "No se ha encontrado ningún equipo en {path} (¿es un fichero de {source}?)",
        "No hosts were found in {path} (is it a {source} file?)",
    ),
    (
        "Los bloques «Match» no se importan",
        "“Match” blocks are not imported",
    ),
    (
        "No se han seguido los ficheros incluidos con «Include {value}»",
        "Files included with “Include {value}” were not followed",
    ),
    (
        "«{name}» no indica usuario: complétalo antes de conectar",
        "“{name}” has no user: fill it in before connecting",
    ),
    (
        "«{name}» usa el protocolo «{other}», que GodRemote no tiene",
        "“{name}” uses the “{other}” protocol, which GodRemote does not have",
    ),
    (
        "«{name}» pasa por el proxy {proxy}: configúralo como salto si es SSH",
        "“{name}” goes through the proxy {proxy}: set it up as a jump host if it is SSH",
    ),
    (
        "«{name}» pasa por la pasarela SSH {gateway}: añádela como salto",
        "“{name}” goes through the SSH gateway {gateway}: add it as a jump host",
    ),
    ("puerto serie", "serial port"),
    ("explorador de archivos", "file browser"),
    ("shell local", "local shell"),
    ("navegador", "browser"),
    (
        "«{name}» es de tipo {what}: no se ha importado",
        "“{name}” is of type {what}: not imported",
    ),
    (
        "«{name}» es de un tipo desconocido: no se ha importado",
        "“{name}” is of an unknown type: not imported",
    ),
    (
        "Las sesiones de PuTTY solo se pueden leer directamente en Windows",
        "PuTTY sessions can only be read directly on Windows",
    ),
    (
        "No se pudo ejecutar «reg export»",
        "Could not run “reg export”",
    ),
    (
        "No hay sesiones de PuTTY guardadas en este equipo",
        "There are no saved PuTTY sessions on this computer",
    ),
    // --- src/keys.rs ---
    ("una fecha fuera de rango", "a date out of range"),
    ("no vale hasta el {date}", "not valid until {date}"),
    ("caducó el {date}", "expired on {date}"),
    ("no caduca", "does not expire"),
    ("válido hasta el {date}", "valid until {date}"),
    ("cualquiera", "anyone"),
    (
        "{path} no es un certificado OpenSSH válido: {e}",
        "{path} is not a valid OpenSSH certificate: {e}",
    ),
    (
        "{path} no es un certificado OpenSSH",
        "{path} is not an OpenSSH certificate",
    ),
    ("Ed25519 (recomendado)", "Ed25519 (recommended)"),
    ("desconocido", "unknown"),
    ("Nombre de fichero no válido", "Invalid file name"),
    ("Ya existe {path}", "{path} already exists"),
    (
        "no se pudo generar la clave: {e}",
        "could not generate the key: {e}",
    ),
    ("escribiendo {path}", "writing {path}"),
    (
        "la clave generada no se pudo releer",
        "the generated key could not be read back",
    ),
    (
        "{path} no parece una clave privada SSH",
        "{path} does not look like an SSH private key",
    ),
    ("ruta sin nombre de fichero", "path without a file name"),
    (
        "la clave importada no se pudo releer",
        "the imported key could not be read back",
    ),
    ("borrando {path}", "deleting {path}"),
    // --- src/macros.rs ---
    ("abrir la paleta de órdenes", "opening the command palette"),
    ("pasar a la pestaña siguiente", "switching to the next tab"),
    (
        "pasar a la pestaña anterior",
        "switching to the previous tab",
    ),
    ("abrir un terminal local", "opening a local terminal"),
    ("cerrar la pestaña", "closing the tab"),
    (
        "Falta la tecla: Ctrl, Alt y Mayús solos no son un atajo",
        "A key is missing: Ctrl, Alt and Shift alone are not a shortcut",
    ),
    (
        "Usa Ctrl, Alt o una tecla de función (F1–F12): una tecla sola ya no se podría escribir en el terminal",
        "Use Ctrl, Alt or a function key (F1–F12): a single key could no longer be typed in the terminal",
    ),
    (
        "{shortcut} está reservado para copiar, pegar y cortar",
        "{shortcut} is reserved for copy, paste and cut",
    ),
    (
        "{shortcut} está reservado para {purpose}",
        "{shortcut} is reserved for {purpose}",
    ),
    (
        "{shortcut} está reservado para buscar en el terminal",
        "{shortcut} is reserved for searching the terminal",
    ),
    ("Mayús", "Shift"),
    // --- src/main.rs ---
    (
        "GodRemote no ha podido abrir su ventana: este equipo no ofrece ningún modo de dibujo que sirva.\n\n{errors}",
        "GodRemote could not open its window: this computer offers no usable rendering mode.\n\n{errors}",
    ),
    // --- src/model.rs ---
    ("Serie", "Serial"),
    ("Contraseña", "Password"),
    ("Clave privada", "Private key"),
    ("Agente SSH", "SSH agent"),
    ("Remoto (-R)", "Remote (-R)"),
    ("Dinámico SOCKS (-D)", "Dynamic SOCKS (-D)"),
    (
        "-L {listen_host}:{listen_port} hacia {target_host}:{target_port}",
        "-L {listen_host}:{listen_port} to {target_host}:{target_port}",
    ),
    (
        "-R {listen_host}:{listen_port} hacia {target_host}:{target_port}",
        "-R {listen_host}:{listen_port} to {target_host}:{target_port}",
    ),
    (
        "Túnel: falta la dirección de escucha",
        "Tunnel: the listen address is missing",
    ),
    (
        "Túnel: el puerto de escucha no es válido",
        "Tunnel: the listen port is not valid",
    ),
    (
        "Túnel: falta el host de destino",
        "Tunnel: the target host is missing",
    ),
    (
        "Túnel: el puerto de destino no es válido",
        "Tunnel: the target port is not valid",
    ),
    ("{device} · {baud} baudios", "{device} · {baud} baud"),
    (
        "Indica el dispositivo serie (COM3, /dev/ttyUSB0…)",
        "Enter the serial device (COM3, /dev/ttyUSB0…)",
    ),
    ("La velocidad no es válida", "The baud rate is not valid"),
    (
        "Los bits de datos deben estar entre 5 y 8",
        "Data bits must be between 5 and 8",
    ),
    (
        "Los bits de parada deben ser 1 o 2",
        "Stop bits must be 1 or 2",
    ),
    ("El host no puede estar vacío", "The host cannot be empty"),
    ("El puerto no es válido", "The port is not valid"),
    (
        "El usuario no puede estar vacío",
        "The user cannot be empty",
    ),
    (
        "Indica la ruta de la clave privada",
        "Enter the path to the private key",
    ),
    (
        "Un equipo no puede saltar a través de sí mismo",
        "A host cannot jump through itself",
    ),
    (
        "Hay un salto repetido en la lista",
        "A jump host is repeated in the list",
    ),
    (
        "Los saltos forman un bucle (pasan dos veces por «{name}»)",
        "The jump hosts form a loop (they go through “{name}” twice)",
    ),
    (
        "Un salto de «{name}» ya no existe; edita la sesión",
        "A jump host of “{name}” no longer exists; edit the session",
    ),
    (
        "El salto «{name}» no es una sesión SSH",
        "The jump host “{name}” is not an SSH session",
    ),
    (
        "Demasiados saltos encadenados (máximo {max})",
        "Too many chained jump hosts (maximum {max})",
    ),
    // --- src/monitor.rs ---
    ("Nombre de la sesión", "Session name"),
    ("Nombre del servidor (hostname)", "Server name (hostname)"),
    (
        "Usuario y dirección de conexión",
        "User and connection address",
    ),
    ("Direcciones IP del servidor", "Server IP addresses"),
    ("Sistema operativo", "Operating system"),
    ("Kernel y arquitectura", "Kernel and architecture"),
    ("Tiempo encendido", "Uptime"),
    (
        "Carga media (1, 5 y 15 min)",
        "Load average (1, 5 and 15 min)",
    ),
    ("Memoria", "Memory"),
    ("Intercambio (swap)", "Swap"),
    ("Espacio libre en disco", "Free disk space"),
    ("Tráfico de red", "Network traffic"),
    ("Usuarios conectados", "Logged-in users"),
    ("Procesos", "Processes"),
    ("Fecha y hora del servidor", "Server date and time"),
    (
        "El servidor no dejó abrir otro canal",
        "The server did not allow opening another channel",
    ),
    ("No se pudo lanzar la consulta", "Could not start the query"),
    (
        "El servidor respondió algo inesperado",
        "The server replied with something unexpected",
    ),
    (
        "El servidor no permite ejecutar órdenes",
        "The server does not allow running commands",
    ),
    (
        "El servidor no devolvió datos (hace falta una shell POSIX)",
        "The server returned no data (a POSIX shell is required)",
    ),
    ("La consulta se interrumpió", "The query was interrupted"),
    // --- src/paths.rs ---
    (
        "solo se abren direcciones http, https o ftp",
        "only http, https or ftp addresses are opened",
    ),
    // --- src/proxy.rs ---
    ("Sin proxy", "No proxy"),
    (
        "No se pudo conectar a {host}:{port}",
        "Could not connect to {host}:{port}",
    ),
    (
        "Tiempo de espera agotado conectando a {host}:{port}",
        "Timed out connecting to {host}:{port}",
    ),
    (
        "No se pudo conectar con el proxy {proxy}",
        "Could not connect to the proxy {proxy}",
    ),
    ("a través del proxy {proxy}", "through the proxy {proxy}"),
    ("nombre de equipo no válido", "invalid host name"),
    (
        "la respuesta del proxy no tiene fin",
        "the proxy's reply never ends",
    ),
    (
        "el proxy cerró la conexión sin contestar",
        "the proxy closed the connection without answering",
    ),
    (
        "lo que contesta no es un proxy HTTP",
        "what answers is not an HTTP proxy",
    ),
    (
        "el proxy pide usuario y contraseña (o no acepta los indicados; si la contraseña está en el baúl, desbloquéalo)",
        "the proxy asks for a user and password (or does not accept the ones given; if the password is in the vault, unlock it)",
    ),
    (
        "el proxy no permite conectar con {target}",
        "the proxy does not allow connecting to {target}",
    ),
    (
        "el proxy contestó «{status_line}»",
        "the proxy answered “{status_line}”",
    ),
    (
        "usuario o contraseña demasiado largos para SOCKS5",
        "user or password too long for SOCKS5",
    ),
    (
        "lo que contesta no es un proxy SOCKS5",
        "what answers is not a SOCKS5 proxy",
    ),
    (
        "el proxy no acepta el usuario o la contraseña",
        "the proxy does not accept the user or the password",
    ),
    (
        "el proxy pide una autenticación que no se le puede dar",
        "the proxy asks for an authentication that cannot be provided",
    ),
    ("respuesta SOCKS5 no válida", "invalid SOCKS5 reply"),
    (
        "el proxy no permite esa conexión",
        "the proxy does not allow that connection",
    ),
    (
        "red inalcanzable desde el proxy",
        "network unreachable from the proxy",
    ),
    (
        "equipo inalcanzable desde el proxy",
        "host unreachable from the proxy",
    ),
    (
        "el destino rechazó la conexión",
        "the destination rejected the connection",
    ),
    (
        "la conexión tardó demasiado",
        "the connection took too long",
    ),
    (
        "el proxy no pudo conectar (código {code})",
        "the proxy could not connect (code {code})",
    ),
    // --- src/rdp.rs ---
    (
        "FreeRDP no encontrado (instala freerdp3-x11 / freerdp)",
        "FreeRDP not found (install freerdp3-x11 / freerdp)",
    ),
    ("No se pudo ejecutar {program}", "Could not run {program}"),
    (
        "No se encontró ningún cliente FreeRDP en el PATH. Instala `freerdp3-x11` (Debian/Ubuntu), `freerdp` (Fedora/Arch) o `brew install freerdp` (macOS).",
        "No FreeRDP client was found in the PATH. Install `freerdp3-x11` (Debian/Ubuntu), `freerdp` (Fedora/Arch) or `brew install freerdp` (macOS).",
    ),
    (
        "Los datos de la sesión contienen saltos de línea",
        "The session data contains line breaks",
    ),
    (
        "Indica el usuario en la sesión para conectar con FreeRDP 2",
        "Enter the user in the session to connect with FreeRDP 2",
    ),
    // --- src/rdp_client.rs ---
    (
        "Error interno del cliente RDP",
        "Internal error in the RDP client",
    ),
    (
        "El servidor anunció un tamaño de escritorio no válido ({width}×{height})",
        "The server announced an invalid desktop size ({width}×{height})",
    ),
    (
        "Autenticación fallida: usuario o contraseña incorrectos ({detail})",
        "Authentication failed: wrong user or password ({detail})",
    ),
    (
        "El servidor rechazó la contraseña",
        "The server rejected the password",
    ),
    (
        "No se pudo completar la conexión",
        "Could not complete the connection",
    ),
    (
        "Contraseña de {user} en {host}",
        "Password for {user} on {host}",
    ),
    (
        "Contraseña incorrecta. Contraseña de {user} en {host}",
        "Wrong password. Password for {user} on {host}",
    ),
    ("Escritorio remoto", "Remote desktop"),
    ("Autenticación cancelada", "Authentication cancelled"),
    ("Tiempo de espera agotado {what}", "Timed out {what}"),
    (
        "Conectando a {host}:{port}…",
        "Connecting to {host}:{port}…",
    ),
    ("Negociando seguridad…", "Negotiating security…"),
    ("negociando con el servidor", "negotiating with the server"),
    (
        "Fallo al negociar la conexión",
        "Failed to negotiate the connection",
    ),
    (
        "El servidor envió datos inesperados antes de la negociación TLS",
        "The server sent unexpected data before the TLS negotiation",
    ),
    ("negociando TLS", "negotiating TLS"),
    (
        "El servidor no presentó ningún certificado TLS",
        "The server presented no TLS certificate",
    ),
    (
        "Certificado rechazado por el usuario",
        "Certificate rejected by the user",
    ),
    ("guardando en {path}", "saving to {path}"),
    (
        "¡EL CERTIFICADO TLS DE {host}:{port} HA CAMBIADO! Puede ser un intento de suplantación (ataque de intermediario) o que el servidor haya renovado su certificado. Huella recibida: {fingerprint}. Huella recordada: {expected}. Si el cambio es legítimo, elimina la entrada «{entry}:{port}» de {file} y vuelve a conectar.",
        "THE TLS CERTIFICATE OF {host}:{port} HAS CHANGED! This may be an impersonation attempt (man-in-the-middle attack), or the server may have renewed its certificate. Fingerprint received: {fingerprint}. Fingerprint remembered: {expected}. If the change is legitimate, remove the entry “{entry}:{port}” from {file} and connect again.",
    ),
    (
        "El servidor exige autenticación de red (NLA): indica el usuario en la sesión",
        "The server requires network level authentication (NLA): enter the user in the session",
    ),
    ("Autenticando…", "Authenticating…"),
    (
        "Estado inesperado del conector RDP antes de CredSSP",
        "Unexpected state of the RDP connector before CredSSP",
    ),
    (
        "No se pudo iniciar la autenticación (NLA)",
        "Could not start the authentication (NLA)",
    ),
    ("autenticando", "authenticating"),
    (
        "Fallo en la autenticación (NLA)",
        "The authentication (NLA) failed",
    ),
    ("estableciendo la sesión", "establishing the session"),
    (
        "No se pudo establecer la sesión",
        "Could not establish the session",
    ),
    (
        "El servidor pidió una redirección a otro equipo, no soportada",
        "The server asked for a redirection to another host, which is not supported",
    ),
    (
        "No se pudieron preparar los códecs gráficos: {e}",
        "Could not prepare the graphics codecs: {e}",
    ),
    ("Kerberos no está soportado", "Kerberos is not supported"),
    (
        "el servidor cerró la conexión",
        "the server closed the connection",
    ),
    (
        "el servidor exige TLS o NLA",
        "the server requires TLS or NLA",
    ),
    (
        "el servidor solo admite la seguridad RDP estándar (sin TLS), que no está soportada",
        "the server only allows standard RDP security (no TLS), which is not supported",
    ),
    (
        "el servidor no tiene un certificado válido",
        "the server has no valid certificate",
    ),
    ("el servidor exige NLA", "the server requires NLA"),
    (
        "el servidor exige un certificado de cliente, no soportado",
        "the server requires a client certificate, which is not supported",
    ),
    (
        "el servidor exige autenticación con Microsoft Entra, no soportada",
        "the server requires Microsoft Entra authentication, which is not supported",
    ),
    (
        "el servidor denegó el acceso a este usuario",
        "the server denied access to this user",
    ),
    (
        "Error en la sesión RDP: {error}",
        "RDP session error: {error}",
    ),
    (
        "Se perdió la conexión con el servidor",
        "The connection to the server was lost",
    ),
    (
        "El servidor cerró la conexión",
        "The server closed the connection",
    ),
    (
        "El servidor cerró la sesión: {description}",
        "The server closed the session: {description}",
    ),
    ("reactivando la sesión", "reactivating the session"),
    (
        "Fallo al reactivar la sesión",
        "Failed to reactivate the session",
    ),
    (
        "El servidor negoció parámetros no válidos al reactivar",
        "The server negotiated invalid parameters on reactivation",
    ),
    // --- src/rdp_client/tls.rs ---
    ("No se pudo preparar TLS", "Could not set up TLS"),
    ("Fallo en la negociación TLS", "The TLS negotiation failed"),
    (
        "No se pudo interpretar el certificado del servidor: {e}",
        "Could not parse the server's certificate: {e}",
    ),
    (
        "El certificado del servidor tiene una clave pública no válida",
        "The server's certificate has an invalid public key",
    ),
    // --- src/serial.rs ---
    (
        "No se pudo abrir el puerto {device}",
        "Could not open the port {device}",
    ),
    (
        "No se pudo duplicar el puerto serie",
        "Could not duplicate the serial port",
    ),
    ("Error en el puerto serie: {e}", "Serial port error: {e}"),
    // --- src/session_log.rs ---
    (
        "No se pudo crear la carpeta {path}",
        "Could not create the folder {path}",
    ),
    ("No se pudo crear {path}", "Could not create {path}"),
    (
        "No se pudo crear un fichero para «{session}» en {path}: {error}",
        "Could not create a file for “{session}” in {path}: {error}",
    ),
    // --- src/sftp.rs ---
    ("Iniciando SFTP…", "Starting SFTP…"),
    (
        "El servidor no ofrece el subsistema SFTP",
        "The server does not offer the SFTP subsystem",
    ),
    ("No se pudo iniciar SFTP: {e}", "Could not start SFTP: {e}"),
    (
        "No se pudo listar {path}: {error}",
        "Could not list {path}: {error}",
    ),
    ("Carpeta creada: {path}", "Folder created: {path}"),
    (
        "No se pudo crear {path}: {e}",
        "Could not create {path}: {e}",
    ),
    ("Eliminado: {path}", "Deleted: {path}"),
    (
        "No se pudo eliminar {path}: {error}",
        "Could not delete {path}: {error}",
    ),
    ("Renombrado a {to}", "Renamed to {to}"),
    ("No se pudo renombrar: {e}", "Could not rename: {e}"),
    (
        "Permisos de {path}: {mode}",
        "Permissions of {path}: {mode}",
    ),
    (
        "Permisos de {path} y su contenido ({n} elementos): {mode}",
        "Permissions of {path} and its contents ({n} items): {mode}",
    ),
    (
        "No se pudieron cambiar los permisos de {path}: {error}",
        "Could not change the permissions of {path}: {error}",
    ),
    ("en {path}", "at {path}"),
    ("listando {path}", "listing {path}"),
    ("Cancelado", "Cancelled"),
    ("creando {path}", "creating {path}"),
    (
        "Nombre remoto no válido: {name}",
        "Invalid remote name: {name}",
    ),
    ("abriendo {path}", "opening {path}"),
    (
        "No se puede reanudar {path}: ya es mayor que el origen",
        "Cannot resume {path}: it is already larger than the source",
    ),
    ("leyendo {path}", "reading {path}"),
    (
        "{path} no es una carpeta (no se siguen enlaces)",
        "{path} is not a folder (links are not followed)",
    ),
    (
        "ya no es un fichero normal",
        "it is no longer a regular file",
    ),
    ("subiendo {path}", "uploading {path}"),
    ("descargando {path}", "downloading {path}"),
    (
        "Ruta no válida en el plan: {path}",
        "Invalid path in the plan: {path}",
    ),
    // --- src/shells.rs ---
    ("Símbolo del sistema (cmd)", "Command Prompt (cmd)"),
    // --- src/ssh.rs ---
    ("en el salto {name}", "at the jump host {name}"),
    (
        "Tiempo de espera agotado saltando hacia {host}:{port}",
        "Timed out jumping to {host}:{port}",
    ),
    (
        "El último salto no pudo abrir la conexión con {host}:{port}: {e}",
        "The last jump host could not open the connection to {host}:{port}: {e}",
    ),
    (
        "El servidor presentó un certificado de host; no está soportado",
        "The server presented a host certificate; this is not supported",
    ),
    (
        "Clave de host rechazada por el usuario",
        "Host key rejected by the user",
    ),
    (
        "¡LA CLAVE DEL HOST HA CAMBIADO! Puede ser un ataque de intermediario o que el servidor se haya reinstalado. Huella recibida: {fingerprint}. Si el cambio es legítimo, elimina la línea {line} de {file} y vuelve a conectar.",
        "THE HOST KEY HAS CHANGED! This may be a man-in-the-middle attack, or the server may have been reinstalled. Fingerprint received: {fingerprint}. If the change is legitimate, remove line {line} from {file} and connect again.",
    ),
    (
        "error leyendo known_hosts: {e}",
        "error reading known_hosts: {e}",
    ),
    (
        "Reenvío del agente no disponible: {error}",
        "Agent forwarding not available: {error}",
    ),
    (
        "Reenvío del agente SSH activo",
        "SSH agent forwarding enabled",
    ),
    (
        "Reenvío del agente SSH activo (Pageant)",
        "SSH agent forwarding enabled (Pageant)",
    ),
    (
        "El servidor rechazó el reenvío del agente (¿AllowAgentForwarding yes?)",
        "The server rejected agent forwarding (AllowAgentForwarding yes?)",
    ),
    ("Saltando hacia {host}…", "Jumping to {host}…"),
    (
        "El salto no pudo abrir la conexión con {host}:{port}: {e}",
        "The jump host could not open the connection to {host}:{port}: {e}",
    ),
    ("Conectando a {host}…", "Connecting to {host}…"),
    ("Autenticando como {user}…", "Authenticating as {user}…"),
    (
        "Código de verificación TOTP enviado",
        "TOTP verification code sent",
    ),
    (
        "El código TOTP de ahora ya se ha usado: esperando al siguiente ({secs} s)…",
        "The current TOTP code has already been used: waiting for the next one ({secs} s)…",
    ),
    (
        "Contraseña o código de verificación incorrectos. Contraseña para {target}",
        "Wrong password or verification code. Password for {target}",
    ),
    ("Contraseña para {target}", "Password for {target}"),
    (
        "Contraseña incorrecta. Contraseña para {target}",
        "Wrong password. Password for {target}",
    ),
    ("Autenticación SSH", "SSH authentication"),
    (
        "El servidor no admite autenticación por contraseña",
        "The server does not allow password authentication",
    ),
    (
        "La contraseña guardada o el código de verificación fueron rechazados",
        "The saved password or the verification code was rejected",
    ),
    (
        "La contraseña guardada fue rechazada",
        "The saved password was rejected",
    ),
    (
        "Autenticación fallida: contraseña o código de verificación incorrectos",
        "Authentication failed: wrong password or verification code",
    ),
    (
        "Autenticación fallida: contraseña incorrecta",
        "Authentication failed: wrong password",
    ),
    ("el código de verificación", "the verification code"),
    ("la respuesta", "the answer"),
    ("la contraseña", "the password"),
    (
        "Autenticación fallida: el servidor exige además un método que no se puede ofrecer aquí ({methods})",
        "Authentication failed: the server also requires a method that cannot be offered here ({methods})",
    ),
    (
        "Autenticación fallida: el servidor rechazó {what}",
        "Authentication failed: the server rejected {what}",
    ),
    (
        "El servidor no aceptó {what}",
        "The server did not accept {what}",
    ),
    (
        "Autenticación fallida: el servidor encadena demasiados pasos",
        "Authentication failed: the server chains too many steps",
    ),
    (
        "El código TOTP automático ya se envió y no bastó: escribe el siguiente.\n{msg}",
        "The automatic TOTP code was already sent and was not enough: type the next one.\n{msg}",
    ),
    (
        "El servidor no aceptó el código anterior.\n{msg}",
        "The server did not accept the previous code.\n{msg}",
    ),
    (
        "no se pudo firmar con la clave privada: {e}",
        "could not sign with the private key: {e}",
    ),
    (
        "El certificado {name} no corresponde a la clave {key}",
        "The certificate {name} does not match the key {key}",
    ),
    (
        "{name} es un certificado de host, no de usuario",
        "{name} is a host certificate, not a user one",
    ),
    (
        "El certificado {name} {validity}",
        "The certificate {name}: {validity}",
    ),
    (
        "El servidor no aceptó el certificado {name} (vale para {principals}, no para «{user}»)",
        "The server did not accept the certificate {name} (it is valid for {principals}, not for “{user}”)",
    ),
    (
        "El servidor no aceptó el certificado {name} (¿confía el servidor en la autoridad que lo firmó?)",
        "The server did not accept the certificate {name} (does the server trust the authority that signed it?)",
    ),
    (
        "No se pudo leer la clave {path}",
        "Could not read the key {path}",
    ),
    (
        "Passphrase de la clave {path} (para {target})",
        "Passphrase for the key {path} (for {target})",
    ),
    (
        "Passphrase incorrecta. Passphrase de {path} (para {target})",
        "Wrong passphrase. Passphrase for {path} (for {target})",
    ),
    (
        "No se pudo descifrar la clave privada",
        "Could not decrypt the private key",
    ),
    (
        "Clave privada no válida ({path}): {e}",
        "Invalid private key ({path}): {e}",
    ),
    (
        "{problem}: se prueba la clave sin certificado",
        "{problem}: trying the key without a certificate",
    ),
    (
        "El servidor rechazó la clave {path}. {problem}",
        "The server rejected the key {path}. {problem}",
    ),
    (
        "El servidor rechazó la clave {path}",
        "The server rejected the key {path}",
    ),
    (
        "Toca la llave de seguridad{name} para confirmar…",
        "Touch the security key{name} to confirm…",
    ),
    (" «{name}»", " “{name}”"),
    (
        "el certificado «{id}» {validity}",
        "the certificate “{id}”: {validity}",
    ),
    (
        "El agente tiene un certificado que no vale: {note}",
        "The agent holds a certificate that is not valid: {note}",
    ),
    (
        "la llave de seguridad{name} no firmó (¿está conectada y la has tocado?)",
        "the security key{name} did not sign (is it plugged in, and did you touch it?)",
    ),
    (
        "el agente no firmó con la clave{name}: {e}",
        "the agent did not sign with the key{name}: {e}",
    ),
    ("Aviso: {note}", "Warning: {note}"),
    (
        "Se perdió la conexión mientras firmaba el agente: {e}",
        "The connection was lost while the agent was signing: {e}",
    ),
    (
        "No se pudo usar el agente SSH: {error}",
        "Could not use the SSH agent: {error}",
    ),
    (
        "El agente SSH no tiene ninguna clave cargada",
        "The SSH agent has no keys loaded",
    ),
    (
        "Pageant no tiene ninguna clave cargada",
        "Pageant has no keys loaded",
    ),
    (
        "Ninguna clave del agente fue aceptada por el servidor",
        "The server accepted none of the agent's keys",
    ),
    (
        "Ninguna clave del agente fue aceptada por el servidor ({notes})",
        "The server accepted none of the agent's keys ({notes})",
    ),
    (
        "{name} es una llave de seguridad: firma la propia llave, a través del agente SSH. Cárgala en el agente (ssh-add {path}) y vuelve a conectar",
        "{name} is a security key: the key itself signs, through the SSH agent. Load it into the agent (ssh-add {path}) and connect again",
    ),
    (
        "No se pudo usar el agente SSH ({error}). {how}",
        "Could not use the SSH agent ({error}). {how}",
    ),
    (
        "El agente SSH no tiene cargada esta clave. {how}",
        "The SSH agent does not have this key loaded. {how}",
    ),
    (
        "El servidor rechazó la clave {path} (llave de seguridad)",
        "The server rejected the key {path} (security key)",
    ),
    (
        "No se pudo entrar con la clave {path}: {notes}",
        "Could not log in with the key {path}: {notes}",
    ),
    (
        "Reenvío X11 no disponible: {error}",
        "X11 forwarding not available: {error}",
    ),
    (
        "Reenvío X11 activo: las aplicaciones gráficas se abrirán en la pestaña «Servidor X»",
        "X11 forwarding enabled: graphical applications will open in the “X server” tab",
    ),
    (
        "Reenvío X11 activo (servidor X del sistema)",
        "X11 forwarding enabled (system X server)",
    ),
    (
        "El servidor rechazó el reenvío X11 (¿X11Forwarding yes y xauth instalado?)",
        "The server rejected X11 forwarding (X11Forwarding yes and xauth installed?)",
    ),
    (
        "La shell terminó con código {code}",
        "The shell exited with code {code}",
    ),
    // --- src/telnet.rs ---
    (
        "Conectando a {host}:{port} por Telnet (sin cifrar)…",
        "Connecting to {host}:{port} over Telnet (unencrypted)…",
    ),
    (
        "Conectando a {host}:{port} por Telnet (sin cifrar tras el último salto)…",
        "Connecting to {host}:{port} over Telnet (unencrypted after the last jump host)…",
    ),
    // --- src/terminal.rs ---
    (
        "No se pudo abrir el enlace: {e}",
        "Could not open the link: {e}",
    ),
    ("Túnel {n}: {e}", "Tunnel {n}: {e}"),
    ("Error: {e}", "Error: {e}"),
    ("[Sesión finalizada]", "[Session ended]"),
    (
        "⚠ Vas a pegar {lines} líneas ({chars} caracteres)",
        "⚠ You are about to paste {lines} lines ({chars} characters)",
    ),
    (
        "En una shell, cada línea se ejecuta según llega. Revisa lo que hay en el portapapeles antes de enviarlo.",
        "In a shell, each line runs as soon as it arrives. Check what is in the clipboard before sending it.",
    ),
    ("… y {n} líneas más", "… and {n} more lines"),
    ("Pegar", "Paste"),
    ("Cancelar", "Cancel"),
    ("Intro pega · Esc cancela", "Enter pastes · Esc cancels"),
    (
        "{url}\nCtrl+clic para abrirlo",
        "{url}\nCtrl+click to open it",
    ),
    ("🔗 Abrir enlace", "🔗 Open link"),
    ("Copiar enlace", "Copy link"),
    ("Copiar", "Copy"),
    ("Copiar toda la pantalla", "Copy the whole screen"),
    ("Limpiar historial", "Clear scrollback"),
    (
        "⬆ historial ({offset} líneas)",
        "⬆ scrollback ({offset} lines)",
    ),
    ("Buscar en el terminal", "Search the terminal"),
    ("sin resultados", "no results"),
    ("{at} de {of}", "{at} of {of}"),
    ("Anterior (Intro)", "Previous (Enter)"),
    ("Siguiente (Mayús+Intro)", "Next (Shift+Enter)"),
    ("Cerrar (Esc)", "Close (Esc)"),
    // --- src/theme.rs ---
    (
        "color no válido «{text}» (se espera #rrggbb, por ejemplo #1e90ff)",
        "invalid color “{text}” (expected #rrggbb, for example #1e90ff)",
    ),
    ("Fondo", "Background"),
    ("Fondo de ventanas y menús", "Window and menu background"),
    ("Fondo de campos de texto", "Text field background"),
    ("Fondo alterno", "Alternate background"),
    ("Texto tenue", "Dim text"),
    ("Texto destacado", "Strong text"),
    ("Acento (selección)", "Accent (selection)"),
    ("Texto sobre el acento", "Text on the accent"),
    ("Botón", "Button"),
    ("Botón (ratón encima)", "Button (hovered)"),
    ("Botón (pulsado)", "Button (pressed)"),
    ("Bordes", "Borders"),
    ("Enlaces", "Links"),
    ("Correcto", "Success"),
    ("Aviso", "Warning"),
    ("Error", "Error"),
    ("Cursor", "Cursor"),
    ("Fondo de la selección", "Selection background"),
    ("Texto de la selección", "Selection text"),
    ("Negro", "Black"),
    ("Blanco", "White"),
    ("Negro brillante", "Bright black"),
    ("Rojo brillante", "Bright red"),
    ("Verde brillante", "Bright green"),
    ("Amarillo brillante", "Bright yellow"),
    ("Azul brillante", "Bright blue"),
    ("Magenta brillante", "Bright magenta"),
    ("Cian brillante", "Bright cyan"),
    ("Blanco brillante", "Bright white"),
    (
        "El tema no es válido (línea {line}, columna {column}): {e}",
        "The theme is not valid (line {line}, column {column}): {e}",
    ),
    (
        "El tema no tiene nombre (campo «nombre»)",
        "The theme has no name (field “nombre”)",
    ),
    (
        "{path} es demasiado grande para ser un tema",
        "{path} is too large to be a theme",
    ),
    (
        "{path}: nombre repetido «{name}»",
        "{path}: duplicate name “{name}”",
    ),
    ("El tema necesita un nombre", "The theme needs a name"),
    (
        "«{name}» es un tema integrado; elige otro nombre",
        "“{name}” is a built-in theme; choose another name",
    ),
    (
        "Ya existe un tema llamado «{name}»",
        "A theme named “{name}” already exists",
    ),
    (
        "No se pudo guardar el tema en {path}",
        "Could not save the theme to {path}",
    ),
    (
        "Los temas integrados no se pueden eliminar",
        "Built-in themes cannot be deleted",
    ),
    // --- src/totp.rs ---
    (
        "Algoritmo TOTP no soportado: {text} (vale SHA1, SHA256 o SHA512)",
        "Unsupported TOTP algorithm: {text} (SHA1, SHA256 or SHA512 are accepted)",
    ),
    (
        "El secreto TOTP no es base32 válido (solo letras de la A a la Z y cifras del 2 al 7)",
        "The TOTP secret is not valid base32 (only letters A to Z and digits 2 to 7)",
    ),
    ("Falta el secreto TOTP", "The TOTP secret is missing"),
    (
        "El secreto TOTP es demasiado largo",
        "The TOTP secret is too long",
    ),
    (
        "Los códigos HOTP (por contador) no están soportados: solo TOTP (por hora)",
        "HOTP (counter-based) codes are not supported: only TOTP (time-based)",
    ),
    (
        "La URI no es de un código TOTP (otpauth://totp/…)",
        "The URI is not for a TOTP code (otpauth://totp/…)",
    ),
    (
        "Número de cifras TOTP no válido: {value}",
        "Invalid number of TOTP digits: {value}",
    ),
    (
        "Periodo TOTP no válido: {value}",
        "Invalid TOTP period: {value}",
    ),
    (
        "La URI otpauth:// no trae el secreto",
        "The otpauth:// URI does not include the secret",
    ),
    (
        "Los códigos TOTP deben tener de 6 a 8 cifras, no {digits}",
        "TOTP codes must have 6 to 8 digits, not {digits}",
    ),
    (
        "El periodo TOTP debe estar entre 1 y {max} segundos",
        "The TOTP period must be between 1 and {max} seconds",
    ),
    (
        "{algorithm} · {digits} cifras · cada {period} s",
        "{algorithm} · {digits} digits · every {period} s",
    ),
    // --- src/triggers.rs ---
    (
        "Falta el texto que dispara la regla",
        "The text that fires the rule is missing",
    ),
    (
        "El texto es demasiado largo (máximo {max} caracteres)",
        "The text is too long (maximum {max} characters)",
    ),
    (
        "El texto debe tener algo más que comodines",
        "The text must contain more than wildcards",
    ),
    (
        "La regla no hace nada: marca «Avisar» o «Enviar un texto»",
        "The rule does nothing: check “Notify” or “Send a text”",
    ),
    (
        "El texto que se envía es demasiado largo (máximo {max} caracteres)",
        "The text to send is too long (maximum {max} characters)",
    ),
    (
        "Lo que envía contiene el texto que la dispara: se respondería a sí misma",
        "What it sends contains the text that fires it: it would answer itself",
    ),
    // --- src/trust.rs ---
    (
        "fichero de confianza dañado: {path}",
        "damaged trust file: {path}",
    ),
    // --- src/tunnel_manager.rs ---
    (
        "Elige el equipo SSH por el que pasa el túnel",
        "Choose the SSH host the tunnel goes through",
    ),
    (
        "El túnel no se pudo abrir",
        "The tunnel could not be opened",
    ),
    // --- src/tunnels.rs ---
    (
        "el servidor rechazó escuchar en {host}:{port} (¿AllowTcpForwarding / GatewayPorts?)",
        "the server rejected listening on {host}:{port} (AllowTcpForwarding / GatewayPorts?)",
    ),
    (
        "no se pudo escuchar en {host}:{port}: {e}",
        "could not listen on {host}:{port}: {e}",
    ),
    // --- src/vault.rs ---
    (
        "parámetros KDF no válidos: {e}",
        "invalid KDF parameters: {e}",
    ),
    (
        "no se pudo derivar la clave: {e}",
        "could not derive the key: {e}",
    ),
    (
        "La contraseña maestra no puede estar vacía",
        "The master password cannot be empty",
    ),
    (
        "Ya existe un baúl en {path}",
        "A vault already exists at {path}",
    ),
    ("baúl dañado", "damaged vault"),
    (
        "Versión de baúl no soportada: {version}",
        "Unsupported vault version: {version}",
    ),
    ("baúl dañado (salt)", "damaged vault (salt)"),
    ("baúl dañado (nonce)", "damaged vault (nonce)"),
    ("clave no válida", "invalid key"),
    ("Contraseña maestra incorrecta", "Wrong master password"),
    ("baúl dañado (contenido)", "damaged vault (contents)"),
    ("fallo al cifrar el baúl", "failed to encrypt the vault"),
    // --- src/vnc.rs ---
    (
        "subrectángulo Hextile fuera de la tesela",
        "Hextile subrectangle outside the tile",
    ),
    ("datos ZRLE truncados", "truncated ZRLE data"),
    (
        "índice de paleta ZRLE no válido",
        "invalid ZRLE palette index",
    ),
    (
        "tramo ZRLE más largo que la tesela",
        "ZRLE run longer than the tile",
    ),
    (
        "subcodificación ZRLE no válida: {sub}",
        "invalid ZRLE subencoding: {sub}",
    ),
    ("datos ZRLE demasiado grandes", "ZRLE data too large"),
    ("flujo ZRLE dañado: {e}", "damaged ZRLE stream: {e}"),
    (
        "Conectando a {host}:{port} por VNC (sin cifrar)…",
        "Connecting to {host}:{port} over VNC (unencrypted)…",
    ),
    (
        "Conectando a {host}:{port} por VNC (sin cifrar tras el último salto)…",
        "Connecting to {host}:{port} over VNC (unencrypted after the last jump host)…",
    ),
    (
        "El servidor no respondió como un servidor VNC",
        "The server did not answer like a VNC server",
    ),
    (
        "El servidor no habla el protocolo VNC (RFB)",
        "The server does not speak the VNC (RFB) protocol",
    ),
    ("Versión RFB no válida", "Invalid RFB version"),
    (
        "El servidor rechazó la conexión: {reason}",
        "The server rejected the connection: {reason}",
    ),
    (
        "El servidor exige un tipo de seguridad no soportado ({types})",
        "The server requires an unsupported security type ({types})",
    ),
    (
        "Contraseña VNC de {host}:{port}",
        "VNC password for {host}:{port}",
    ),
    (
        "Tipo de seguridad VNC no soportado: {other}",
        "Unsupported VNC security type: {other}",
    ),
    (
        "Autenticación VNC fallida: contraseña incorrecta {reason}",
        "VNC authentication failed: wrong password {reason}",
    ),
    (
        "Nombre de escritorio demasiado largo",
        "Desktop name too long",
    ),
    (
        "Tamaño de escritorio no válido: {width}x{height}",
        "Invalid desktop size: {width}x{height}",
    ),
    (
        "El servidor envió un rectángulo fuera de la pantalla",
        "The server sent a rectangle outside the screen",
    ),
    (
        "CopyRect fuera de la pantalla",
        "CopyRect outside the screen",
    ),
    ("Bloque ZRLE demasiado grande", "ZRLE block too large"),
    (
        "Codificación VNC no soportada: {other}",
        "Unsupported VNC encoding: {other}",
    ),
    (
        "Texto de portapapeles demasiado grande",
        "Clipboard text too large",
    ),
    (
        "Mensaje VNC desconocido: {other}",
        "Unknown VNC message: {other}",
    ),
    // --- src/x11.rs ---
    (
        "no hay servidor X local (variable DISPLAY sin definir)",
        "there is no local X server (DISPLAY is not set)",
    ),
    ("DISPLAY no válido: {value}", "invalid DISPLAY: {value}"),
    // --- src/xserver/mod.rs ---
    ("El servidor X está parado", "The X server is stopped"),
    (
        "Número de pantalla X demasiado alto: {number}",
        "X display number too high: {number}",
    ),
    (
        "No se pudo abrir el puerto del servidor X: {e}",
        "Could not open the X server port: {e}",
    ),
    (
        "No queda ningún número de pantalla X libre",
        "No free X display number is left",
    ),
];

#[cfg(test)]
mod tests {
    use super::T;
    use crate::ssh::worth_retrying;

    /// El texto sin sus huecos: sus nombres («{key}», «{agent}») no se ven.
    fn without_holes(text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            rest = rest[start..]
                .find('}')
                .map_or("", |end| &rest[start + end + 1..]);
        }
        out + rest
    }

    /// La reconexión automática decide por palabras del mensaje de error si
    /// merece reintentar (`ssh::worth_retrying`), y el mensaje le llega ya
    /// traducido. Cada texto tiene que clasificarse igual en español y en
    /// inglés: si no, según el idioma se insistiría (o no) con una contraseña
    /// rechazada.
    #[test]
    fn errors_are_classified_alike_in_both_languages() {
        // Textos que nunca son, por sí solos, el error de una conexión, y en
        // los que «key» no traduce «clave» sino «tecla» o «llave».
        const NOT_CONNECTION_ERRORS: [&str; 4] = [
            "Falta la tecla: Ctrl, Alt y Mayús solos no son un atajo",
            "Usa Ctrl, Alt o una tecla de función (F1–F12): una tecla sola ya no se podría \
             escribir en el terminal",
            "Toca la llave de seguridad{name} para confirmar…",
            "la llave de seguridad{name} no firmó (¿está conectada y la has tocado?)",
        ];
        let different: Vec<_> = T
            .iter()
            .filter(|(spanish, _)| !NOT_CONNECTION_ERRORS.contains(spanish))
            .filter(|(spanish, english)| {
                worth_retrying(&without_holes(spanish)) != worth_retrying(&without_holes(english))
            })
            .collect();
        assert!(different.is_empty(), "{different:#?}");
        // (Que la lista de excepciones no se quede vieja.)
        for text in NOT_CONNECTION_ERRORS {
            assert!(T.iter().any(|(spanish, _)| *spanish == text), "{text}");
        }
        assert_eq!(without_holes("{a} y {b}: {c}"), " y : ");
        assert_eq!(without_holes("sin huecos"), "sin huecos");
    }
}
