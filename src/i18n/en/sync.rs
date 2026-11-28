//! (Véase `mod.rs`.) Sincronización entre equipos y carpetas compartidas.
//!
//! Aquí «equipo» es casi siempre el ordenador del usuario (*computer*); los
//! equipos del árbol siguen siendo *hosts*.

pub(super) const T: super::Table = &[
    // --- src/sync.rs ---
    (
        "El fichero está incompleto (quizá se está copiando)",
        "The file is incomplete (it may still be copying)",
    ),
    (
        "La contraseña no es la de este fichero",
        "The password is not the one for this file",
    ),
    (
        "El fichero está dañado o no es de GodRemote (no se ha tocado nada)",
        "The file is damaged or is not a GodRemote file (nothing has been touched)",
    ),
    (
        "El fichero es de una versión más reciente de GodRemote: actualiza este equipo",
        "The file comes from a newer version of GodRemote: update this computer",
    ),
    ("El fichero es demasiado grande", "The file is too large"),
    ("No se pudo cifrar: {e}", "Could not encrypt: {e}"),
    ("fallo al cifrar", "encryption failed"),
    ("no es un fichero", "it is not a file"),
    ("es demasiado grande", "it is too large"),
    (
        "no se pudo preparar un elemento: {e}",
        "could not prepare an item: {e}",
    ),
    (
        "{what} no se puede leer ({e}); no se sincroniza para no propagar el fallo",
        "{what} cannot be read ({e}); not syncing, so the problem does not spread",
    ),
    (
        "sessions.json tiene dos elementos con el mismo identificador ({id}); no se sincroniza",
        "sessions.json has two items with the same identifier ({id}); not syncing",
    ),
    ("no es un baúl válido", "it is not a valid vault"),
    (
        "no se pudo escribir {what}: {e}",
        "could not write {what}: {e}",
    ),
    ("un tema", "a theme"),
    (
        "El tema «{name}» no se puede usar en esta versión y se ha dejado como está",
        "The theme “{name}” cannot be used in this version and has been left as it is",
    ),
    (
        "los ajustes no son un objeto",
        "the settings are not an object",
    ),
    (
        "Ajustes que esta versión no entiende y se han dejado como estaban: {lista}",
        "Settings this version does not understand, left as they were: {lista}",
    ),
    (
        "los ajustes recibidos no son válidos: {e}",
        "the settings received are not valid: {e}",
    ),
    (
        "El baúl recibido no es válido y se ha conservado el de este equipo",
        "The vault received is not valid, so this computer's has been kept",
    ),
    ("carpeta", "folder"),
    ("nombre", "name"),
    ("puerto", "port"),
    ("usuario", "user"),
    ("autenticación", "authentication"),
    ("clave privada", "private key"),
    ("certificado", "certificate"),
    ("protocolo", "protocol"),
    ("saltos", "jump hosts"),
    ("túneles", "tunnels"),
    ("notas", "notes"),
    ("órdenes al conectar", "commands on connect"),
    ("valores para sus equipos", "values for its hosts"),
    ("texto", "text"),
    ("atajo", "shortcut"),
    ("equipo", "host"),
    ("túnel", "tunnel"),
    ("arranque automático", "automatic start"),
    ("fondo", "background"),
    ("color", "color"),
    ("tema", "theme"),
    ("tema automatico", "automatic theme"),
    ("tema claro", "light theme"),
    ("tema oscuro", "dark theme"),
    ("barra servidor", "server info bar"),
    ("rdp integrado", "built-in RDP"),
    ("reconectar", "reconnect"),
    ("reintentos", "reconnection attempts"),
    ("restaurar pestanas", "restore tabs"),
    ("avisar actividad", "activity notice"),
    ("resaltado", "highlighting"),
    ("bloqueo baul", "vault lock"),
    ("atajos", "shortcuts"),
    ("confirmar pegado", "confirm paste"),
    ("confirmar cierre", "confirm close"),
    ("avisar versiones", "new version notice"),
    ("idioma", "language"),
    ("panel estado", "status dashboard"),
    ("disparadores", "triggers"),
    ("historial", "command history"),
    ("sí", "yes"),
    ("no", "no"),
    ("(nada)", "(nothing)"),
    ("Equipo «{name}»", "Host “{name}”"),
    ("Snippet «{name}»", "Snippet “{name}”"),
    ("Túnel «{name}»", "Tunnel “{name}”"),
    ("Tema «{name}»", "Theme “{name}”"),
    ("Ajuste «{name}»", "Setting “{name}”"),
    ("Baúl de credenciales", "Credential vault"),
    ("Elemento «{key}»", "Item “{key}”"),
    (
        "Se han guardado o cambiado credenciales en los dos lados.",
        "Credentials have been saved or changed on both sides.",
    ),
    (
        "Borrado en este equipo y modificado en la carpeta.",
        "Deleted on this computer and changed in the folder.",
    ),
    (
        "Modificado en este equipo y borrado en la carpeta.",
        "Changed on this computer and deleted in the folder.",
    ),
    (
        "Aquí: {mine} · En la carpeta: {theirs}",
        "Here: {mine} · In the folder: {theirs}",
    ),
    ("Distinto en los dos lados.", "Different on both sides."),
    (
        "{label}: aquí «{mine}», en la carpeta «{theirs}»",
        "{label}: “{mine}” here, “{theirs}” in the folder",
    ),
    (
        "{label}: distinto en los dos lados",
        "{label}: different on both sides",
    ),
    (
        "tiene {n} elementos (el máximo es {max})",
        "it has {n} items (the maximum is {max})",
    ),
    (
        "tiene claves no admitidas",
        "it has keys that are not allowed",
    ),
    (
        "La carpeta de sincronización no está disponible ({e})",
        "The sync folder is not available ({e})",
    ),
    (
        "La contraseña de sincronización no es la de esta carpeta",
        "The sync password is not the one for this folder",
    ),
    (
        "Lo que hay en la carpeta no es válido ({e}); no se ha tocado nada",
        "What is in the folder is not valid ({e}); nothing has been touched",
    ),
    (
        "Otro equipo ha escrito en la carpeta a la vez; se vuelve a intentar",
        "Another computer wrote to the folder at the same time; trying again",
    ),
    (
        "No se pudo escribir en la carpeta ({e})",
        "Could not write to the folder ({e})",
    ),
    ("no es una carpeta", "it is not a folder"),
    ("{carpeta} no existe", "{carpeta} does not exist"),
    (
        "la configuración es demasiado grande para sincronizarla",
        "the configuration is too large to sync",
    ),
    (
        "no se pudo guardar la copia previa ({e}); no se ha cambiado nada",
        "could not save the pre-sync backup ({e}); nothing has been changed",
    ),
    (
        "{n} elemento(s) se han dejado en la raíz: su carpeta se borró o se movió en otro equipo",
        "{n} item(s) have been left at the root: their folder was deleted or moved on another computer",
    ),
    (
        "no se pudo guardar el estado: {e}",
        "could not save the state: {e}",
    ),
    // --- src/share.rs ---
    ("formato desconocido", "unknown format"),
    ("no se entiende: {e}", "it cannot be understood: {e}"),
    (
        "El fichero compartido no está disponible ({e})",
        "The shared file is not available ({e})",
    ),
    (
        "La contraseña no es la de esta carpeta compartida",
        "The password is not the one for this shared folder",
    ),
    (
        "El fichero compartido no es válido ({e}); no se ha tocado nada",
        "The shared file is not valid ({e}); nothing has been touched",
    ),
    (
        "El fichero compartido es más antiguo (revisión {found}) que el último visto (revisión {seen}); se conserva lo que había",
        "The shared file is older (revision {found}) than the last one seen (revision {seen}); what was there is kept",
    ),
    (
        "No se pudo escribir el fichero compartido ({e})",
        "Could not write the shared file ({e})",
    ),
    (
        "{n} equipo(s) traían órdenes al conectar, túneles o reenvíos, que GodRemote no publica: se han ignorado (¿seguro que el fichero es de quien dice?)",
        "{n} host(s) came with commands on connect, tunnels or forwarding, which GodRemote does not publish: they have been ignored (are you sure the file is from who it claims to be?)",
    ),
    (
        "{n} equipo(s) se han dejado fuera (puerto serie, sin dirección o repetidos)",
        "{n} host(s) have been left out (serial port, no address or duplicates)",
    ),
    (
        "{n} salto(s) pasan por equipos que no están en la carpeta: esos equipos no conectarán hasta que los dupliques y les pongas tu salto",
        "{n} jump host(s) go through hosts that are not in the folder: those hosts will not connect until you duplicate them and give them your own jump host",
    ),
    (
        "tiene más de {max} niveles de subcarpetas",
        "it has more than {max} levels of subfolders",
    ),
    (
        "tiene más de {max} equipos y carpetas",
        "it has more than {max} hosts and folders",
    ),
    (
        "es demasiado grande para compartirla",
        "it is too large to share",
    ),
    ("no existe", "does not exist"),
    (
        "la carpeta ya no existe en este equipo",
        "the folder no longer exists on this computer",
    ),
    (
        "{path} es una carpeta: indica el nombre del fichero",
        "{path} is a folder: give the name of the file",
    ),
    (
        "Ya hay otro fichero en {path}; elige otro nombre o bórralo antes",
        "There is already another file at {path}; choose another name or delete it first",
    ),
    ("{n} equipo(s) nuevos", "{n} new host(s)"),
    ("{n} quitados", "{n} removed"),
    (
        "{n} con otra dirección ({lista})",
        "{n} with a different address ({lista})",
    ),
    ("«{name}»: {old} ➡ {new}", "“{name}”: {old} ➡ {new}"),
    (
        "{n} elemento(s) tuyos que estaban dentro han pasado a la raíz",
        "{n} item(s) of yours that were inside have been moved to the root",
    ),
    ("sin cambios en sus equipos", "no changes to its hosts"),
    (
        "En esa ruta hay ahora otra publicación (se ha vuelto a publicar desde cero)",
        "There is now a different publication at that path (it has been published again from scratch)",
    ),
    ("👥 {name} (compartida)", "👥 {name} (shared)"),
    // --- src/vault.rs (lo añadido para la sincronización) ---
    ("El otro baúl no es válido", "The other vault is not valid"),
    (
        "El otro baúl tiene otra contraseña maestra",
        "The other vault has a different master password",
    ),
    (
        "La contraseña maestra del otro baúl no es esa",
        "That is not the other vault's master password",
    ),
    ("el otro baúl está dañado", "the other vault is damaged"),
    // --- Ventana «Sincronización» (`src/ui/sync_view.rs`) ---
    (
        "No se pudo guardar la configuración: {e}",
        "Could not save the configuration: {e}",
    ),
    (
        "No se pudo guardar el estado: {e}",
        "Could not save the state: {e}",
    ),
    (
        "Sincronización: falta la contraseña",
        "Sync: password missing",
    ),
    ("Sincronizando…", "Syncing…"),
    (
        "Sincronización: {conflicts} conflicto(s)",
        "Sync: {conflicts} conflict(s)",
    ),
    ("Sincronizado", "Synced"),
    ("Sincronización pendiente", "Sync pending"),
    ("Sincronización: sin carpeta", "Sync: folder unavailable"),
    (
        "Sincronización: contraseña incorrecta",
        "Sync: wrong password",
    ),
    ("Sincronización: error", "Sync: error"),
    ("contraseña de «{owner}»", "password of “{owner}”"),
    ("passphrase de «{owner}»", "passphrase of “{owner}”"),
    ("secreto TOTP de «{owner}»", "TOTP secret of “{owner}”"),
    ("secreto de «{owner}»", "secret of “{owner}”"),
    (
        "Sincronización de la configuración entre tus equipos (pulsa para abrirla)",
        "Configuration sync between your computers (click to open it)",
    ),
    ("el editor de equipos", "the host editor"),
    ("el editor de carpetas", "the folder editor"),
    ("la ventana de snippets", "the snippets window"),
    ("la ventana de temas", "the themes window"),
    ("el gestor de túneles", "the tunnel manager"),
    ("un diálogo", "a dialog"),
    (
        "No se pudo guardar en el baúl: {e}",
        "Could not save to the vault: {e}",
    ),
    (
        "el llavero no está disponible",
        "the keyring is not available",
    ),
    (
        "No se pudieron leer las sesiones: {e}",
        "Could not read the sessions: {e}",
    ),
    (
        "El baúl ha cambiado en otro equipo: ábrelo con su contraseña maestra",
        "The vault has changed on another computer: unlock it with its master password",
    ),
    (
        "La sincronización ha traído el baúl de otro equipo: ya puedes abrirlo",
        "Sync has brought the vault from another computer: you can now unlock it",
    ),
    (
        "A la espera de que cierres {what} para sincronizar",
        "Waiting for you to close {what} before syncing",
    ),
    (
        "Claves privadas que no están en este equipo (se avisará al conectar): {lista}",
        "Private keys that are not on this computer (you will be told when connecting): {lista}",
    ),
    (
        "Sincronizado: {recibidos} cambio(s) recibidos y {enviados} enviados",
        "Synced: {recibidos} change(s) received and {enviados} sent",
    ),
    (
        "Antes de aplicar los cambios se guardó una copia en {ruta}",
        "Before applying the changes a backup was saved to {ruta}",
    ),
    (
        "Sincronización: {conflicts} conflicto(s) por resolver (ábrela desde la barra de estado)",
        "Sync: {conflicts} conflict(s) to resolve (open it from the status bar)",
    ),
    ("Configuración sincronizada", "Configuration synced"),
    (
        "Baúl combinado con el de la carpeta: {added} secreto(s) añadidos o cambiados y {removed} quitados",
        "Vault combined with the folder's: {added} secret(s) added or changed and {removed} removed",
    ),
    ("Sincronización", "Sync"),
    (
        "Guarda tu configuración, cifrada, en una carpeta que ya repliques entre tus equipos (Dropbox, OneDrive, Syncthing, una unidad de red…) y la mantiene al día en todos.",
        "Saves your configuration, encrypted, in a folder you already replicate between your computers (Dropbox, OneDrive, Syncthing, a network drive…) and keeps it up to date on all of them.",
    ),
    (
        "Sincronizar la configuración de este equipo",
        "Sync this computer's configuration",
    ),
    (
        "carpeta que ya se replica entre tus equipos",
        "folder already replicated between your computers",
    ),
    ("Carpeta de sincronización", "Sync folder"),
    ("Este equipo", "This computer"),
    (
        "nombre con el que lo verán los demás",
        "name the others will see it by",
    ),
    ("Cada", "Every"),
    (
        "También se sincroniza al arrancar y a los pocos segundos de cambiar algo aquí o en la carpeta",
        "It also syncs at startup and a few seconds after something changes here or in the folder",
    ),
    (
        "(puesta; escribe otra para cambiarla)",
        "(set; type to change it)",
    ),
    ("contraseña de sincronización", "sync password"),
    ("Escribe la contraseña dos veces", "Type the password twice"),
    (
        "Falta la contraseña de sincronización: escríbela (la misma en todos tus equipos).",
        "The sync password is missing: type it (the same one on all your computers).",
    ),
    (
        "Falta la contraseña de sincronización: abre el baúl, donde se guarda, o escríbela.",
        "The sync password is missing: unlock the vault, where it is stored, or type it.",
    ),
    (
        "🔐 La contraseña está guardada en el baúl.",
        "🔐 The password is stored in the vault.",
    ),
    (
        "La contraseña está solo en memoria: con el baúl cerrado no se guarda y habrá que escribirla al volver a abrir GodRemote.",
        "The password is only in memory: with the vault locked it is not saved, and you will have to type it when you open GodRemote again.",
    ),
    ("Desactivada", "Disabled"),
    (
        "{n} conflicto(s) por resolver",
        "{n} conflict(s) to resolve",
    ),
    ("Sin sincronizar todavía", "Not synced yet"),
    ("Hay cambios por sincronizar", "There are changes to sync"),
    ("{e}. Se reintentará.", "{e}. It will be retried."),
    (
        "La contraseña no es la de esta carpeta",
        "The password is not the one for this folder",
    ),
    ("Estado:", "Status:"),
    ("🔄 Sincronizar ahora", "🔄 Sync now"),
    (
        "Actívala e indica la carpeta y la contraseña",
        "Enable it and give the folder and the password",
    ),
    (
        "Última: {fecha} · revisión {revision}",
        "Last: {fecha} · revision {revision}",
    ),
    (
        "Se sustituirá lo que haya en la carpeta por la configuración de este equipo, cifrada con la contraseña de ahora. Los demás equipos la fusionarán con la suya.",
        "Whatever is in the folder will be replaced with this computer's configuration, encrypted with the current password. The other computers will merge it with theirs.",
    ),
    ("Sí, reescribir", "Yes, rewrite"),
    (
        "Reescribir la carpeta con lo de este equipo…",
        "Rewrite the folder with what this computer has…",
    ),
    (
        "Para cuando el fichero de la carpeta está dañado o se quiere cambiar la contraseña",
        "For when the file in the folder is damaged or you want to change the password",
    ),
    (
        "{nombre} · este (revisión {revision}, {visto})",
        "{nombre} · this one (revision {revision}, {visto})",
    ),
    (
        "{nombre} (revisión {revision}, {visto})",
        "{nombre} (revision {revision}, {visto})",
    ),
    ("Equipos vistos:", "Computers seen:"),
    (
        "El servicio que replica la carpeta ha dejado {n} copia(s) «en conflicto». No hacen falta: cada equipo vuelve a enviar lo suyo.",
        "The service that replicates the folder has left {n} “conflicted” copy(ies). They are not needed: each computer sends its own changes again.",
    ),
    ("Borrarlas", "Delete them"),
    (
        "Conflictos por resolver ({n})",
        "Conflicts to resolve ({n})",
    ),
    (
        "cambiados aquí y en otro equipo de forma distinta",
        "changed differently here and on another computer",
    ),
    ("Quedarme con todo lo mío", "Keep all of mine"),
    (
        "Quedarme con todo lo de la carpeta",
        "Keep all of the folder's",
    ),
    ("Lo mío", "Mine"),
    ("Lo de la carpeta", "The folder's"),
    (
        "Mientras no elijas, cada lado conserva lo suyo. Antes de cambiar nada en este equipo se guarda una copia de su configuración.",
        "Until you choose, each side keeps its own. Before anything is changed on this computer, a backup of its configuration is saved.",
    ),
    ("Últimos avisos", "Latest notices"),
    (
        "Se sincronizan los equipos y sus carpetas, los snippets, los túneles, los temas, los ajustes y el baúl (tal cual, cifrado con su contraseña maestra). No: las claves privadas (solo su ruta), los registros, las grabaciones, el historial de órdenes ni los ajustes propios de cada equipo (editor, shell local, carpeta de registros, proxy, servidor X).",
        "Hosts and their folders, snippets, tunnels, themes, settings and the vault (as it is, encrypted with its master password) are synced. Not synced: private keys (only their path), logs, recordings, command history and the settings that belong to each computer (editor, local shell, log folder, proxy, X server).",
    ),
    (
        "Abrir la carpeta de las copias previas",
        "Open the folder with the pre-sync backups",
    ),
    (
        "Los ficheros «antes-de-sincronizar-…» se restauran desde «Importar y copias de seguridad»",
        "The “antes-de-sincronizar-…” files are restored from “Import and backups”",
    ),
    (
        "Contraseña guardada en el baúl",
        "Password saved in the vault",
    ),
    (
        "Contraseña puesta (solo en memoria: el baúl está cerrado)",
        "Password set (only in memory: the vault is locked)",
    ),
    (
        "{removed} copia(s) en conflicto borradas",
        "{removed} conflicted copy(ies) deleted",
    ),
    (
        "Se pueden combinar: de la carpeta entrarían {added} secreto(s) y se quitarían {removed} que se borraron allí.",
        "They can be combined: {added} secret(s) would come in from the folder and {removed} that were deleted there would be removed.",
    ),
    ("Combinar los dos baúles", "Combine the two vaults"),
    (
        "Con valores distintos en los dos: {lista}",
        "With different values in each: {lista}",
    ),
    ("Combinar; en esos, lo mío", "Combine; for those, mine"),
    (
        "Combinar; en esos, lo de la carpeta",
        "Combine; for those, the folder's",
    ),
    (
        "Contraseña maestra del baúl de la carpeta:",
        "Master password of the folder's vault:",
    ),
    (
        "Con ella se pueden combinar los dos baúles secreto a secreto.",
        "With it the two vaults can be combined secret by secret.",
    ),
    (
        "Abre el baúl para combinarlos secreto a secreto, sin perder nada.",
        "Unlock the vault to combine them secret by secret, without losing anything.",
    ),
    ("Desbloquear…", "Unlock…"),
    (
        "Vuelve a sincronizar para ver cómo combinarlos.",
        "Sync again to see how to combine them.",
    ),
    (
        "O uno entero (el otro queda en la copia previa):",
        "Or one of them whole (the other one stays in the pre-sync backup):",
    ),
    ("Mi baúl", "My vault"),
    ("El de la carpeta", "The folder's"),
    // --- Ventana «Carpetas compartidas» (`src/ui/share_view.rs`) ---
    ("repítela", "repeat it"),
    ("Usar", "Use"),
    ("Al día", "Up to date"),
    ("Cierra antes {what}", "Close {what} first"),
    (
        "No se pudo guardar shares.json: {e}",
        "Could not save shares.json: {e}",
    ),
    (
        "Es de una carpeta compartida, de solo lectura. Duplícalo para tener una copia tuya que puedas cambiar.",
        "It belongs to a shared folder, which is read-only. Duplicate it to get a copy of your own that you can change.",
    ),
    (
        "Las carpetas compartidas no se reorganizan: son de quien las publica",
        "Shared folders cannot be rearranged: they belong to whoever publishes them",
    ),
    (
        "No se puede crear nada dentro de una carpeta compartida",
        "Nothing can be created inside a shared folder",
    ),
    ("{name} (copia)", "{name} (copy)"),
    (
        "Copia creada fuera de la carpeta compartida: ya es tuya y la puedes cambiar",
        "Copy created outside the shared folder: it is yours now and you can change it",
    ),
    (
        "Se ha olvidado la contraseña guardada de {forgotten} equipo(s) compartidos cuya dirección ha cambiado: se te pedirá al conectar",
        "The saved password of {forgotten} shared host(s) whose address has changed has been forgotten: you will be asked for it when connecting",
    ),
    (
        "{equipos} ya no está(n) en la carpeta compartida «{name}»: sus pestañas siguen abiertas, pero no se volverán a abrir solas",
        "No longer in the shared folder “{name}”: {equipos}. Their tabs stay open, but will not be reopened automatically",
    ),
    (
        "Carpeta compartida «{name}»: {cambios}",
        "Shared folder “{name}”: {cambios}",
    ),
    (
        "Al día. Al abrir el baúl se olvidará la contraseña guardada de los equipos que han cambiado de dirección",
        "Up to date. When the vault is unlocked, the saved password of the hosts whose address has changed will be forgotten",
    ),
    (
        "{rescued} elemento(s) que estaban dentro de una carpeta compartida han pasado a la raíz: esa carpeta es de quien la publica",
        "{rescued} item(s) that were inside a shared folder have been moved to the root: that folder belongs to whoever publishes it",
    ),
    ("llavero no disponible", "keyring not available"),
    (
        "{e}. Se usa la última copia y se reintentará.",
        "{e}. The last copy is being used and it will be retried.",
    ),
    (
        "Publicada: {n} equipo(s), revisión {revision}",
        "Published: {n} host(s), revision {revision}",
    ),
    (
        "{n} salto(s) pasan por equipos de fuera de la carpeta: a quien la siga no le funcionarán",
        "{n} jump host(s) go through hosts outside the folder: they will not work for whoever follows it",
    ),
    (
        "{n} equipo(s) no se publican (puerto serie o sin dirección)",
        "{n} host(s) are not published (serial port or no address)",
    ),
    (
        "Carpeta publicada. Da a tus compañeros la ruta del fichero y la contraseña.",
        "Folder published. Give your teammates the path to the file and the password.",
    ),
    (
        "Esa carpeta la publicas tú desde este equipo",
        "You publish that folder yourself from this computer",
    ),
    ("Ya sigues esa carpeta", "You already follow that folder"),
    (
        "Ya tienes en el árbol una carpeta con ese identificador",
        "You already have a folder with that identifier in the tree",
    ),
    (
        "Ahora sigues «{name}»: {count} equipo(s). Está en el árbol, marcada como compartida.",
        "You now follow “{name}”: {count} host(s). It is in the tree, marked as shared.",
    ),
    (
        "Ya no sigues la carpeta: sus equipos se quedan como una carpeta tuya",
        "You no longer follow the folder: its hosts stay as a folder of your own",
    ),
    (
        "Ya no sigues la carpeta: se ha quitado del árbol",
        "You no longer follow the folder: it has been removed from the tree",
    ),
    ("Carpetas compartidas", "Shared folders"),
    (
        "Carpetas de compañeros que sigo",
        "Teammates' folders I follow",
    ),
    (
        "Aparecen en el árbol, de solo lectura, y se actualizan solas. Puedes conectar a sus equipos, duplicarlos a una carpeta tuya y usar tu usuario y tu clave sin tocar lo publicado.",
        "They appear in the tree, read-only, and update themselves. You can connect to their hosts, duplicate them into a folder of your own and use your own user and key without touching what is published.",
    ),
    ("(ninguna todavía)", "(none yet)"),
    (
        "de {autor} · revisión {revision} · publicada el {fecha}",
        "by {autor} · revision {revision} · published on {fecha}",
    ),
    ("Mi usuario", "My user"),
    ("el publicado", "as published"),
    ("Mi autenticación", "My authentication"),
    ("La publicada", "As published"),
    (
        "Tu usuario y tu clave para los equipos SSH de esta carpeta. No cambia nada para los demás",
        "Your user and your key for the SSH hosts in this folder. Nothing changes for anyone else",
    ),
    ("🔄 Actualizar ahora", "🔄 Update now"),
    (
        "¿Qué hago con sus equipos?",
        "What should I do with its hosts?",
    ),
    ("Quitarlos", "Remove them"),
    ("Dejarlos como carpeta mía", "Keep them as a folder of mine"),
    (
        "Se quedan como están ahora, ya no se actualizan y puedes cambiarlos",
        "They stay as they are now, no longer update, and you can change them",
    ),
    ("Dejar de seguir…", "Unfollow…"),
    ("Seguir otra:", "Follow another:"),
    (
        "fichero publicado por un compañero",
        "file published by a teammate",
    ),
    ("Fichero de la carpeta compartida", "Shared folder file"),
    ("Carpeta de GodRemote", "GodRemote folder"),
    ("contraseña", "password"),
    ("Suscribirme", "Subscribe"),
    ("Carpetas mías que publico", "Folders of mine I publish"),
    (
        "Se publican los equipos de la carpeta y de sus subcarpetas con su dirección, usuario, ruta de la clave, saltos, notas y colores. No se publican contraseñas ni nada del baúl, ni las órdenes al conectar, los túneles, los reenvíos del agente o de X11, ni los equipos de puerto serie.",
        "The hosts in the folder and its subfolders are published with their address, user, key path, jump hosts, notes and colors. Passwords and anything from the vault are not published, nor are commands on connect, tunnels, agent or X11 forwarding, or serial port hosts.",
    ),
    (
        "(la carpeta ya no existe en este equipo)",
        "(the folder no longer exists on this computer)",
    ),
    (
        "revisión {revision} · publicada el {fecha}",
        "revision {revision} · published on {fecha}",
    ),
    (
        "Volver a publicarla sola cuando cambie",
        "Publish it again automatically when it changes",
    ),
    ("Publicar ahora", "Publish now"),
    ("Dejar de publicar", "Stop publishing"),
    (
        "El fichero ya publicado no se borra: quien lo siga conserva lo último. Bórralo tú si quieres retirarlo",
        "The file already published is not deleted: whoever follows it keeps the latest version. Delete it yourself if you want to withdraw it",
    ),
    ("Publicar otra:", "Publish another:"),
    ("(elige una)", "(choose one)"),
    ("Fichero", "File"),
    (
        "en una carpeta a la que lleguen tus compañeros",
        "in a folder your teammates can reach",
    ),
    ("Dónde publicar la carpeta", "Where to publish the folder"),
    (
        "la que darás a tus compañeros",
        "the one for your teammates",
    ),
    ("Publicar", "Publish"),
    (
        "Elige la carpeta y el fichero, y escribe la contraseña dos veces",
        "Choose the folder and the file, and type the password twice",
    ),
    (
        "Las contraseñas de las carpetas se guardan en el baúl; con el baúl cerrado se piden y solo duran hasta cerrar GodRemote. Quien tenga la contraseña y pueda escribir en la ruta puede sustituir lo publicado: compártela solo con quien deba publicar o leer.",
        "The folders' passwords are stored in the vault; with the vault locked you are asked for them and they only last until GodRemote is closed. Anyone who has the password and can write to the path can replace what is published: share it only with those who should publish or read.",
    ),
    ("Leyendo la carpeta…", "Reading the folder…"),
    ("Falta su contraseña:", "Password missing:"),
    (
        "Falta su contraseña (abre el baúl, donde se guarda, o escríbela):",
        "Password missing (unlock the vault, where it is kept, or type it):",
    ),
    // --- Textos que también están en otras tablas (traducidos igual) ---
    ("clave no válida", "invalid key"),
    ("ruta sin nombre de fichero", "path without a file name"),
    ("no es válido", "not valid"),
    ("leyendo {path}", "reading {path}"),
    ("(sin nombre)", "(unnamed)"),
    ("Carpeta «{name}»", "Folder “{name}”"),
    ("Carpeta", "Folder"),
    ("Contraseña", "Password"),
    ("Elegir…", "Choose…"),
    ("No", "No"),
    ("Abrir", "Open"),
    (
        "No se pudo abrir {ruta}: {error}",
        "Could not open {ruta}: {error}",
    ),
    ("Clave privada", "Private key"),
    ("Agente SSH", "SSH agent"),
    ("Aplicar", "Apply"),
    ("Cancelar", "Cancel"),
];
