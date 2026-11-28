//! Modo claro u oscuro del sistema operativo, para elegir el tema solo.
//!
//! Cada sistema lo cuenta a su manera: Windows en el registro, macOS en sus
//! preferencias y los escritorios de Linux a través del portal de escritorio
//! (o, si no lo hay, de la configuración de GTK o de KDE). Cuando no hay
//! manera de saberlo la respuesta es `None`, y decide quien pregunta.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Tiempo mínimo entre dos consultas al sistema.
const MIN_INTERVAL: Duration = Duration::from_secs(3);

/// Cada cuánto conviene volver a mirar aunque no pase nada en la ventana.
pub const IDLE_INTERVAL: Duration = Duration::from_secs(20);

/// Función que despierta la interfaz (pide un repintado).
pub type Waker = Arc<dyn Fn() + Send + Sync>;

struct State {
    dark: Option<bool>,
    /// La interfaz ha pedido que se vuelva a mirar.
    requested: bool,
    stop: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Vigila el modo del sistema sin entretener a la interfaz: las consultas
/// (que en algunos sistemas lanzan un programa) van en un hilo aparte y solo
/// se repiten cuando alguien lo pide.
pub struct Watcher {
    shared: Arc<Shared>,
}

impl Watcher {
    /// Mira el sistema una vez, para arrancar ya con el tema que toca, y
    /// deja preparado el hilo que repetirá la consulta.
    pub fn start(waker: Waker) -> Self {
        Self::with_source(query, waker, MIN_INTERVAL)
    }

    fn with_source(
        source: impl Fn() -> Option<bool> + Send + 'static,
        waker: Waker,
        interval: Duration,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                dark: source(),
                requested: false,
                stop: false,
            }),
            wake: Condvar::new(),
        });
        let background = shared.clone();
        // Si el hilo no pudiera crearse, queda lo leído al arrancar.
        let _ = std::thread::Builder::new()
            .name("tema-del-sistema".into())
            .spawn(move || watch(&background, source, waker, interval));
        Self { shared }
    }

    /// `Some(true)` si el sistema está en modo oscuro, `Some(false)` si en
    /// claro y `None` si no se ha podido averiguar.
    pub fn dark(&self) -> Option<bool> {
        lock(&self.shared.state).dark
    }

    /// Pide que se vuelva a mirar. Es barato llamarla en cada fotograma: las
    /// consultas se espacian solas.
    pub fn refresh(&self) {
        let mut state = lock(&self.shared.state);
        if !state.requested {
            state.requested = true;
            self.shared.wake.notify_all();
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        lock(&self.shared.state).stop = true;
        self.shared.wake.notify_all();
    }
}

