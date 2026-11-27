//! Gestor de túneles: túneles SSH guardados que se arrancan y se paran por
//! su cuenta, cada uno sobre su propia conexión y sin abrir un terminal.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use russh::Disconnect;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::model::Tunnel;
use crate::ssh::{Endpoint, Prompter};
use crate::tunnels::{self, TunnelStatus};

/// Un túnel guardado, independiente de cualquier terminal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTunnel {
    pub id: Uuid,
    #[serde(rename = "nombre")]
    pub name: String,
    /// Equipo SSH del árbol por el que pasa (con sus saltos, si los tiene).
    #[serde(rename = "equipo")]
    pub session: Option<Uuid>,
    #[serde(rename = "tunel")]
    pub tunnel: Tunnel,
    /// Ponerlo en marcha al abrir la aplicación.
    #[serde(rename = "autoarranque")]
    pub autostart: bool,
}

impl Default for SavedTunnel {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            session: None,
            tunnel: Tunnel::default(),
            autostart: false,
        }
    }
}

impl SavedTunnel {
    /// Nombre para mostrar: el suyo o, si no tiene, lo que hace.
    pub fn display_name(&self) -> String {
        match self.name.trim() {
            "" => self.tunnel.describe(),
            name => name.to_string(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.session.is_none() {
            return Err("Elige el equipo SSH por el que pasa el túnel".into());
        }
        self.tunnel.validate()
    }
}

/// Los túneles guardados (`tunnels.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TunnelStore {
    #[serde(default, rename = "tuneles")]
    pub tunnels: Vec<SavedTunnel>,
}

impl TunnelStore {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::paths::write_private(path, &serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    pub fn get(&self, id: Uuid) -> Option<&SavedTunnel> {
        self.tunnels.iter().find(|t| t.id == id)
    }

    /// Guarda un túnel nuevo o sustituye al que tenga su identificador.
    pub fn upsert(&mut self, tunnel: SavedTunnel) {
        match self.tunnels.iter_mut().find(|t| t.id == tunnel.id) {
            Some(existing) => *existing = tunnel,
            None => self.tunnels.push(tunnel),
        }
    }

    pub fn remove(&mut self, id: Uuid) {
        self.tunnels.retain(|t| t.id != id);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunState {
    /// Conectando; con el último mensaje de progreso.
    Connecting(String),
    Active,
    /// Parado por el usuario.
    Stopped,
    Failed(String),
}

enum Event {
    Status(String),
    Active,
    SaveSecrets(Vec<(String, String)>),
    Closed(Option<String>),
}

/// Un túnel en marcha (o que lo estuvo) sobre su propia conexión SSH.
pub struct RunningTunnel {
    events: std::sync::mpsc::Receiver<Event>,
    stop: Option<oneshot::Sender<()>>,
    pub state: RunState,
    /// Llegó a estar activo: si después falla es una caída de la conexión.
    pub was_active: bool,
    /// Secretos pendientes de guardar en el baúl (los recoge la app).
    pub secrets_to_save: Vec<(String, String)>,
}

impl RunningTunnel {
    /// Procesa lo que haya ocurrido desde la última llamada.
    pub fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Status(text) => {
                    if matches!(self.state, RunState::Connecting(_)) {
                        self.state = RunState::Connecting(text);
                    }
                }
                Event::Active => {
                    self.state = RunState::Active;
                    self.was_active = true;
                }
                Event::SaveSecrets(secrets) => self.secrets_to_save.extend(secrets),
                Event::Closed(None) => self.state = RunState::Stopped,
                Event::Closed(Some(error)) => self.state = RunState::Failed(error),
            }
        }
    }

    /// Cierra el túnel y su conexión.
    pub fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }

    /// Conectando o activo.
    pub fn is_running(&self) -> bool {
        matches!(self.state, RunState::Connecting(_) | RunState::Active)
    }
}

