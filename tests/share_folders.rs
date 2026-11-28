//! Carpetas compartidas: quien publica y quienes se suscriben son carpetas
//! de configuración distintas, y el fichero publicado vive en una carpeta
//! temporal que hace de recurso compartido del equipo.

use std::path::{Path, PathBuf};

use godremote::model::{AuthMethod, Node, Session, Tree};
use godremote::share::{
    self, Fetched, Publication, ShareError, ShareStore, Subscription, local_id,
};
use godremote::sync::{self, Keyring, SealError, SyncConfig, SyncState};
use godremote::vault::Vault;
use uuid::Uuid;

const PASSWORD: &str = "contraseña del equipo";

/// Una carpeta de configuración: la de quien publica o la de un compañero.
struct Profile {
    home: tempfile::TempDir,
}

impl Profile {
    fn new() -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn dir(&self) -> &Path {
        self.home.path()
    }

    fn tree(&self) -> Tree {
        Tree::load(&self.dir().join("sessions.json")).unwrap()
    }

    fn edit(&self, change: impl FnOnce(&mut Tree)) {
        let mut tree = self.tree();
        change(&mut tree);
        tree.save(&self.dir().join("sessions.json")).unwrap();
    }

    fn names(&self) -> Vec<String> {
        self.tree()
            .session_paths()
            .into_iter()
            .map(|(_, path)| path)
            .collect()
    }

    fn shares(&self) -> ShareStore {
        ShareStore::load(self.dir())
    }

    /// Se suscribe: lee el fichero, lo acepta y pone la carpeta en su
    /// árbol, como hace la ventana.
    fn subscribe(&self, file: &Path, password: &str) -> Result<Uuid, ShareError> {
        let fetched = share::fetch(file, &mut Keyring::new(password))?;
        let mut sub = Subscription::new(&file.display().to_string(), &fetched);
        sub.accept(&fetched)?;
        let id = sub.id;
        self.edit(|tree| {
            sub.place(tree, None);
        });
        let mut store = self.shares();
        store.subscriptions.push(sub);
        store.save(self.dir()).unwrap();
        Ok(id)
    }

    /// Lo que se hace cada cierto tiempo con una carpeta que se sigue:
    /// mirar el fichero y, si ha cambiado, aplicarlo. Devuelve el resumen
    /// de lo que cambia, o `None` si no había nada nuevo.
    fn update(&self, id: Uuid, password: &str) -> Result<Option<String>, ShareError> {
        let mut store = self.shares();
        let sub = store
            .subscriptions
            .iter_mut()
            .find(|s| s.id == id)
            .expect("la suscripción");
        let fetched: Fetched = share::fetch(&sub.file_path(), &mut Keyring::new(password))?;
        if fetched.fingerprint == sub.file {
            return Ok(None);
        }
        let previous = sub.content.clone();
        let mut changes = sub.accept(&fetched)?;
        self.edit(|tree| changes.rescued = sub.place(tree, previous.as_ref()));
        store.save(self.dir()).unwrap();
        Ok(Some(changes.summary()))
    }
}

fn host(name: &str) -> Session {
    Session {
        name: name.into(),
        host: format!("{name}.oficina.example"),
        username: "operador".into(),
        ..Default::default()
    }
}

fn session_mut(tree: &mut Tree, id: Uuid) -> &mut Session {
    match tree.find_mut(id) {
        Some(Node::Session(s)) => s,
        _ => panic!("no está el equipo"),
    }
}