fn watch(shared: &Shared, source: impl Fn() -> Option<bool>, waker: Waker, interval: Duration) {
    loop {
        {
            let mut state = lock(&shared.state);
            while !state.requested && !state.stop {
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if state.stop {
                return;
            }
            state.requested = false;
        }
        let dark = source();
        {
            let mut state = lock(&shared.state);
            if state.stop {
                return;
            }
            // Una consulta fallida no borra lo que ya se sabía: el tema no
            // debe bailar porque un programa tarde en contestar.
            if dark.is_some() && state.dark != dark {
                state.dark = dark;
                drop(state);
                waker();
            }
        }
        // Descanso entre consultas (se interrumpe al cerrar).
        let rested = Instant::now() + interval;
        let mut state = lock(&shared.state);
        while !state.stop {
            let now = Instant::now();
            if now >= rested {
                break;
            }
            state = shared
                .wake
                .wait_timeout(state, rested - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        if state.stop {
            return;
        }
    }
}

/// Pregunta al sistema operativo: `Some(true)` si pide modo oscuro.
pub fn query() -> Option<bool> {
    platform::query()
}

#[cfg(windows)]
mod platform {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};

    /// «Modo de aplicación» de Configuración → Personalización → Colores.
    pub fn query() -> Option<bool> {
        let wide = |text: &str| text.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
        let value = wide("AppsUseLightTheme");
        let mut light: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        // SAFETY: las dos cadenas acaban en nulo y viven durante la llamada;
        // `light` y `size` describen un búfer válido de cuatro bytes, que es
        // lo que ocupa el único tipo de valor que se admite (REG_DWORD).
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&raw mut light).cast(),
                &raw mut size,
            )
        };
        (status == ERROR_SUCCESS).then_some(light == 0)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    pub fn query() -> Option<bool> {
        let (success, output) = super::run(
            "/usr/bin/defaults",
            &["read", "-g", "AppleInterfaceStyle"],
            None,
        )?;
        Some(super::macos_is_dark(success, &output))
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::{Evidence, run};

    const PORTAL: &str = "org.freedesktop.portal.Desktop";
    const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
    const NAMESPACE: &str = "org.freedesktop.appearance";

    pub fn query() -> Option<bool> {
        let mut evidence = Evidence {
            gtk_env: std::env::var("GTK_THEME").ok().filter(|v| !v.is_empty()),
            ..Default::default()
        };
        // Sin bus de sesión no hay escritorio al que preguntar (y las
        // herramientas intentarían arrancar uno).
        if let Some(bus) = session_bus() {
            evidence.portal = portal(&bus);
            match evidence.portal {
                // Preferencia expresa: no hace falta más.
                Some(1 | 2) => {}
                // El escritorio contesta «sin preferencia»: queda por ver si
                // usa un tema de los que se llaman «…-dark».
                Some(_) => evidence.gtk_theme = gsettings(&bus, "gtk-theme"),
                None => {
                    evidence.gnome_scheme = gsettings(&bus, "color-scheme");
                    evidence.gtk_theme = gsettings(&bus, "gtk-theme");
                }
            }
        }
        if !matches!(evidence.portal, Some(1 | 2)) {
            evidence.kde_scheme = kde_scheme();
        }
        evidence.verdict()
    }

    /// Dirección del bus de sesión de D-Bus, si lo hay.
    fn session_bus() -> Option<String> {
        if let Some(address) = std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .ok()
            .filter(|a| !a.is_empty())
        {
            return Some(address);
        }
        let socket = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("bus");
        socket
            .exists()
            .then(|| format!("unix:path={}", socket.display()))
    }

    /// `color-scheme` del portal de escritorio: 0 sin preferencia, 1 oscuro,
    /// 2 claro. Se pregunta con la primera herramienta de D-Bus que haya.
    fn portal(bus: &str) -> Option<u32> {
        let settings = format!("{PORTAL}.Settings");
        let method = format!("{settings}.Read");
        let attempts: [(&str, Vec<&str>); 3] = [
            (
                "gdbus",
                vec![
                    "call",
                    "--session",
                    "--timeout",
                    "1",
                    "--dest",
                    PORTAL,
                    "--object-path",
                    PORTAL_PATH,
                    "--method",
                    &method,
                    NAMESPACE,
                    "color-scheme",
                ],
            ),
            (
                "dbus-send",
                vec![
                    "--session",
                    "--print-reply=literal",
                    "--reply-timeout=1000",
                    "--dest=org.freedesktop.portal.Desktop",
                    PORTAL_PATH,
                    &method,
                    "string:org.freedesktop.appearance",
                    "string:color-scheme",
                ],
            ),
            (
                "busctl",
                vec![
                    "--user",
                    "--timeout=1",
                    "call",
                    PORTAL,
                    PORTAL_PATH,
                    &settings,
                    "Read",
                    "ss",
                    NAMESPACE,
                    "color-scheme",
                ],
            ),
        ];
        for (program, args) in attempts {
            match run(program, &args, Some(bus)) {
                Some((true, reply)) => return super::parse_portal_reply(&reply),
                // La herramienta existe y el portal no contesta: otra
                // herramienta diría lo mismo.
                Some((false, _)) => return None,
                // No está instalada (o se ha colgado): se prueba la siguiente.
                None => {}
            }
        }
        None
    }

    fn gsettings(bus: &str, key: &str) -> Option<String> {
        let (success, output) = run(
            "gsettings",
            &["get", "org.gnome.desktop.interface", key],
            Some(bus),
        )?;
        success
            .then(|| super::unquote(&output))
            .filter(|v| !v.is_empty())
    }

    /// Esquema de colores de KDE Plasma, de su fichero de configuración.
    fn kde_scheme() -> Option<String> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                Some(std::path::PathBuf::from(std::env::var_os("HOME")?).join(".config"))
            })?;
        let text = std::fs::read_to_string(config.join("kdeglobals")).ok()?;
        super::kde_color_scheme(&text)
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    pub fn query() -> Option<bool> {
        None
    }
}

