//! Sincronización entre dos «equipos»: dos carpetas de configuración y una
//! carpeta de sincronización temporal que hace de Dropbox.

use std::path::{Path, PathBuf};

use godremote::macros::MacroStore;
use godremote::model::{Node, Session, Tree};
use godremote::sync::{
    self, Expect, Keyring, Outcome, Remote, SYNC_FILE, SealError, Side, SyncConfig, SyncError,
    SyncState,
};
use godremote::theme::{Settings, Theme, ThemeStore};
use godremote::tunnel_manager::{SavedTunnel, TunnelStore};
use godremote::vault::Vault;
use uuid::Uuid;

const PASSWORD: &str = "contraseña de sincronización";

struct Device {
    home: tempfile::TempDir,
    config: SyncConfig,
    keys: Keyring,
}

impl Device {
    fn new(name: &str, folder: &Path) -> Self {
        Self::with_password(name, folder, PASSWORD)
    }

    fn with_password(name: &str, folder: &Path, password: &str) -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            config: SyncConfig {
                enabled: true,
                folder: folder.display().to_string(),
                name: name.to_string(),
                ..Default::default()
            },
            keys: Keyring::new(password),
        }
    }

    fn dir(&self) -> &Path {
        self.home.path()
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir().join(name)
    }

    /// Una sincronización completa, con el estado leído del disco como al
    /// arrancar la aplicación.
    fn sync(&mut self) -> Result<Outcome, SyncError> {
        let mut state = SyncState::load(self.dir());
        sync::sync_once(self.home.path(), &self.config, &mut state, &mut self.keys)
    }

    fn state(&self) -> SyncState {
        SyncState::load(self.dir())
    }

    fn tree(&self) -> Tree {
        Tree::load(&self.file("sessions.json")).unwrap()
    }

    fn edit(&self, change: impl FnOnce(&mut Tree)) {
        let mut tree = self.tree();
        change(&mut tree);
        tree.save(&self.file("sessions.json")).unwrap();
    }

    fn names(&self) -> Vec<String> {
        self.tree()
            .session_paths()
            .into_iter()
            .map(|(_, path)| path)
            .collect()
    }

    fn settings(&self) -> Settings {
        Settings::load(&self.file("settings.json"))
    }

    fn set(&self, change: impl FnOnce(&mut Settings)) {
        let mut settings = self.settings();
        change(&mut settings);
        settings.save(&self.file("settings.json")).unwrap();
    }

    /// Contenido de los ficheros de configuración, para comprobar que una
    /// sincronización fallida no ha tocado nada.
    fn contents(&self) -> Vec<(String, Option<Vec<u8>>)> {
        [
            "sessions.json",
            "macros.json",
            "tunnels.json",
            "settings.json",
            "vault.json",
        ]
        .iter()
        .map(|name| (name.to_string(), std::fs::read(self.file(name)).ok()))
        .collect()
    }

    fn safety_copies(&self) -> Vec<PathBuf> {
        let mut copies: Vec<PathBuf> = std::fs::read_dir(self.dir())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("antes-de-sincronizar-")
            })
            .collect();
        copies.sort();
        copies
    }
}

fn host(name: &str) -> Session {
    Session {
        name: name.into(),
        host: format!("{name}.interno.example"),
        username: "administrador".into(),
        ..Default::default()
    }
}

fn session_mut(tree: &mut Tree, id: Uuid) -> &mut Session {
    match tree.find_mut(id) {
        Some(Node::Session(s)) => s,
        _ => panic!("no está el equipo"),
    }
}

/// Las dos configuraciones son la misma en todo lo que se sincroniza.
fn assert_converged(a: &Device, b: &Device) {
    assert_eq!(a.tree(), b.tree());
    for name in ["macros.json", "tunnels.json", "vault.json"] {
        assert_eq!(
            std::fs::read(a.file(name)).ok(),
            std::fs::read(b.file(name)).ok(),
            "{name}"
        );
    }
}