impl Drop for RunningTunnel {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Abre una conexión SSH propia y pone en marcha el túnel sobre ella.
pub fn spawn(
    rt: &tokio::runtime::Handle,
    endpoint: Endpoint,
    tunnel: Tunnel,
    prompter: Prompter,
) -> RunningTunnel {
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = oneshot::channel();
    let waker = prompter.waker();
    rt.spawn(async move {
        let emit = move |event: Event| {
            let _ = event_tx.send(event);
            waker();
        };
        let result = run(&endpoint, &tunnel, &prompter, &emit, stop_rx).await;
        emit(Event::Closed(result.err().map(|e| format!("{e:#}"))));
    });
    RunningTunnel {
        events: event_rx,
        stop: Some(stop_tx),
        state: RunState::Connecting("Conectando…".into()),
        was_active: false,
        secrets_to_save: vec![],
    }
}

async fn run(
    endpoint: &Endpoint,
    tunnel: &Tunnel,
    prompter: &Prompter,
    emit: &(dyn Fn(Event) + Send + Sync),
    mut stop: oneshot::Receiver<()>,
) -> Result<()> {
    let status = |text: String| emit(Event::Status(text));
    let connected = tokio::select! {
        connected = crate::ssh::connect_endpoint(endpoint, prompter, &status) => connected?,
        // Parado (o descartado) antes de terminar de conectar.
        _ = &mut stop => return Ok(()),
    };
    if !connected.to_save.is_empty() {
        emit(Event::SaveSecrets(connected.to_save));
    }
    let handle = connected.handle;

    let failure = Arc::new(Mutex::new(None));
    let report: tunnels::Report = {
        let failure = failure.clone();
        Arc::new(move |_, status| {
            if let TunnelStatus::Failed(error) = status {
                *failure.lock().unwrap() = Some(error);
            }
        })
    };
    let set = tunnels::start(&handle, std::slice::from_ref(tunnel), report).await;
    let failure = failure.lock().unwrap().take();
    let outcome = match failure {
        Some(error) => Err(anyhow::anyhow!(error)),
        None => {
            emit(Event::Active);
            // En marcha hasta que se pare o se caiga la conexión.
            loop {
                tokio::select! {
                    _ = &mut stop => break Ok(()),
                    _ = tokio::time::sleep(Duration::from_millis(500)) => {
                        if handle.is_closed() {
                            break Err(anyhow::anyhow!("Se perdió la conexión con el servidor"));
                        }
                    }
                }
            }
        }
    };
    drop(set);
    tokio::task::yield_now().await;
    if !handle.is_closed() {
        let _ = handle.disconnect(Disconnect::ByApplication, "", "es").await;
    }
    if let Err(error) = &outcome
        && error.to_string().is_empty()
    {
        bail!("El túnel no se pudo abrir");
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TunnelKind;

    #[test]
    fn store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tunnels.json");
        // Sin fichero, vacío.
        assert!(TunnelStore::load(&path).tunnels.is_empty());

        let mut store = TunnelStore::default();
        let session = Uuid::new_v4();
        let mut web = SavedTunnel {
            name: "Web interna".into(),
            session: Some(session),
            autostart: true,
            ..Default::default()
        };
        store.upsert(web.clone());
        store.upsert(SavedTunnel {
            session: Some(session),
            tunnel: Tunnel {
                kind: TunnelKind::Dynamic,
                listen_port: 1080,
                ..Default::default()
            },
            ..Default::default()
        });
        store.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"tuneles\"") && text.contains("\"autoarranque\": true"));
        assert_eq!(TunnelStore::load(&path), store);

        // Guardar de nuevo el mismo sustituye, no duplica.
        web.tunnel.listen_port = 9000;
        store.upsert(web.clone());
        assert_eq!(store.tunnels.len(), 2);
        assert_eq!(store.get(web.id).unwrap().tunnel.listen_port, 9000);
        assert_eq!(store.tunnels[0].display_name(), "Web interna");
        assert_eq!(
            store.tunnels[1].display_name(),
            "-D 127.0.0.1:1080 (SOCKS5)"
        );
        store.remove(web.id);
        assert!(store.get(web.id).is_none());
    }

    #[test]
    fn a_tunnel_needs_a_host_and_valid_ports() {
        let mut tunnel = SavedTunnel::default();
        assert!(tunnel.validate().unwrap_err().contains("equipo"));
        tunnel.session = Some(Uuid::new_v4());
        assert!(tunnel.validate().is_ok());
        tunnel.tunnel.target_host.clear();
        assert!(tunnel.validate().is_err());
        tunnel.tunnel.kind = TunnelKind::Dynamic;
        assert!(tunnel.validate().is_ok());
    }
}