/// Quien publica: una carpeta «Clientes» con una subcarpeta, y cosas suyas
/// fuera que no se publican.
fn publisher(shared: &Path) -> (Profile, Publication, [Session; 3]) {
    let profile = Profile::new();
    let (web, db, deep) = (host("web"), host("db"), host("interno"));
    let mut team = Uuid::nil();
    profile.edit(|tree| {
        tree.upsert_session(None, host("privado"));
        team = tree.add_folder(None, "Clientes");
        let mut with_secrets = web.clone();
        with_secrets.on_connect = "sudo systemctl status".into();
        with_secrets.agent_forwarding = true;
        tree.upsert_session(Some(team), with_secrets);
        tree.upsert_session(Some(team), db.clone());
        let sub = tree.add_folder(Some(team), "ACME S.A.");
        tree.upsert_session(Some(sub), deep.clone());
    });
    // Tiene contraseñas guardadas de esos equipos: no deben salir de aquí.
    let mut vault = Vault::create(&profile.dir().join("vault.json"), "maestra").unwrap();
    vault
        .set(&web.password_key(), "secreto-de-quien-publica")
        .unwrap();
    let publication = Publication {
        folder: team,
        path: shared
            .join("clientes.godremote-carpeta.json")
            .display()
            .to_string(),
        ..Default::default()
    };
    (profile, publication, [web, db, deep])
}