/// Lo que haría Dropbox con dos subidas a la vez: quedarse con una y dejar
/// la otra aparte como «copia en conflicto».
fn conflicted_copy(folder: &Path, content: &[u8]) -> PathBuf {
    let path = folder.join("godremote-sync (copia en conflicto de portátil 2026-11-28).json");
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn everything_travels_encrypted_and_both_devices_converge() {
    let folder = tempfile::tempdir().unwrap();
    let mut a = Device::new("sobremesa", folder.path());
    let mut b = Device::new("portátil", folder.path());

    // El sobremesa tiene de todo.
    let web = host("web-01");
    let db = host("db-01");
    let mut tree = Tree::default();
    let prod = tree.add_folder(None, "Producción");
    tree.upsert_session(Some(prod), web.clone());
    tree.upsert_session(Some(prod), db.clone());
    tree.upsert_session(None, host("suelto"));
    tree.save(&a.file("sessions.json")).unwrap();
    let mut macros = MacroStore::default();
    macros.add("reiniciar nginx", "sudo systemctl restart nginx\\n".into());
    macros.save(&a.file("macros.json")).unwrap();
    let mut tunnels = TunnelStore::default();
    tunnels.upsert(SavedTunnel {
        name: "base de datos".into(),
        session: Some(db.id),
        ..Default::default()
    });
    tunnels.save(&a.file("tunnels.json")).unwrap();
    let mut themes = ThemeStore::load(&a.file("themes"));
    let mut theme = Theme::base();
    theme.name = "Mío".into();
    themes.save_user(theme, None).unwrap();
    a.set(|s| {
        s.reconnect_attempts = 9;
        s.theme = "Mío".into();
        s.theme_auto = false;
        // De cada equipo: no viajan.
        s.editor = "code -w".into();
        s.log.folder = "/solo/en/el/sobremesa".into();
        s.local_shell = "/bin/zsh".into();
    });
    let mut vault = Vault::create(&a.file("vault.json"), "maestra").unwrap();
    vault.set(&web.password_key(), "contraseña-de-web").unwrap();
    drop(vault);

    // Primera sincronización: se crea el fichero de la carpeta.
    let out = a.sync().unwrap();
    assert!(out.conflicts.is_empty() && !out.applied.any());
    assert!(out.pushed > 5);
    let raw = std::fs::read_to_string(folder.path().join(SYNC_FILE)).unwrap();
    // Nada legible: ni equipos, ni usuarios, ni nombres de nada.
    for word in [
        "web-01",
        "interno.example",
        "administrador",
        "Producción",
        "nginx",
        "base de datos",
        "Mío",
        "sobremesa",
        "sessions",
        "elementos",
    ] {
        assert!(!raw.contains(word), "«{word}» se lee en la carpeta");
    }
    // Y nada más que ese fichero.
    let listed: Vec<String> = std::fs::read_dir(folder.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(listed, [SYNC_FILE]);
    // Sin cambios, otra vuelta no escribe nada.
    let out = a.sync().unwrap();
    assert!(out.write.is_none() && out.pushed == 0 && out.pulled == 0);

    // El portátil, recién instalado, lo recibe todo.
    let out = b.sync().unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    assert!(out.applied.tree && out.applied.macros && out.applied.tunnels);
    assert!(out.applied.themes && out.applied.settings && out.applied.vault);
    assert!(out.write.is_none(), "no tenía nada que añadir");
    assert_converged(&a, &b);
    assert_eq!(
        b.names(),
        ["Producción / web-01", "Producción / db-01", "suelto"]
    );
    assert_eq!(
        MacroStore::load(&b.file("macros.json")).unwrap().macros[0].name,
        "reiniciar nginx"
    );
    assert!(ThemeStore::load(&b.file("themes")).exists("Mío"));
    let settings = b.settings();
    assert_eq!(settings.reconnect_attempts, 9);
    assert_eq!(settings.theme, "Mío");
    assert_eq!(
        (
            settings.editor.as_str(),
            settings.log.folder.as_str(),
            settings.local_shell.as_str()
        ),
        ("", "", ""),
        "lo que es de cada equipo no se sincroniza"
    );
    // El baúl llega tal cual: se abre con la misma contraseña maestra.
    let vault = Vault::unlock(&b.file("vault.json"), "maestra").unwrap();
    assert_eq!(vault.get(&web.password_key()), Some("contraseña-de-web"));
    drop(vault);
    // Se ven los dos… cuando el portátil escriba. De momento, el sobremesa.
    assert_eq!(b.state().devices.len(), 1);

    // Cada uno cambia cosas distintas.
    a.edit(|tree| {
        session_mut(tree, web.id).port = 2222;
        tree.remove(db.id);
    });
    b.edit(|tree| {
        session_mut(tree, web.id).notes = "ojo con el disco".into();
        tree.upsert_session(Some(prod), host("cache-01"));
    });
    b.set(|s| s.confirm_paste = false);
    let out = a.sync().unwrap();
    assert!(out.write.is_some() && !out.applied.any());
    let copies_before = b.safety_copies().len();
    let out = b.sync().unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    assert!(out.applied.tree && out.write.is_some());
    // Antes de aplicar lo que llega se guarda lo que había.
    let copies = b.safety_copies();
    assert_eq!(copies.len(), copies_before + 1);
    assert_eq!(out.safety_copy.as_ref(), copies.last());
    let saved = std::fs::read_to_string(copies.last().unwrap()).unwrap();
    assert!(saved.contains("cache-01") && saved.contains("db-01"));
    let out = a.sync().unwrap();
    assert!(out.applied.tree && out.applied.settings && out.write.is_none());
    assert_converged(&a, &b);
    assert_eq!(
        a.names(),
        ["Producción / web-01", "Producción / cache-01", "suelto"]
    );
    let merged = a.tree().session(web.id).unwrap().clone();
    assert_eq!(
        (merged.port, merged.notes.as_str()),
        (2222, "ojo con el disco")
    );
    assert!(!a.settings().confirm_paste);
    assert_eq!(a.settings().editor, "code -w");
    // El borrado de «db-01» no vuelve, por muchas vueltas que se den.
    for turn in 0..3 {
        let device = if turn == 1 { &mut b } else { &mut a };
        let out = device.sync().unwrap();
        assert!(out.write.is_none() && !out.applied.any());
    }
    assert!(b.tree().session(db.id).is_none());
    let state = a.state();
    let seen: Vec<&str> = state.devices.values().map(|d| d.name.as_str()).collect();
    assert!(seen.contains(&"sobremesa") && seen.contains(&"portátil"));
    assert!(state.last_sync.is_some());
}

#[test]
fn conflicts_wait_for_an_answer_and_nothing_is_lost_meanwhile() {
    let folder = tempfile::tempdir().unwrap();
    let mut a = Device::new("a", folder.path());
    let mut b = Device::new("b", folder.path());
    let web = host("web");
    let old = host("antiguo");
    a.edit(|tree| {
        tree.upsert_session(None, web.clone());
        tree.upsert_session(None, old.clone());
    });
    a.sync().unwrap();
    b.sync().unwrap();
    assert_converged(&a, &b);

    // Los dos cambian el puerto del mismo equipo; y uno borra lo que el
    // otro ha tocado.
    a.edit(|tree| {
        session_mut(tree, web.id).port = 2201;
        session_mut(tree, old.id).notes = "todavía se usa".into();
    });
    b.edit(|tree| {
        session_mut(tree, web.id).port = 2202;
        session_mut(tree, web.id).username = "deploy".into();
        tree.remove(old.id);
    });
    a.sync().unwrap();
    let out = b.sync().unwrap();
    assert_eq!(out.conflicts.len(), 2, "{:?}", out.conflicts);
    assert!(!out.applied.any(), "en conflicto no se toca lo local");
    // Cada lado conserva lo suyo.
    assert_eq!(b.tree().session(web.id).unwrap().port, 2202);
    assert!(b.tree().session(old.id).is_none());
    a.sync().unwrap();
    assert_eq!(a.tree().session(web.id).unwrap().port, 2201);
    assert!(a.tree().session(old.id).is_some());
    // Y siguen pendientes, también tras «reiniciar» (el estado en disco).
    let pending = b.state().conflicts;
    assert_eq!(pending.len(), 2);
    assert_eq!(b.sync().unwrap().conflicts, pending);

    // En el puerto me quedo con lo mío; en el borrado, con lo de la carpeta.
    let mut state = b.state();
    for conflict in &pending {
        let side = if conflict.title.contains("web") {
            Side::Mine
        } else {
            Side::Theirs
        };
        state.decide(conflict, side);
    }
    state.save(b.dir()).unwrap();
    let out = b.sync().unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    assert!(out.applied.tree && out.write.is_some());
    assert!(out.safety_copy.is_some());
    a.sync().unwrap();
    assert_converged(&a, &b);
    let merged = a.tree().session(web.id).unwrap().clone();
    assert_eq!((merged.port, merged.username.as_str()), (2202, "deploy"));
    assert_eq!(a.tree().session(old.id).unwrap().notes, "todavía se usa");
    assert!(b.state().conflicts.is_empty());
}

#[test]
fn simultaneous_writes_lose_nothing() {
    let folder = tempfile::tempdir().unwrap();
    let mut a = Device::new("a", folder.path());
    let mut b = Device::new("b", folder.path());
    let (keep, gone, other) = (host("se-queda"), host("se-borra"), host("otro"));
    a.edit(|tree| {
        for s in [&keep, &gone, &other] {
            tree.upsert_session(None, s.clone());
        }
    });
    a.sync().unwrap();
    b.sync().unwrap();

    // Los dos leen la misma revisión y preparan su escritura.
    a.edit(|tree| {
        tree.remove(gone.id);
        tree.upsert_session(None, host("nuevo-de-a"));
    });
    b.edit(|tree| {
        session_mut(tree, other.id).port = 2022;
        tree.upsert_session(None, host("nuevo-de-b"));
    });
    let path = a.config.folder_path();
    let read_a = sync::read_folder(&path, &mut a.keys).unwrap();
    let read_b = sync::read_folder(&path, &mut b.keys).unwrap();
    let (mut state_a, mut state_b) = (a.state(), b.state());
    let out_a = sync::merge_local(a.dir(), &a.config, &mut state_a, &read_a).unwrap();
    let out_b = sync::merge_local(b.dir(), &b.config, &mut state_b, &read_b).unwrap();
    let (snap_a, snap_b) = (out_a.write.unwrap(), out_b.write.unwrap());
    assert_eq!(snap_a.revision, snap_b.revision, "misma revisión, a la vez");
    let expect = |read: &Remote| Expect::Unchanged(read.fingerprint.clone());

    // 1) Carpeta local o de red: el segundo ve que ha cambiado y no pisa.
    sync::write_folder(
        &path,
        &snap_a,
        &mut a.keys,
        read_a.params.as_ref(),
        &expect(&read_a),
    )
    .unwrap();
    let refused = sync::write_folder(
        &path,
        &snap_b,
        &mut b.keys,
        read_b.params.as_ref(),
        &expect(&read_b),
    );
    assert_eq!(refused.unwrap_err(), SyncError::Changed);
    assert!(SyncError::Changed.is_transient());
    // Reintenta: lo que hay no desciende de lo que creía haber escrito,
    // así que vuelve a la base anterior y reenvía sus cambios.
    let out = b.sync().unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    a.sync().unwrap();
    assert_converged(&a, &b);
    let expected = ["se-queda", "otro", "nuevo-de-a", "nuevo-de-b"];
    let mut names = a.names();
    names.sort();
    let mut sorted = expected.to_vec();
    sorted.sort();
    assert_eq!(names, sorted);
    assert_eq!(a.tree().session(other.id).unwrap().port, 2022);

    // 2) Un servicio como Dropbox: cada uno escribe en su copia, gana una
    //    y la otra queda como «copia en conflicto».
    a.edit(|tree| {
        tree.remove(keep.id);
        tree.upsert_session(None, host("segundo-de-a"));
    });
    b.edit(|tree| session_mut(tree, other.id).notes = "cambio de b".into());
    let read_a = sync::read_folder(&path, &mut a.keys).unwrap();
    let read_b = sync::read_folder(&path, &mut b.keys).unwrap();
    let (mut state_a, mut state_b) = (a.state(), b.state());
    let snap_a = sync::merge_local(a.dir(), &a.config, &mut state_a, &read_a)
        .unwrap()
        .write
        .unwrap();
    let snap_b = sync::merge_local(b.dir(), &b.config, &mut state_b, &read_b)
        .unwrap()
        .write
        .unwrap();
    sync::write_folder(
        &path,
        &snap_a,
        &mut a.keys,
        read_a.params.as_ref(),
        &Expect::Anything,
    )
    .unwrap();
    let lost = std::fs::read(path.join(SYNC_FILE)).unwrap();
    // Gana la de b: la de a se queda aparte.
    sync::write_folder(
        &path,
        &snap_b,
        &mut b.keys,
        read_b.params.as_ref(),
        &Expect::Anything,
    )
    .unwrap();
    let copy = conflicted_copy(&path, &lost);
    // b no nota nada raro; a ve que lo suyo no está y lo reenvía, incluido
    // el borrado.
    let out = b.sync().unwrap();
    assert!(out.write.is_none());
    let out = a.sync().unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    assert!(out.write.is_some() && out.applied.tree);
    b.sync().unwrap();
    assert_converged(&a, &b);
    assert!(b.tree().session(keep.id).is_none(), "el borrado de a llega");
    assert!(b.names().contains(&"segundo-de-a".to_string()));
    assert_eq!(a.tree().session(other.id).unwrap().notes, "cambio de b");
    // La copia en conflicto no estorba; se avisa y se puede limpiar.
    let read = sync::read_folder(&path, &mut a.keys).unwrap();
    assert_eq!(read.copies.len(), 1);
    assert!(copy.exists());
    assert_eq!(sync::remove_conflict_copies(&path), 1);
    assert!(!copy.exists() && path.join(SYNC_FILE).exists());
}

#[test]
fn half_written_corrupt_or_foreign_files_leave_local_untouched() {
    let folder = tempfile::tempdir().unwrap();
    let mut a = Device::new("a", folder.path());
    let mut b = Device::new("b", folder.path());
    a.edit(|tree| tree.upsert_session(None, host("uno")));
    a.sync().unwrap();
    b.edit(|tree| tree.upsert_session(None, host("propio-de-b")));
    b.set(|s| s.reconnect_attempts = 3);
    let before = b.contents();
    let file = folder.path().join(SYNC_FILE);
    let good = std::fs::read(&file).unwrap();

    // A medio copiar: vacío o cortado. No es grave, se reintenta.
    for partial in [&good[..0], &good[..good.len() / 3], &good[..good.len() - 2]] {
        std::fs::write(&file, partial).unwrap();
        let e = b.sync().unwrap_err();
        assert_eq!(e, SyncError::Seal(SealError::Incomplete));
        assert!(e.is_transient());
        assert_eq!(b.contents(), before);
        assert_eq!(std::fs::read(&file).unwrap(), partial, "no se pisa");
    }
    // Dañado o manipulado: mensaje claro, sin tocar nada.
    let mut tampered = good.clone();
    let middle = tampered.len() - 40;
    tampered[middle] = if tampered[middle] == b'A' { b'B' } else { b'A' };
    for bad in [&tampered[..], &b"{\"otra\":\"cosa\"}"[..], &b"basura"[..]] {
        std::fs::write(&file, bad).unwrap();
        let e = b.sync().unwrap_err();
        assert_eq!(e, SyncError::Seal(SealError::Corrupt));
        assert!(!e.is_transient());
        assert!(e.to_string().contains("no se ha tocado nada"), "{e}");
        assert_eq!(b.contents(), before);
    }
    // Contraseña equivocada.
    std::fs::write(&file, &good).unwrap();
    let mut wrong = Device::with_password("c", folder.path(), "no es esta");
    wrong.edit(|tree| tree.upsert_session(None, host("de-c")));
    let untouched = wrong.contents();
    let e = wrong.sync().unwrap_err();
    assert_eq!(e, SyncError::Seal(SealError::WrongPassword));
    assert!(e.to_string().contains("contraseña"), "{e}");
    assert_eq!(wrong.contents(), untouched);
    assert_eq!(std::fs::read(&file).unwrap(), good);
    // Sin carpeta (unidad sin montar): se reintenta, no se crea.
    let mut offline = Device::new("d", &folder.path().join("no-montada"));
    let e = offline.sync().unwrap_err();
    assert!(matches!(e, SyncError::Unavailable(_)) && e.is_transient());
    assert!(!folder.path().join("no-montada").exists());

    // Con el fichero entero otra vez, todo sigue.
    let out = b.sync().unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    let mut names = b.names();
    names.sort();
    assert_eq!(names, ["propio-de-b", "uno"]);
    a.sync().unwrap();
    assert_converged(&a, &b);
    assert_eq!(a.settings().reconnect_attempts, 3);

    // Si lo que no se puede leer es la configuración de este equipo, no se
    // propaga el estropicio.
    std::fs::write(b.file("sessions.json"), "{ esto no es json").unwrap();
    let folder_before = std::fs::read(&file).unwrap();
    assert!(matches!(b.sync().unwrap_err(), SyncError::Local(_)));
    assert_eq!(std::fs::read(&file).unwrap(), folder_before);
    assert_eq!(a.sync().unwrap().pulled, 0);
    assert_eq!(a.names().len(), 2);
}

#[test]
fn the_vault_travels_as_a_block_and_two_changed_vaults_are_asked_about() {
    let folder = tempfile::tempdir().unwrap();
    let mut a = Device::new("a", folder.path());
    let mut b = Device::new("b", folder.path());
    let mut vault_a = Vault::create(&a.file("vault.json"), "maestra").unwrap();
    vault_a.set("comun/password", "inicial").unwrap();
    a.sync().unwrap();
    b.sync().unwrap();
    assert_converged(&a, &b);
    let mut vault_b = Vault::unlock(&b.file("vault.json"), "maestra").unwrap();

    // Solo cambia un lado: el otro lo recibe y lo relee con su clave.
    vault_a.set("solo-a/password", "de a").unwrap();
    a.sync().unwrap();
    let out = b.sync().unwrap();
    assert!(out.applied.vault && out.conflicts.is_empty());
    vault_b.reload().unwrap();
    assert_eq!(vault_b.get("solo-a/password"), Some("de a"));

    // Cambian los dos: ni gana el más reciente ni se pierde nada; se
    // pregunta.
    vault_a
        .set("nuevo-a/password", "valor-secreto-de-a")
        .unwrap();
    vault_a.remove("solo-a/password").unwrap();
    vault_b
        .set("nuevo-b/password", "valor-secreto-de-b")
        .unwrap();
    a.sync().unwrap();
    let before = std::fs::read(b.file("vault.json")).unwrap();
    let out = b.sync().unwrap();
    assert_eq!(out.conflicts.len(), 1);
    assert!(out.conflicts[0].is_vault());
    assert_eq!(out.conflicts[0].title, "Baúl de credenciales");
    assert!(!out.applied.any());
    assert_eq!(std::fs::read(b.file("vault.json")).unwrap(), before);
    // Ningún secreto sale del baúl para avisar del conflicto.
    let shown = format!("{:?}", out.conflicts);
    assert!(!shown.contains("valor-secreto") && !shown.contains("nuevo-"));
    let state_text = std::fs::read_to_string(SyncState::file(b.dir())).unwrap();
    assert!(!state_text.contains("valor-secreto") && !state_text.contains("nuevo-b"));

    // Con el baúl abierto se combinan secreto a secreto, con lo acordado
    // la última vez como base: así el borrado de «solo-a» no vuelve.
    let theirs = out.their_vault.unwrap();
    let state = b.state();
    let their_secrets = vault_b.read_other(theirs.as_bytes(), None).unwrap();
    let base = vault_b
        .read_other(state.vault_base().unwrap().as_bytes(), None)
        .unwrap();
    let plan = sync::plan_vault_merge(Some(&base), vault_b.entries(), &their_secrets, Side::Mine);
    assert!(plan.clashes.is_empty());
    assert_eq!(plan.remove, ["solo-a/password"]);
    vault_b.apply(&plan.set, &plan.remove).unwrap();
    // El baúl combinado es «lo mío»: se decide así y se envía.
    let mut state = b.state();
    state.keep_merged_vault(b.dir(), &theirs).unwrap();
    state.save(b.dir()).unwrap();
    let out = b.sync().unwrap();
    assert!(out.conflicts.is_empty() && out.write.is_some());
    let out = a.sync().unwrap();
    assert!(out.applied.vault && out.safety_copy.is_some());
    assert_converged(&a, &b);
    vault_a.reload().unwrap();
    let mut keys: Vec<&str> = vault_a.keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["comun/password", "nuevo-a/password", "nuevo-b/password"]
    );
    assert_eq!(vault_a.get("nuevo-b/password"), Some("valor-secreto-de-b"));
    // El baúl anterior del equipo que recibe queda en su copia previa.
    let saved = std::fs::read_to_string(out.safety_copy.unwrap()).unwrap();
    assert!(saved.contains("vault.json"));
}

#[test]
fn rewriting_the_folder_and_forgetting_the_state_are_safe() {
    let folder = tempfile::tempdir().unwrap();
    let mut a = Device::new("a", folder.path());
    let mut b = Device::new("b", folder.path());
    let (one, two) = (host("uno"), host("dos"));
    a.edit(|tree| {
        tree.upsert_session(None, one.clone());
        tree.upsert_session(None, two.clone());
    });
    a.sync().unwrap();
    b.sync().unwrap();

    // El fichero se estropea para siempre. Entretanto, a borra un equipo y
    // b añade otro.
    let file = folder.path().join(SYNC_FILE);
    std::fs::write(&file, "estropeado").unwrap();
    a.edit(|tree| {
        tree.remove(two.id);
    });
    b.edit(|tree| tree.upsert_session(None, host("tres")));
    assert!(a.sync().is_err() && b.sync().is_err());
    // a reescribe la carpeta con lo suyo (lo pide el usuario).
    let mut state = a.state();
    let out = sync::merge_local(a.dir(), &a.config, &mut state, &Remote::default()).unwrap();
    let path = a.config.folder_path();
    sync::write_folder(
        &path,
        &out.write.unwrap(),
        &mut a.keys,
        None,
        &Expect::Anything,
    )
    .unwrap();
    // b reconoce de dónde viene (desciende de lo que ya conocía): fusiona
    // con base, así que el borrado de a se respeta y lo suyo se conserva.
    let out = b.sync().unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    let mut names = b.names();
    names.sort();
    assert_eq!(names, ["tres", "uno"]);
    a.sync().unwrap();
    assert_converged(&a, &b);

    // Un equipo que pierde su estado (o se reinstala con una copia vieja de
    // su configuración) no borra nada a nadie: une.
    std::fs::remove_file(SyncState::file(b.dir())).unwrap();
    a.edit(|tree| tree.upsert_session(None, host("cuatro")));
    a.sync().unwrap();
    let out = b.sync().unwrap();
    assert!(out.conflicts.is_empty(), "{:?}", out.conflicts);
    a.sync().unwrap();
    assert_converged(&a, &b);
    assert_eq!(a.names().len(), 3);

    // Carpeta vaciada (alguien borró el fichero): no se toma por «todo
    // borrado»; quien llega primero la vuelve a llenar.
    std::fs::remove_file(&file).unwrap();
    let out = b.sync().unwrap();
    assert!(!out.applied.any() && out.write.is_some());
    assert_eq!(b.names().len(), 3);
    a.sync().unwrap();
    assert_converged(&a, &b);

    // Cambio de contraseña: se escribe con la nueva y el otro equipo la
    // rechaza con un mensaje claro hasta que se le da.
    a.keys = Keyring::new("contraseña nueva");
    assert_eq!(
        a.sync().unwrap_err(),
        SyncError::Seal(SealError::WrongPassword)
    );
    let mut state = a.state();
    let out = sync::merge_local(a.dir(), &a.config, &mut state, &Remote::default()).unwrap();
    sync::write_folder(
        &path,
        &out.write.unwrap(),
        &mut a.keys,
        None,
        &Expect::Anything,
    )
    .unwrap();
    assert_eq!(
        b.sync().unwrap_err(),
        SyncError::Seal(SealError::WrongPassword)
    );
    b.keys = Keyring::new("contraseña nueva");
    b.edit(|tree| tree.upsert_session(None, host("cinco")));
    b.sync().unwrap();
    a.sync().unwrap();
    assert_converged(&a, &b);
    assert_eq!(a.names().len(), 4);
}

#[test]
fn old_safety_copies_are_pruned() {
    let folder = tempfile::tempdir().unwrap();
    let mut a = Device::new("a", folder.path());
    let mut b = Device::new("b", folder.path());
    let web = host("web");
    a.edit(|tree| tree.upsert_session(None, web.clone()));
    a.sync().unwrap();
    b.sync().unwrap();
    for round in 0..14u16 {
        a.edit(|tree| session_mut(tree, web.id).port = 3000 + round);
        a.sync().unwrap();
        assert!(b.sync().unwrap().applied.tree);
    }
    let copies = b.safety_copies();
    assert_eq!(copies.len(), 10, "{copies:?}");
    // Quedan las más recientes: la última guarda el penúltimo puerto.
    let last = std::fs::read_to_string(copies.last().unwrap()).unwrap();
    assert!(last.contains("3012"), "{last}");
}
