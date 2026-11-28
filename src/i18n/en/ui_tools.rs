//! Herramientas y ventanas auxiliares: panel de estado, barra del servidor,
//! túneles, ejecución en varios equipos, snippets, paleta, escritorio remoto,
//! grabaciones, historial de órdenes y disparadores. (Véase `mod.rs`.)

pub(super) const T: super::Table = &[
    // --- Panel de estado (`src/ui/dashboard_view.rs`) ---
    ("todas las carpetas", "all folders"),
    ("(carpeta eliminada)", "(deleted folder)"),
    ("📊 Panel de estado", "📊 Status dashboard"),
    ("Equipos de", "Hosts in"),
    (
        "🔍 Filtrar por nombre, dirección o estado…",
        "🔍 Filter by name, address or status…",
    ),
    ("Solo con problemas", "Only with problems"),
    (
        "Recargar: vuelve a leer los equipos de la carpeta y abre las conexiones de nuevo",
        "Reload: reads the hosts in the folder again and reopens the connections",
    ),
    ("⚙ Refresco y alertas", "⚙ Refresh and alerts"),
    (
        "Cada cuánto se actualiza y cuándo salta una alerta",
        "How often it updates and when an alert is raised",
    ),
    ("Abriendo las conexiones…", "Opening the connections…"),
    (
        "No hay equipos SSH en esta carpeta.",
        "There are no SSH hosts in this folder.",
    ),
    ("1 equipo con alertas", "1 host with alerts"),
    ("{n} equipos con alertas", "{n} hosts with alerts"),
    ("{online} de {total} en línea", "{online} of {total} online"),
    ("no se vigila", "not watched"),
    ("Actualizar cada", "Update every"),
    (
        "Conexiones a la vez, como mucho",
        "Connections at once, at most",
    ),
    (
        "Cada equipo mantiene su conexión abierta entre refrescos. Si la carpeta tiene más \
         equipos que este tope, se van turnando: cada uno conecta, da sus datos y cede el sitio \
         al siguiente",
        "Each host keeps its connection open between refreshes. If the folder has more hosts \
         than this limit, they take turns: each one connects, reports its data and makes way \
         for the next",
    ),
    ("se aplica al recargar (🔄)", "applied on reload (🔄)"),
    ("Minigráfica", "Sparkline"),
    ("de los últimos", "over the last"),
    ("Avisos", "Notices"),
    (
        "Avisar en la aplicación al saltar o resolverse una alerta",
        "Notify in the app when an alert is raised or resolved",
    ),
    ("CPU por encima del", "CPU above"),
    (" %", "%"),
    ("Carga por núcleo por encima de", "Load per core above"),
    ("     …sostenidas durante", "     …sustained for"),
    (
        "La CPU y la carga tienen que seguir altas todas estas muestras seguidas para que salte \
         la alerta, y otras tantas bajas para darla por resuelta",
        "CPU and load must stay high for this many samples in a row before the alert is raised, \
         and low for as many before it is considered resolved",
    ),
    (" muestras seguidas", " samples in a row"),
    ("Memoria por encima del", "Memory above"),
    ("Disco más lleno por encima del", "Fullest disk above"),
    (
        "Equipo inalcanzable o sin acceso",
        "Host unreachable or no access",
    ),
    (
        "Cuando falla el intento de conectar o de volver a conectar, y cuando se llega al equipo \
         pero no se puede entrar (falta una credencial, el servidor la rechaza o ha cambiado su \
         clave)",
        "When an attempt to connect or reconnect fails, and when the host is reached but cannot \
         be logged in to (a credential is missing, the server rejects it or its key has changed)",
    ),
    ("Restablecer", "Reset"),
    ("Equipo", "Host"),
    ("Estado", "Status"),
    ("Latencia", "Latency"),
    ("CPU", "CPU"),
    ("Carga", "Load"),
    ("Memoria", "Memory"),
    ("Disco más lleno", "Fullest disk"),
    ("Encendido", "Uptime"),
    ("Tendencia", "Trend"),
    ("Actualizado", "Updated"),
    (
        "A la espera de turno para conectar",
        "Waiting for its turn to connect",
    ),
    ("Sin conexión: {error}", "No connection: {error}"),
    ("Reintentando: {step}", "Retrying: {step}"),
    (
        "Intentos fallidos: {n} · próximo intento a las {hora}",
        "Failed attempts: {n} · next attempt at {hora}",
    ),
    (
        "Falta una contraseña o passphrase que no está en el baúl. Botón derecho → Reintentar \
         para escribirla",
        "A password or passphrase that is not in the vault is missing. Right-click → Retry to \
         type it",
    ),
    (
        "No se reintenta sola: {error}. Botón derecho → Reintentar",
        "It will not retry on its own: {error}. Right-click → Retry",
    ),
    (
        "Conectado, pero sin datos: {error}",
        "Connected, but no data: {error}",
    ),
    ("Últimos datos conocidos:", "Last known data:"),
    (
        "Carga: {a} {b} {c} · {cores} núcleo(s)",
        "Load: {a} {b} {c} · {cores} core(s)",
    ),
    ("Carga: {a} {b} {c}", "Load: {a} {b} {c}"),
    ("Memoria: {usada} de {total}", "Memory: {usada} of {total}"),
    (
        "Disco {disco}: {uso} · quedan {libre}",
        "Disk {disco}: {uso} · {libre} left",
    ),
    ("Alertas: {lista}", "Alerts: {lista}"),
    (
        "Doble clic: abrir su terminal",
        "Double-click: open its terminal",
    ),
    ("Ordenar por esta columna", "Sort by this column"),
    (
        "{dato} en los últimos {minutos} min. La raya marca el umbral de la alerta",
        "{dato} over the last {minutos} min. The line marks the alert threshold",
    ),
    (
        "Ningún equipo coincide con el filtro.",
        "No host matches the filter.",
    ),
    ("💻 Abrir su terminal", "💻 Open its terminal"),
    ("🔁 Reintentar ahora", "🔁 Retry now"),
    (
        "Vuelve a conectar ya, preguntando la contraseña si hace falta",
        "Reconnects right away, asking for the password if needed",
    ),
    ("Copiar lo que se sabe de él", "Copy what is known about it"),
    ("🔔 Alertas: ninguna activa", "🔔 Alerts: none active"),
    ("🔔 Alertas: 1 activa", "🔔 Alerts: 1 active"),
    ("🔔 Alertas: {n} activas", "🔔 Alerts: {n} active"),
    (
        "Mostrar u ocultar el historial de alertas",
        "Show or hide the alert history",
    ),
    ("· historial ({n})", "· history ({n})"),
    ("Vaciar", "Clear"),
    (
        "Aquí se anota cada alerta que salta y cuándo se resuelve.",
        "Each alert that is raised, and when it is resolved, is noted here.",
    ),
    ("alerta", "alert"),
    ("resuelto", "resolved"),
    (
        "No se pudieron guardar las preferencias: {error}",
        "Could not save the settings: {error}",
    ),
    // --- Barra de información del servidor (`src/ui/server_bar.rs`) ---
    ("Sesión", "Session"),
    (
        "Usuario y dirección de conexión",
        "User and connection address",
    ),
    ("Nombre del servidor", "Server name"),
    (
        "Carga media en 1, 5 y 15 minutos",
        "Load average over 1, 5 and 15 minutes",
    ),
    ("{c} núcleo(s)", "{c} core(s)"),
    (
        "Memoria en uso: {uso}\nDisponible: {libre}",
        "Memory in use: {uso}\nAvailable: {libre}",
    ),
    ("Intercambio en uso: {uso}", "Swap in use: {uso}"),
    ("Disco {disco}", "Disk {disco}"),
    ("{espacio} libres", "{espacio} free"),
    (
        "{sistema}\nTotal: {total}\nUsado: {usado} ({uso})",
        "{sistema}\nTotal: {total}\nUsed: {usado} ({uso})",
    ),
    ("Red", "Net"),
    (
        "Tráfico recibido y enviado (sin contar interfaces virtuales)",
        "Traffic received and sent (not counting virtual interfaces)",
    ),
    ("Usuarios", "Users"),
    ("Sesiones de usuario abiertas", "Open user sessions"),
    ("Procesos", "Processes"),
    ("Hora", "Time"),
    ("Hora del servidor: {hora}", "Server time: {hora}"),
    (
        "Elegir qué se muestra en esta barra",
        "Choose what this bar shows",
    ),
    (
        "Sin datos del servidor: {error}",
        "No data from the server: {error}",
    ),
    ("Consultando el servidor…", "Querying the server…"),
    ("Datos sin actualizar: {error}", "Data not updated: {error}"),
    ("Barra de información del servidor", "Server info bar"),
    (
        "Mostrar la barra bajo los terminales SSH",
        "Show the bar below SSH terminals",
    ),
    (
        "Los datos se piden por un canal aparte de la misma conexión, sin escribir nada en tu \
         terminal. Hace falta que el servidor tenga una shell POSIX; el uso de CPU, memoria y \
         red sale de /proc (Linux). Cada equipo puede desactivarla en su ficha.",
        "The data is requested over a separate channel of the same connection, without typing \
         anything in your terminal. The server needs a POSIX shell; CPU, memory and network \
         usage come from /proc (Linux). Each host can turn it off in its own settings.",
    ),
    ("Discos", "Disks"),
    (
        "Rutas del servidor cuyo espacio libre se muestra, separadas por comas",
        "Server paths whose free space is shown, separated by commas",
    ),
    ("Barritas de uso", "Usage bars"),
    (
        "Junto a CPU, memoria y disco",
        "Next to CPU, memory and disk",
    ),
    ("Color de aviso desde", "Warning color from"),
    ("Color de error desde", "Error color from"),
    (
        "Rutas no admitidas (solo letras, números y / _ . -): {rutas}",
        "Paths not allowed (only letters, digits and / _ . -): {rutas}",
    ),
    (
        "Qué se muestra, y en qué orden",
        "What is shown, and in what order",
    ),
    ("Antes", "Before"),
    ("Después", "After"),
    // --- Túneles (`src/ui/tunnels_view.rs`) ---
    (
        "«{nombre}» ya escucha en {host}:{puerto}",
        "“{nombre}” is already listening on {host}:{puerto}",
    ),
    ("No se pudo guardar: {error}", "Could not save: {error}"),
    ("Túnel guardado", "Tunnel saved"),
    ("Túneles", "Tunnels"),
    (
        "Cada túnel abre su propia conexión SSH con el equipo elegido (pasando por sus saltos, \
         si los tiene) y sigue en marcha aunque no haya ningún terminal abierto. Los cambios en \
         un túnel en marcha se aplican al volver a iniciarlo.",
        "Each tunnel opens its own SSH connection to the chosen host (through its jump hosts, if \
         it has any) and keeps running even with no terminal open. Changes to a running tunnel \
         are applied when it is started again.",
    ),
    ("Túneles guardados", "Saved tunnels"),
    ("Aún no hay túneles.", "No tunnels yet."),
    ("Detener", "Stop"),
    ("Iniciar", "Start"),
    ("➕ Nuevo túnel", "➕ New tunnel"),
    ("Nombre", "Name"),
    (
        "opcional; por ejemplo «BD de producción»",
        "optional; for example “Production DB”",
    ),
    ("Equipo SSH", "SSH host"),
    ("(el equipo ya no existe)", "(the host no longer exists)"),
    ("Elige un equipo…", "Choose a host…"),
    (
        "No hay equipos SSH guardados",
        "There are no saved SSH hosts",
    ),
    ("Tipo", "Type"),
    ("Escucha en el servidor", "Listens on the server"),
    ("Escucha en este equipo", "Listens on this computer"),
    (
        "127.0.0.1 solo deja entrar desde el propio equipo; 0.0.0.0 lo abre a toda su red",
        "127.0.0.1 only lets in connections from the computer itself; 0.0.0.0 opens it to its \
         whole network",
    ),
    ("Destino, visto desde aquí", "Target, as seen from here"),
    (
        "Destino, visto desde el servidor",
        "Target, as seen from the server",
    ),
    (
        "Iniciarlo al abrir GodRemote",
        "Start it when GodRemote opens",
    ),
    (
        "Local: lo que se conecte al puerto de este equipo sale por el servidor hacia el destino.",
        "Local: whatever connects to the port on this computer goes out through the server to \
         the target.",
    ),
    (
        "Remoto: lo que se conecte al puerto del servidor llega, a través de este equipo, al \
         destino.",
        "Remote: whatever connects to the port on the server reaches the target through this \
         computer.",
    ),
    (
        "Dinámico: un proxy SOCKS5 en este equipo; cada programa que lo use sale a la red por el \
         servidor.",
        "Dynamic: a SOCKS5 proxy on this computer; every program that uses it reaches the \
         network through the server.",
    ),
    ("Guardar", "Save"),
    ("⏹ Detener", "⏹ Stop"),
    ("▶ Iniciar", "▶ Start"),
    (
        "Guarda el túnel para poder iniciarlo",
        "Save the tunnel to be able to start it",
    ),
    ("🗑 Eliminar", "🗑 Delete"),
    ("Túnel eliminado", "Tunnel deleted"),
    ("En marcha: {tunel}", "Running: {tunel}"),
    ("Detenido", "Stopped"),
    // --- Ejecutar en varios equipos (`src/ui/fleet_view.rs`) ---
    ("Ejecutar en varios equipos", "Run on several hosts"),
    ("A la vez", "At once"),
    ("Tiempo máximo", "Time limit"),
    (
        "Lo que cada equipo puede tardar en conectar y terminar",
        "How long each host may take to connect and finish",
    ),
    ("Resultado ({n})", "Result ({n})"),
    ("Equipos SSH ({n})", "SSH hosts ({n})"),
    ("Todos", "All"),
    ("Ninguno", "None"),
    ("Elegir de nuevo", "Choose again"),
    ("en cola", "queued"),
    ("conectando", "connecting"),
    ("en marcha", "running"),
    ("bien", "ok"),
    ("código {code}", "code {code}"),
    ("terminado", "finished"),
    ("error", "error"),
    ("cancelado", "cancelled"),
    ("Orden", "Command"),
    ("Usar un snippet", "Use a snippet"),
    (
        "por ejemplo: uptime; df -h /",
        "for example: uptime; df -h /",
    ),
    (
        "Se ejecuta sin terminal, como «ssh equipo 'orden'»: no sirve para órdenes que preguntan \
         algo. Admite las variables de los snippets ({{host}}, {{usuario}}, {{nombre}}, \
         {{fecha}}…), que se sustituyen en cada equipo.",
        "It runs without a terminal, like “ssh host 'command'”: it is no use for commands that \
         ask something. It accepts the snippet variables ({{host}}, {{user}}, {{name}}, \
         {{date}}…), which are replaced on each host.",
    ),
    ("⏹ Cancelar", "⏹ Cancel"),
    (
        "Se ejecutará en {n} equipos. ¿Seguro?",
        "It will run on {n} hosts. Are you sure?",
    ),
    ("Sí, ejecutar", "Yes, run"),
    ("No", "No"),
    ("▶ Ejecutar en 1 equipo", "▶ Run on 1 host"),
    ("▶ Ejecutar en {n} equipos", "▶ Run on {n} hosts"),
    ("{ok} bien", "{ok} ok"),
    ("{failed} con error", "{failed} failed"),
    ("{pending} sin terminar", "{pending} unfinished"),
    (
        "Aquí aparecerá lo que conteste cada equipo.",
        "What each host answers will appear here.",
    ),
    ("Copiar su salida", "Copy its output"),
    ("Copiar todo", "Copy all"),
    (
        "La salida de todos los equipos, con una cabecera por equipo",
        "The output of all hosts, with a header for each one",
    ),
    ("Guardar todo…", "Save all…"),
    (
        "[salida recortada: era demasiado larga]",
        "[output truncated: it was too long]",
    ),
    (
        "Guardar la salida de todos los equipos",
        "Save the output of all hosts",
    ),
    ("salida-equipos.txt", "hosts-output.txt"),
    ("Guardado en {ruta}", "Saved to {ruta}"),
    // --- Snippets (`src/ui/macros_view.rs`) ---
    ("Nuevo snippet", "New snippet"),
    (
        "Ctrl+C, Ctrl+V y Ctrl+X están reservados para copiar, pegar y cortar",
        "Ctrl+C, Ctrl+V and Ctrl+X are reserved for copy, paste and cut",
    ),
    (
        "El atajo {shortcut} ya lo usa «{nombre}»",
        "The shortcut {shortcut} is already used by “{nombre}”",
    ),
    ("Snippet guardado", "Snippet saved"),
    ("Snippets", "Snippets"),
    ("Snippets guardados", "Saved snippets"),
    ("Aún no hay snippets.", "No snippets yet."),
    ("➕ Nuevo snippet", "➕ New snippet"),
    (
        "Texto u órdenes a enviar, una por línea",
        "Text or commands to send, one per line",
    ),
    (
        "Cada salto de línea es Intro: sin salto al final, el texto se escribe y queda a la \
         espera. Teclas especiales: \\e (Escape), \\t (tabulador), \\x03 (Ctrl+C), \\x04 \
         (Ctrl+D)…",
        "Each line break is Enter: without a final line break, the text is typed and left \
         waiting. Special keys: \\e (Escape), \\t (Tab), \\x03 (Ctrl+C), \\x04 (Ctrl+D)…",
    ),
    (
        "Variables: {{host}}, {{usuario}}, {{puerto}}, {{nombre}}, {{fecha}}, {{hora}}. Con \
         {{?Pregunta}} o {{?Pregunta=valor}} se pide un dato al lanzarlo.",
        "Variables: {{host}}, {{user}}, {{port}}, {{name}}, {{date}}, {{time}}. With \
         {{?Question}} or {{?Question=value}} you are asked for a value when it is run.",
    ),
    ("▶ Lanzar", "▶ Run"),
    (
        "Envía el snippet guardado al terminal activo",
        "Sends the saved snippet to the active terminal",
    ),
    (
        "Guarda el snippet y abre un terminal conectado",
        "Save the snippet and open a connected terminal",
    ),
    ("Snippet eliminado", "Snippet deleted"),
    (
        "No se pudieron guardar los snippets: {error}",
        "Could not save the snippets: {error}",
    ),
    ("Atajo", "Shortcut"),
    (
        "Pulsa la combinación de teclas… (Esc para cancelar)",
        "Press the key combination… (Esc to cancel)",
    ),
    ("Cancelar", "Cancel"),
    ("sin atajo", "no shortcut"),
    ("Cambiar…", "Change…"),
    ("Asignar…", "Assign…"),
    (
        "Elige la combinación que lanzará este snippet en el terminal activo: con Ctrl o Alt, o \
         una tecla de función (F1–F12)",
        "Choose the combination that will run this snippet in the active terminal: with Ctrl or \
         Alt, or a function key (F1–F12)",
    ),
    ("Quitar", "Remove"),
    (
        "El atajo funciona con el foco en un terminal. Ctrl+letra suele tener ya un uso en la \
         shell; Ctrl+Alt+letra, Ctrl+Mayús+letra y F1–F12 no chocan con nada.",
        "The shortcut works while a terminal has the focus. Ctrl+letter usually already has a \
         use in the shell; Ctrl+Alt+letter, Ctrl+Shift+letter and F1–F12 do not clash with \
         anything.",
    ),
    // --- Paleta de órdenes (`src/ui/palette_view.rs`) ---
    (
        "Equipo, pestaña, snippet, túnel o acción…",
        "Host, tab, snippet, tunnel or action…",
    ),
    (
        "Nada coincide con lo escrito.",
        "Nothing matches what you typed.",
    ),
    (
        "Flechas para moverse · Intro para abrir · Esc para cerrar",
        "Arrows to move · Enter to open · Esc to close",
    ),
    // --- Utilidades de presentación (`src/ui/util.rs`) ---
    ("Rojo", "Red"),
    ("Naranja", "Orange"),
    ("Amarillo", "Yellow"),
    ("Verde", "Green"),
    ("Azul", "Blue"),
    ("Morado", "Purple"),
    ("Otro color", "Another color"),
    ("{n} %", "{n}%"),
    ("Escribe usuario@host[:puerto]", "Type user@host[:port]"),
    ("Dirección IPv6 mal formada", "Malformed IPv6 address"),
    ("Puerto no válido", "Invalid port"),
    ("Host o puerto no válidos", "Invalid host or port"),
    (
        "La conexión rápida no admite el puerto serie",
        "Quick connect does not support the serial port",
    ),
    ("Falta el usuario: usuario@host", "Missing user: user@host"),
    // --- Escritorio remoto (`src/ui/desktop_view.rs`) ---
    ("Conectando…", "Connecting…"),
    ("Sesión cerrada: {error}", "Session closed: {error}"),
    (
        "Sesión de escritorio remoto finalizada.",
        "Remote desktop session ended.",
    ),
    // --- Ventanas sueltas del servidor X (`src/ui/xwindows_view.rs`) ---
    ("Aplicación X", "X application"),
    (
        "Las aplicaciones gráficas se muestran cada una en su propia ventana.",
        "Each graphical application is shown in its own window.",
    ),
    (
        "Ahora mismo no hay ninguna abierta.",
        "None is open right now.",
    ),
    ("Hay 1 ventana abierta.", "There is 1 window open."),
    ("Hay {n} ventanas abiertas.", "There are {n} windows open."),
    (
        "Cerrar esta pestaña para el servidor X y cierra esas aplicaciones.",
        "Closing this tab stops the X server and closes those applications.",
    ),
    // --- Reproductor de grabaciones (`src/ui/cast_view.rs`) ---
    ("Pausa (Espacio)", "Pause (Space)"),
    ("Reproducir (Espacio)", "Play (Space)"),
    ("Volver al principio (Inicio)", "Back to the start (Home)"),
    (
        "Posición: arrastra para ir a cualquier punto (flechas: ±5 s)",
        "Position: drag to go to any point (arrows: ±5 s)",
    ),
    ("Velocidad", "Speed"),
    ("Saltar silencios", "Skip silences"),
    (
        "Ningún silencio dura más de {segundos} s",
        "No silence lasts longer than {segundos} s",
    ),
    ("La grabación está vacía", "The recording is empty"),
    (
        "⚠ 1 línea ilegible se ha saltado",
        "⚠ 1 unreadable line was skipped",
    ),
    (
        "⚠ {n} líneas ilegibles se han saltado",
        "⚠ {n} unreadable lines were skipped",
    ),
    ("⌛ avanzando…", "⌛ catching up…"),
    // --- Historial de órdenes (`src/ui/history_view.rs`) ---
    ("Historial de órdenes", "Command history"),
    ("(sin historial)", "(no history)"),
    (
        "Recordar las órdenes de este equipo",
        "Remember the commands of this host",
    ),
    (
        "Desmarcado, lo que ejecutes en este equipo no se anota (lo ya anotado sigue ahí hasta \
         que lo borres)",
        "Unchecked, what you run on this host is not recorded (what is already recorded stays \
         there until you delete it)",
    ),
    ("Buscar entre sus órdenes…", "Search its commands…"),
    ("sin órdenes", "no commands"),
    ("1 orden", "1 command"),
    ("{total} órdenes", "{total} commands"),
    ("{shown} de {total}", "{shown} of {total}"),
    (
        "Aún no se ha anotado ninguna orden de este equipo.",
        "No command from this host has been recorded yet.",
    ),
    (
        "Nada coincide con lo buscado.",
        "Nothing matches your search.",
    ),
    (
        "Borrar esta orden del historial",
        "Delete this command from the history",
    ),
    ("⌨ Escribir en el terminal", "⌨ Type in the terminal"),
    (
        "La deja escrita, sin ejecutarla (Intro, o doble clic)",
        "Leaves it typed, without running it (Enter, or double-click)",
    ),
    (
        "Elige una orden y abre un terminal conectado",
        "Choose a command and open a connected terminal",
    ),
    ("▶ Ejecutar", "▶ Run"),
    (
        "La escribe y pulsa Intro (Mayús+Intro)",
        "Types it and presses Enter (Shift+Enter)",
    ),
    ("📋 Copiar", "📋 Copy"),
    ("se envía a «{title}»", "sent to “{title}”"),
    (
        "no hay un terminal conectado a la vista",
        "no connected terminal in view",
    ),
    (
        "Recordar las órdenes que ejecuto",
        "Remember the commands I run",
    ),
    (
        "En todos los equipos, salvo los que se excluyan uno a uno",
        "On all hosts, except those excluded one by one",
    ),
    ("hasta", "up to"),
    ("por equipo", "per host"),
    (
        "🗑 Borrar el historial de este equipo",
        "🗑 Delete the history of this host",
    ),
    (
        "¿Borrar el de todos los equipos?",
        "Delete the history of all hosts?",
    ),
    ("Sí, borrar todo", "Yes, delete everything"),
    ("Borrar todo…", "Delete everything…"),
    (
        "Se anota lo que el terminal muestra al pulsar Intro, sin el indicador de la shell: una \
         contraseña tecleada sin eco no se guarda, y tampoco las respuestas a preguntas \
         («Usuario:», «¿Seguro? [s/N]») ni las órdenes que empiecen por un espacio. Si la shell \
         marca sus órdenes (OSC 133) solo se anotan las suyas; si no, también lo que se escriba \
         a otros programas con indicador propio, como mysql o python.",
        "What the terminal shows when Enter is pressed is recorded, without the shell prompt: a \
         password typed without echo is not saved, and neither are answers to questions \
         (“Username:”, “Are you sure? [y/N]”) or commands that start with a space. If the shell \
         marks its commands (OSC 133) only those are recorded; otherwise, so is whatever is \
         typed into other programs with a prompt of their own, such as mysql or python.",
    ),
    (
        "Los terminales locales comparten un mismo historial.",
        "Local terminals share a single history.",
    ),
    // --- Disparadores (`src/ui/triggers_view.rs`) ---
    ("Todos los terminales", "All terminals"),
    ("(equipo eliminado)", "(deleted host)"),
    (
        "No caben más de {max} reglas",
        "There is no room for more than {max} rules",
    ),
    ("Regla guardada", "Rule saved"),
    ("Disparadores", "Triggers"),
    (
        "Reglas que reaccionan cuando en la salida de un terminal aparece un texto: avisan y \
         marcan la pestaña o, si lo pides expresamente, contestan por ti.",
        "Rules that react when some text appears in the output of a terminal: they notify you \
         and mark the tab or, if you explicitly ask for it, answer for you.",
    ),
    ("Reglas ({n})", "Rules ({n})"),
    ("Aún no hay reglas.", "No rules yet."),
    ("Regla activa", "Rule enabled"),
    ("Cuando aparezca «{texto}»", "When “{texto}” appears"),
    ("⚠ ENVÍA TEXTO", "⚠ SENDS TEXT"),
    (
        "Escribe en el terminal sin preguntar:\n{texto}",
        "Types in the terminal without asking:\n{texto}",
    ),
    ("avisa 🔔", "notifies 🔔"),
    ("avisa", "notifies"),
    ("ámbito limitado", "limited scope"),
    ("➕ Nueva regla", "➕ New rule"),
    ("opcional", "optional"),
    ("Cuando aparezca", "When this appears"),
    ("texto que dispara la regla", "text that fires the rule"),
    ("Comodines", "Wildcards"),
    (
        "* vale por cualquier texto y ? por un carácter cualquiera; \\* y \\? son los signos tal \
         cual",
        "* stands for any text and ? for any single character; \\* and \\? are the signs \
         themselves",
    ),
    ("Distinguir mayúsculas", "Case sensitive"),
    ("En", "In"),
    ("Avisar", "Notify"),
    (
        "Un aviso en la barra de estado y una marca en la pestaña",
        "A notice in the status bar and a mark on the tab",
    ),
    ("con campana 🔔", "with bell 🔔"),
    (
        "La pestaña lleva la campana y, si GodRemote no está en primer plano, pide atención en \
         la barra de tareas",
        "The tab shows the bell and, if GodRemote is not in the foreground, it asks for \
         attention in the taskbar",
    ),
    (
        "⚠ Enviar un texto al terminal",
        "⚠ Send text to the terminal",
    ),
    (
        "La regla escribe en el terminal sin preguntar",
        "The rule types in the terminal without asking",
    ),
    (
        "Esta regla escribirá en el terminal por su cuenta cada vez que el texto aparezca en la \
         línea donde espera el cursor.",
        "This rule will type in the terminal on its own every time the text appears on the line \
         where the cursor is waiting.",
    ),
    (
        "Lo recibirá cualquier servidor o programa que escriba ese texto, no solo el que tienes \
         en mente. No pongas contraseñas: se guardan sin cifrar con los ajustes.",
        "Any server or program that writes that text will receive it, not just the one you have \
         in mind. Do not put passwords here: they are stored unencrypted with the settings.",
    ),
    ("Sí, que envíe texto", "Yes, let it send text"),
    (
        "Respuesta; cada salto de línea es Intro",
        "Answer; each line break is Enter",
    ),
    (
        "Como en los snippets: un salto de línea es Intro; \\e, \\t y \\x03 son teclas \
         especiales. Solo se envía si el texto está en la línea donde espera el cursor y lleva \
         un momento quieta; nunca a lo que pasa de largo en un listado ni a una aplicación a \
         pantalla completa.",
        "As in snippets: a line break is Enter; \\e, \\t and \\x03 are special keys. It is only \
         sent if the text is on the line where the cursor is waiting and that line has been \
         still for a moment; never to what scrolls past in a listing or to a full-screen \
         application.",
    ),
    ("No volver a dispararse en", "Do not fire again for"),
    (
        "Enfriamiento: evita que la regla se dispare en bucle",
        "Cooldown: keeps the rule from firing in a loop",
    ),
    ("en el mismo terminal", "in the same terminal"),
    ("Probar con", "Test with"),
    ("una línea de ejemplo", "a sample line"),
    ("✔ se dispara", "✔ fires"),
    ("no se dispara", "does not fire"),
    ("falta el texto", "the text is missing"),
    ("Regla eliminada", "Rule deleted"),
    ("Órdenes largas", "Long commands"),
    (
        "Avisar cuando termine una orden que dure más de",
        "Notify when a command lasting more than",
    ),
    (
        "en una pestaña que no estoy mirando",
        "finishes in a tab I am not looking at",
    ),
    (
        "Si la shell marca sus órdenes (integración de shell, OSC 133: fish, PowerShell en \
         Windows Terminal, o bash y zsh con la integración de kitty, WezTerm, iTerm2…) el aviso \
         es exacto y dice si terminó con error. Si no, se deduce: se pulsó Intro, hubo salida \
         durante ese tiempo y después silencio con el cursor en algo que parece un indicador o \
         una pregunta.",
        "If the shell marks its commands (shell integration, OSC 133: fish, PowerShell in \
         Windows Terminal, or bash and zsh with the integration of kitty, WezTerm, iTerm2…) the \
         notice is exact and says whether it ended with an error. Otherwise it is inferred: \
         Enter was pressed, there was output during that time and then silence with the cursor \
         on something that looks like a prompt or a question.",
    ),
    // --- Herramientas de los terminales (`src/ui/term_tools.rs`) ---
    ("Terminales locales", "Local terminals"),
    (
        "Historial de órdenes de este equipo",
        "Command history of this host",
    ),
    ("🎬 Grabando", "🎬 Recording"),
    (
        "Grabando esta sesión, con sus tiempos, en\n{ruta}\nPulsa para terminar la grabación.",
        "Recording this session, with its timing, to\n{ruta}\nClick to end the recording.",
    ),
    ("🎬 Grabar", "🎬 Record"),
    (
        "Grabar esta sesión con sus tiempos (asciicast, .cast) para reproducirla después",
        "Record this session with its timing (asciicast, .cast) to play it back later",
    ),
    (
        "Grabación guardada en {ruta} (se reproduce desde «Herramientas»)",
        "Recording saved to {ruta} (play it back from “Tools”)",
    ),
    ("Grabando en {ruta}", "Recording to {ruta}"),
    (
        "No se pudo grabar la sesión: {error}",
        "Could not record the session: {error}",
    ),
    (
        "{tab}: el disparador «{nombre}» se ha disparado demasiadas veces seguidas y queda en \
         pausa en esta pestaña",
        "{tab}: the trigger “{nombre}” has fired too many times in a row and is paused in this tab",
    ),
    (
        "⚠ {tab}: el disparador «{nombre}» ha contestado a «{linea}»",
        "⚠ {tab}: the trigger “{nombre}” has answered “{linea}”",
    ),
    (
        "🔔 {tab}: {nombre} — «{linea}»",
        "🔔 {tab}: {nombre} — “{linea}”",
    ),
    (
        "{tab}: la orden ha terminado con error (código {code}) tras {time}",
        "{tab}: the command finished with an error (code {code}) after {time}",
    ),
    (
        "{tab}: la orden ha terminado tras {time}",
        "{tab}: the command finished after {time}",
    ),
    (
        "{tab}: parece que la orden ha terminado (o espera una respuesta) tras {time}",
        "{tab}: the command seems to have finished (or is waiting for an answer) after {time}",
    ),
    (
        "No se pudo guardar el historial de órdenes: {error}",
        "Could not save the command history: {error}",
    ),
    ("{stored} (eliminado)", "{stored} (deleted)"),
    (
        "Abre un terminal conectado para enviarle la orden",
        "Open a connected terminal to send it the command",
    ),
    ("Historial", "History"),
    ("Reproducir una grabación", "Play a recording"),
    (
        "Grabaciones de terminal (asciicast)",
        "Terminal recordings (asciicast)",
    ),
    ("Todos los ficheros", "All files"),
    ("🛠 Herramientas", "🛠 Tools"),
    ("🔔 Disparadores…", "🔔 Triggers…"),
    ("🔔 Disparadores… (1 activo)", "🔔 Triggers… (1 active)"),
    (
        "🔔 Disparadores… ({n} activos)",
        "🔔 Triggers… ({n} active)",
    ),
    (
        "Avisar o contestar cuando aparezca un texto en un terminal, y avisar cuando termine una \
         orden larga",
        "Notify or answer when some text appears in a terminal, and notify when a long command \
         finishes",
    ),
    ("🕘 Historial de órdenes…", "🕘 Command history…"),
    (
        "Las órdenes ejecutadas en cada equipo, para buscarlas y volver a enviarlas",
        "The commands run on each host, to search them and send them again",
    ),
    (
        "⏹ Terminar la grabación de esta sesión",
        "⏹ End the recording of this session",
    ),
    (
        "🎬 Grabar esta sesión (.cast)",
        "🎬 Record this session (.cast)",
    ),
    ("Abre un terminal", "Open a terminal"),
    ("▶ Reproducir una grabación…", "▶ Play a recording…"),
    ("Últimas grabaciones", "Latest recordings"),
    (
        "📂 Abrir la carpeta de las grabaciones",
        "📂 Open the recordings folder",
    ),
    (
        "Disparadores, historial de órdenes y grabaciones de los terminales",
        "Triggers, command history and terminal recordings",
    ),
    (
        "No se pudo abrir {ruta}: {error}",
        "Could not open {ruta}: {error}",
    ),
];