#[test]
fn a_published_folder_reaches_subscribers_and_follows_its_changes() {
    let shared = tempfile::tempdir().unwrap();
    let (ana, mut publication, [web, db, deep]) = publisher(shared.path());
    let mut keys = Keyring::new(PASSWORD);
    let file = PathBuf::from(&publication.path);
    let export = share::publish(&ana.tree(), &mut publication, "Ana", &mut keys).unwrap();
    assert_eq!((export.report.sessions, export.report.disarmed), (3, 1));
    // Un solo fichero, sin nada legible y sin rastro de los secretos.
    let listed: Vec<_> = std::fs::read_dir(shared.path())
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(listed.len(), 1);
    let raw = std::fs::read_to_string(&file).unwrap();
    for word in [
        "oficina.example",
        "operador",
        "Clientes",
        "ACME S.A.",
        "secreto-de-quien-publica",
        "systemctl",
        "privado",
    ] {
        assert!(
            !raw.contains(word),
            "«{word}» se lee en el fichero publicado"
        );
    }

    // Dos compañeros se suscriben; uno ya tenía equipos suyos.
    let (luis, marta) = (Profile::new(), Profile::new());
    let own = host("de-luis");
    luis.edit(|tree| tree.upsert_session(None, own.clone()));
    assert_eq!(
        luis.subscribe(&file, "no es esta").unwrap_err(),
        ShareError::Seal(SealError::WrongPassword)
    );
    assert_eq!(luis.names(), ["de-luis"], "sin contraseña no cambia nada");
    let sub_luis = luis.subscribe(&file, PASSWORD).unwrap();
    let sub_marta = marta.subscribe(&file, PASSWORD).unwrap();
    assert_eq!(
        sub_luis, sub_marta,
        "la misma publicación, el mismo identificador"
    );
    assert_eq!(
        luis.names(),
        [
            "de-luis",
            "Clientes / web",
            "Clientes / db",
            "Clientes / ACME S.A. / interno"
        ]
    );
    assert_eq!(marta.names().len(), 3);
    // Lo que no se publica no llega: ni lo de fuera de la carpeta, ni las
    // órdenes al conectar, ni el reenvío del agente.
    let tree = luis.tree();
    let local_web = local_id(sub_luis, web.id);
    let shared_web = tree.session(local_web).unwrap();
    assert_eq!(shared_web.host, "web.oficina.example");
    assert!(shared_web.on_connect.is_empty() && !shared_web.agent_forwarding);
    assert!(
        tree.find(web.id).is_none(),
        "nunca con el identificador original"
    );
    // La suscripción queda anotada; el fichero de quien publica, intacto.
    let store = luis.shares();
    assert_eq!(store.roots().into_iter().collect::<Vec<_>>(), [sub_luis]);
    assert_eq!(store.subscriptions[0].publisher, "Ana");
    assert_eq!(
        ana.tree().session(web.id).unwrap().on_connect,
        "sudo systemctl status"
    );

    // Cada cual con su usuario y su clave, sin tocar lo publicado.
    let mut store = luis.shares();
    store.subscriptions[0].my_username = "luis".into();
    store.subscriptions[0].my_auth = Some(AuthMethod::Key);
    store.subscriptions[0].my_key_path = "~/.ssh/luis".into();
    let sub = store.subscriptions[0].clone();
    luis.edit(|tree| {
        sub.place(tree, sub.content.as_ref());
    });
    store.save(luis.dir()).unwrap();
    let as_luis = luis.tree().resolved_session(local_web).unwrap();
    assert_eq!(
        (
            as_luis.username.as_str(),
            as_luis.auth,
            as_luis.key_path.as_str()
        ),
        ("luis", AuthMethod::Key, "~/.ssh/luis")
    );
    let as_marta = marta.tree().resolved_session(local_web).unwrap();
    assert_eq!(
        (as_marta.username.as_str(), as_marta.auth),
        ("operador", AuthMethod::Password)
    );

    // Sin cambios no hay nada que aplicar.
    assert_eq!(luis.update(sub_luis, PASSWORD).unwrap(), None);

    // Quien publica cambia la carpeta y vuelve a publicar: llega sola.
    ana.edit(|tree| {
        session_mut(tree, db.id).host = "db-nueva.oficina.example".into();
        tree.remove(deep.id);
        let folder = tree.parent_of(web.id);
        tree.upsert_session(folder, host("cache"));
    });
    let changed = share::export(&ana.tree(), publication.folder)
        .unwrap()
        .unwrap();
    assert_ne!(changed.digest, publication.digest, "hay algo que publicar");
    share::publish(&ana.tree(), &mut publication, "Ana", &mut keys).unwrap();
    assert_eq!(publication.revision, 2);
    let summary = luis.update(sub_luis, PASSWORD).unwrap().unwrap();
    assert!(summary.contains("1 equipo(s) nuevos"), "{summary}");
    assert!(summary.contains("1 quitados"), "{summary}");
    assert!(summary.contains("db-nueva.oficina.example"), "{summary}");
    assert_eq!(
        luis.names(),
        [
            "de-luis",
            "Clientes / web",
            "Clientes / db",
            "Clientes / cache"
        ]
    );
    // Sigue con su usuario, y la contraseña que tuviera guardada para la
    // dirección anterior de «db» queda marcada para olvidarse.
    let tree = luis.tree();
    assert_eq!(
        tree.resolved_session(local_id(sub_luis, db.id))
            .unwrap()
            .username,
        "luis"
    );
    assert_eq!(
        luis.shares().subscriptions[0].forget,
        [local_id(sub_luis, db.id)]
    );
    assert_eq!(luis.shares().subscriptions[0].revision, 2);
    // Un equipo que desaparece deja de estar en el árbol: una pestaña
    // guardada para reabrirlo ya no lo encuentra y se salta.
    assert!(tree.resolved_session(local_id(sub_luis, deep.id)).is_none());

    // Fichero a medio copiar: se conserva lo que había y se reintenta.
    let good = std::fs::read(&file).unwrap();
    std::fs::write(&file, &good[..good.len() / 2]).unwrap();
    let before = marta.tree();
    let e = marta.update(sub_marta, PASSWORD).unwrap_err();
    assert!(e.is_transient(), "{e}");
    assert_eq!(marta.tree(), before);
    assert_eq!(marta.names().len(), 3, "sigue con la versión anterior");
    // Sin el recurso compartido (sin conexión), lo mismo.
    std::fs::remove_file(&file).unwrap();
    assert!(
        marta
            .update(sub_marta, PASSWORD)
            .unwrap_err()
            .is_transient()
    );
    assert_eq!(marta.tree(), before);
    // De vuelta, se pone al día de una vez.
    std::fs::write(&file, &good).unwrap();
    assert!(marta.update(sub_marta, PASSWORD).unwrap().is_some());
    assert_eq!(marta.tree().session_paths().len(), 3);
    assert!(marta.names().contains(&"Clientes / cache".to_string()));

    // Alguien repone una versión antigua del fichero: no se acepta.
    let mut old_keys = Keyring::new(PASSWORD);
    let mut old = publication.clone();
    old.revision = 0;
    std::fs::remove_file(&file).unwrap();
    share::publish(&ana.tree(), &mut old, "Ana", &mut old_keys).unwrap();
    assert_eq!(old.revision, 1);
    let e = luis.update(sub_luis, PASSWORD).unwrap_err();
    assert_eq!(e, ShareError::Stale { seen: 2, found: 1 });
    assert_eq!(luis.names().len(), 4);
}