/// Lo que tarda como mucho una herramienta del sistema en contestar.
#[cfg(unix)]
const COMMAND_TIMEOUT: Duration = Duration::from_millis(1500);

/// Ejecuta un programa del sistema y devuelve si terminó bien y lo que
/// escribió. `None` si no existe o no contesta a tiempo.
#[cfg(unix)]
fn run(program: &str, args: &[&str], bus: Option<&str>) -> Option<(bool, String)> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(bus) = bus {
        command.env("DBUS_SESSION_BUS_ADDRESS", bus);
    }
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    // La salida se lee aparte: así un programa que no la cierra no deja
    // esperando a quien pregunta.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = sender.send(text);
    });
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(4));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let text = receiver.recv_timeout(Duration::from_millis(200)).ok()?;
    Some((status.success(), text))
}

/// `defaults read -g AppleInterfaceStyle` contesta «Dark» en modo oscuro y
/// falla en modo claro, porque entonces la clave no existe.
#[cfg(any(test, target_os = "macos"))]
fn macos_is_dark(success: bool, output: &str) -> bool {
    success && output.trim().eq_ignore_ascii_case("dark")
}

/// El número de la respuesta del portal, la escriba quien la escriba:
/// `(<<uint32 1>>,)` (gdbus), `variant variant uint32 1` (dbus-send) o
/// `v v u 1` (busctl).
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn parse_portal_reply(reply: &str) -> Option<u32> {
    let digits: String = reply
        .split_whitespace()
        .last()?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// Quita las comillas con que `gsettings` escribe las cadenas.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn unquote(output: &str) -> String {
    output.trim().trim_matches(['\'', '"']).to_string()
}

/// `ColorScheme` del grupo `[General]` de un `kdeglobals`.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn kde_color_scheme(config: &str) -> Option<String> {
    let mut in_general = false;
    for line in config.lines().map(str::trim) {
        if line.starts_with('[') {
            in_general = line == "[General]";
        } else if in_general
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "ColorScheme"
        {
            return Some(value.trim().to_string()).filter(|v| !v.is_empty());
        }
    }
    None
}

/// Lo que se ha podido saber de un escritorio de Linux.
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
#[derive(Debug, Default, PartialEq)]
struct Evidence {
    /// `color-scheme` del portal: 0 sin preferencia, 1 oscuro, 2 claro.
    portal: Option<u32>,
    /// `color-scheme` de GNOME: `default`, `prefer-dark` o `prefer-light`.
    gnome_scheme: Option<String>,
    /// Nombre del tema de GTK.
    gtk_theme: Option<String>,
    /// Esquema de colores de KDE.
    kde_scheme: Option<String>,
    /// Variable de entorno `GTK_THEME`.
    gtk_env: Option<String>,
}