#[test]
fn followed_folders_stay_out_of_my_own_sync() {
    let shared = tempfile::tempdir().unwrap();
    let cloud = tempfile::tempdir().unwrap();
    let (ana, mut publication, [web, ..]) = publisher(shared.path());
    let file = PathBuf::from(&publication.path);
    share::publish(
        &ana.tree(),
        &mut publication,
        "Ana",
        &mut Keyring::new(PASSWORD),
    )
    .unwrap();

    // Luis sigue la carpeta en su sobremesa, que además sincroniza su
    // configuración con su portátil.
    let (desktop, laptop) = (Profile::new(), Profile::new());
    let own = host("de-luis");
    desktop.edit(|tree| tree.upsert_session(None, own.clone()));
    let sub = desktop.subscribe(&file, PASSWORD).unwrap();
    assert_eq!(desktop.names().len(), 4);
    let config = |name: &str| SyncConfig {
        enabled: true,
        folder: cloud.path().display().to_string(),
        name: name.into(),
        ..Default::default()
    };
    let (desktop_config, laptop_config) = (config("sobremesa"), config("portátil"));
    let mut keys = Keyring::new("contraseña de sincronización");
    let mut run = |profile: &Profile, config: &SyncConfig| {
        let mut state = SyncState::load(profile.dir());
        sync::sync_once(profile.dir(), config, &mut state, &mut keys).unwrap()
    };
    run(&desktop, &desktop_config);
    // A la carpeta de sincronización solo va lo suyo.
    let remote = sync::read_folder(
        cloud.path(),
        &mut Keyring::new("contraseña de sincronización"),
    )
    .unwrap();
    let items = remote.snapshot.unwrap().items;
    assert!(items.contains_key(&format!("s/{}", own.id)));
    assert!(!items.contains_key(&format!("c/{sub}")));
    assert!(!items.contains_key(&format!("s/{}", local_id(sub, web.id))));
    assert_eq!(items.keys().filter(|k| k.starts_with("s/")).count(), 1);
    // Al portátil llega lo suyo, no la carpeta del compañero (la seguirá
    // desde allí si quiere, con su ruta).
    run(&laptop, &laptop_config);
    assert_eq!(laptop.names(), ["de-luis"]);
    // El portátil añade un equipo; al aplicarlo en el sobremesa la carpeta
    // compartida sigue en su sitio.
    laptop.edit(|tree| tree.upsert_session(None, host("del-portátil")));
    run(&laptop, &laptop_config);
    let outcome = run(&desktop, &desktop_config);
    assert!(outcome.applied.tree);
    let mut names = desktop.names();
    names.sort();
    assert_eq!(
        names,
        [
            "Clientes / ACME S.A. / interno",
            "Clientes / db",
            "Clientes / web",
            "de-luis",
            "del-portátil"
        ]
    );
    assert!(desktop.tree().find(sub).is_some());
    // Y otra vuelta no cambia nada en ningún lado.
    assert!(run(&desktop, &desktop_config).write.is_none());
    assert!(run(&laptop, &laptop_config).write.is_none());
    assert_eq!(laptop.names().len(), 2);

    // Si se suscribe también en el portátil, los equipos compartidos tienen
    // allí los mismos identificadores: las credenciales que guarde (y que
    // viajan en el baúl sincronizado) le valen en los dos.
    let sub_laptop = laptop.subscribe(&file, PASSWORD).unwrap();
    assert_eq!(sub_laptop, sub);
    assert!(laptop.tree().session(local_id(sub, web.id)).is_some());
    assert!(run(&laptop, &laptop_config).write.is_none());
}