#[cfg(any(test, all(unix, not(target_os = "macos"))))]
impl Evidence {
    fn verdict(&self) -> Option<bool> {
        match self.portal {
            Some(1) => return Some(true),
            Some(2) => return Some(false),
            _ => {}
        }
        match self.gnome_scheme.as_deref() {
            Some("prefer-dark") => return Some(true),
            Some("prefer-light") => return Some(false),
            _ => {}
        }
        // Sin preferencia expresa quedan los nombres: antes de que existiera,
        // el modo oscuro era elegir un tema «…-dark».
        let names = [&self.gtk_env, &self.gtk_theme, &self.kde_scheme];
        if names
            .iter()
            .any(|name| name.as_deref().is_some_and(name_is_dark))
        {
            return Some(true);
        }
        // Hay un escritorio que contesta y no pide oscuro: es claro.
        let answered = self.portal.is_some()
            || self.gnome_scheme.is_some()
            || names.iter().any(|name| name.is_some());
        answered.then_some(false)
    }
}

/// ¿Es el nombre de un tema oscuro? («Adwaita-dark», «BreezeDark»…)
#[cfg(any(test, all(unix, not(target_os = "macos"))))]
fn name_is_dark(name: &str) -> bool {
    name.to_lowercase().contains("dark")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

    #[test]
    fn the_system_can_be_asked_without_hanging() {
        // Lo que conteste depende del equipo; lo que no puede es colgarse
        // ni contradecirse en dos consultas seguidas.
        let started = Instant::now();
        let first = query();
        assert_eq!(first, query());
        assert!(started.elapsed() < Duration::from_secs(15));
        eprintln!("modo del sistema: {first:?}");
    }

    #[test]
    fn portal_replies_are_understood_from_any_tool() {
        assert_eq!(parse_portal_reply("(<<uint32 1>>,)\n"), Some(1));
        assert_eq!(parse_portal_reply("(<<uint32 0>>,)"), Some(0));
        assert_eq!(
            parse_portal_reply("   variant       variant          uint32 2\n"),
            Some(2)
        );
        assert_eq!(parse_portal_reply("v v u 1\n"), Some(1));
        assert_eq!(parse_portal_reply(""), None);
        assert_eq!(parse_portal_reply("Error: no such interface"), None);
    }

    #[test]
    fn gsettings_and_kde_values_are_read() {
        assert_eq!(unquote("'prefer-dark'\n"), "prefer-dark");
        assert_eq!(unquote("'Yaru-dark'"), "Yaru-dark");
        assert_eq!(unquote("\n"), "");

        let kde = "[General]\nName=Brisa\nColorScheme=BreezeDark\n\n[KDE]\nColorScheme=Otro\n";
        assert_eq!(kde_color_scheme(kde).as_deref(), Some("BreezeDark"));
        // Solo cuenta el del grupo general, y puede no estar.
        assert_eq!(kde_color_scheme("[KDE]\nColorScheme=BreezeDark\n"), None);
        assert_eq!(kde_color_scheme("[General]\nColorScheme=\n"), None);
        assert_eq!(kde_color_scheme(""), None);
    }

    #[test]
    fn macos_reports_dark_only_when_the_key_exists() {
        assert!(macos_is_dark(true, "Dark\n"));
        assert!(!macos_is_dark(false, ""));
        assert!(!macos_is_dark(true, "Light\n"));
        assert!(!macos_is_dark(false, "Dark"));
    }

    #[test]
    fn linux_evidence_is_weighed_in_order() {
        let text = |value: &str| Some(value.to_string());
        // Nada que contestara: no se sabe.
        assert_eq!(Evidence::default().verdict(), None);
        // El portal manda cuando expresa una preferencia…
        let dark_portal = Evidence {
            portal: Some(1),
            gtk_theme: text("Adwaita"),
            ..Default::default()
        };
        assert_eq!(dark_portal.verdict(), Some(true));
        let light_portal = Evidence {
            portal: Some(2),
            gtk_env: text("Adwaita:dark"),
            ..Default::default()
        };
        assert_eq!(light_portal.verdict(), Some(false));
        // …y si no la expresa, un tema «…-dark» sigue siendo oscuro.
        let legacy = Evidence {
            portal: Some(0),
            gtk_theme: text("Mint-Y-Dark"),
            ..Default::default()
        };
        assert_eq!(legacy.verdict(), Some(true));
        let plain = Evidence {
            portal: Some(0),
            gtk_theme: text("Adwaita"),
            ..Default::default()
        };
        assert_eq!(plain.verdict(), Some(false));
        // Sin portal: la configuración de GNOME, KDE o la variable de GTK.
        let gnome = |scheme: &str| Evidence {
            gnome_scheme: text(scheme),
            ..Default::default()
        };
        assert_eq!(gnome("prefer-dark").verdict(), Some(true));
        assert_eq!(gnome("prefer-light").verdict(), Some(false));
        assert_eq!(gnome("default").verdict(), Some(false));
        let kde = |scheme: &str| Evidence {
            kde_scheme: text(scheme),
            ..Default::default()
        };
        assert_eq!(kde("BreezeDark").verdict(), Some(true));
        assert_eq!(kde("BreezeLight").verdict(), Some(false));
        let env = Evidence {
            gtk_env: text("Adwaita:dark"),
            ..Default::default()
        };
        assert_eq!(env.verdict(), Some(true));
    }

    /// Fuente de mentira: 0 = no se sabe, 1 = claro, 2 = oscuro.
    fn fake(
        value: Arc<AtomicU8>,
        calls: Arc<AtomicUsize>,
    ) -> impl Fn() -> Option<bool> + Send + 'static {
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            match value.load(Ordering::SeqCst) {
                0 => None,
                v => Some(v == 2),
            }
        }
    }

    fn wait_until(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "no ocurrió: {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn watcher_follows_the_system_only_when_asked() {
        let value = Arc::new(AtomicU8::new(1));
        let calls = Arc::new(AtomicUsize::new(0));
        let woken = Arc::new(AtomicUsize::new(0));
        let waker: Waker = {
            let woken = woken.clone();
            Arc::new(move || {
                woken.fetch_add(1, Ordering::SeqCst);
            })
        };
        let watcher = Watcher::with_source(
            fake(value.clone(), calls.clone()),
            waker,
            Duration::from_millis(20),
        );
        // Arranca sabiendo ya la respuesta, sin despertar a nadie.
        assert_eq!(watcher.dark(), Some(false));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // El sistema cambia: no se entera hasta que se le pide mirar.
        value.store(2, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(watcher.dark(), Some(false));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        watcher.refresh();
        wait_until("ve el modo oscuro", || watcher.dark() == Some(true));
        assert_eq!(woken.load(Ordering::SeqCst), 1);

        // Pedirlo muchas veces seguidas no multiplica las consultas.
        let before = calls.load(Ordering::SeqCst);
        for _ in 0..200 {
            watcher.refresh();
        }
        std::thread::sleep(Duration::from_millis(30));
        assert!(calls.load(Ordering::SeqCst) <= before + 3);
        assert_eq!(woken.load(Ordering::SeqCst), 1, "sin cambios no despierta");

        // Una consulta fallida no borra lo que se sabía.
        value.store(0, Ordering::SeqCst);
        let before = calls.load(Ordering::SeqCst);
        watcher.refresh();
        wait_until("vuelve a consultar", || {
            calls.load(Ordering::SeqCst) > before
        });
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(watcher.dark(), Some(true));

        // Y vuelve al claro cuando el sistema vuelve.
        value.store(1, Ordering::SeqCst);
        wait_until("ve el modo claro", || {
            watcher.refresh();
            watcher.dark() == Some(false)
        });
        assert_eq!(woken.load(Ordering::SeqCst), 2);

        // Al soltarlo, el hilo termina y no consulta más.
        drop(watcher);
        std::thread::sleep(Duration::from_millis(60));
        let after = calls.load(Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(calls.load(Ordering::SeqCst), after);
    }
}
