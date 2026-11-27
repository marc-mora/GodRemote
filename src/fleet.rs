//! Ejecutar una orden en varios equipos SSH a la vez y recoger, de cada
//! uno, lo que escribe y su código de salida.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use russh::{ChannelMsg, Disconnect};
use tokio::sync::{Semaphore, watch};
use uuid::Uuid;

use crate::ssh::{Endpoint, Prompter};

/// Salida que se guarda de cada equipo; lo que pase de aquí se descarta.
pub const MAX_OUTPUT: usize = 512 * 1024;

/// Un equipo en el que ejecutar: cómo llegar a él y qué orden lanzar (ya
/// con sus variables sustituidas).
pub struct HostJob {
    pub id: Uuid,
    pub name: String,
    pub endpoint: Endpoint,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HostState {
    /// A la espera de turno.
    Waiting,
    /// Conectando; con el último mensaje de progreso.
    Connecting(String),
    Running,
    /// Terminó, con su código de salida (`None` si el servidor no lo dio).
    Done(Option<u32>),
    Failed(String),
    Cancelled,
}

impl HostState {
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            HostState::Done(_) | HostState::Failed(_) | HostState::Cancelled
        )
    }

    /// Terminó y con código 0.
    pub fn is_success(&self) -> bool {
        *self == HostState::Done(Some(0))
    }
}

pub struct HostResult {
    pub id: Uuid,
    pub name: String,
    pub state: HostState,
    /// Salida estándar y de errores, en el orden en que llegaron.
    pub output: String,
    /// Se ha descartado salida por pasar de [`MAX_OUTPUT`].
    pub truncated: bool,
    started: Option<Instant>,
    /// Lo que tardó (o lleva), desde que empezó a conectar.
    pub elapsed: Duration,
}

enum Event {
    State(usize, HostState),
    Output(usize, Vec<u8>),
    SaveSecrets(Vec<(String, String)>),
}

/// Una ejecución en curso (o terminada) sobre varios equipos.
pub struct FleetRun {
    events: std::sync::mpsc::Receiver<Event>,
    cancel: watch::Sender<bool>,
    pub hosts: Vec<HostResult>,
    /// Secretos pendientes de guardar en el baúl (los recoge la app).
    pub secrets_to_save: Vec<(String, String)>,
    /// Bytes de salida aún sin convertir a texto (UTF-8 a medias).
    partial: Vec<Vec<u8>>,
}

impl FleetRun {
    /// Procesa lo ocurrido desde la última llamada.
    pub fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::State(index, state) => {
                    let host = &mut self.hosts[index];
                    if host.started.is_none() && state != HostState::Waiting {
                        host.started = Some(Instant::now());
                    }
                    if state.is_finished() {
                        // Lo que quedara a medias se muestra como esté.
                        let rest = std::mem::take(&mut self.partial[index]);
                        host.output.push_str(&String::from_utf8_lossy(&rest));
                    }
                    host.state = state;
                }
                Event::Output(index, data) => {
                    let host = &mut self.hosts[index];
                    let room = MAX_OUTPUT.saturating_sub(host.output.len());
                    if room == 0 {
                        host.truncated = true;
                        continue;
                    }
                    let pending = &mut self.partial[index];
                    pending.extend_from_slice(&data);
                    // Solo se pasa a texto lo que ya es UTF-8 completo.
                    let valid = match std::str::from_utf8(pending) {
                        Ok(_) => pending.len(),
                        Err(e) if e.error_len().is_none() => e.valid_up_to(),
                        Err(_) => pending.len(),
                    };
                    let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
                    pending.drain(..valid);
                    if text.len() > room {
                        let mut cut = room;
                        while !text.is_char_boundary(cut) {
                            cut -= 1;
                        }
                        host.output.push_str(&text[..cut]);
                        host.truncated = true;
                    } else {
                        host.output.push_str(&text);
                    }
                }
                Event::SaveSecrets(secrets) => self.secrets_to_save.extend(secrets),
            }
        }
        for host in &mut self.hosts {
            if let Some(started) = host.started
                && !host.state.is_finished()
            {
                host.elapsed = started.elapsed();
            }
        }
    }

    /// Interrumpe lo que siga en marcha.
    pub fn cancel(&self) {
        let _ = self.cancel.send(true);
    }

    pub fn is_finished(&self) -> bool {
        self.hosts.iter().all(|h| h.state.is_finished())
    }

    /// (bien, con error, sin terminar).
    pub fn summary(&self) -> (usize, usize, usize) {
        let ok = self.hosts.iter().filter(|h| h.state.is_success()).count();
        let running = self.hosts.iter().filter(|h| !h.state.is_finished()).count();
        (ok, self.hosts.len() - ok - running, running)
    }

    /// Toda la salida en un solo texto, con una cabecera por equipo.
    pub fn report(&self) -> String {
        let mut out = String::new();
        for host in &self.hosts {
            let state = match &host.state {
                HostState::Done(Some(code)) => format!("código {code}"),
                HostState::Done(None) => "terminado".to_string(),
                HostState::Failed(error) => format!("error: {error}"),
                HostState::Cancelled => "cancelado".to_string(),
                _ => "sin terminar".to_string(),
            };
            out.push_str(&format!("===== {} ({state}) =====\n", host.name));
            out.push_str(&host.output);
            if !host.output.is_empty() && !host.output.ends_with('\n') {
                out.push('\n');
            }
            if host.truncated {
                out.push_str("[salida recortada]\n");
            }
            out.push('\n');
        }
        out
    }
}

impl Drop for FleetRun {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Lanza la orden de cada trabajo en su equipo, como mucho `parallel` a la
/// vez. Cada equipo tiene `timeout` para conectar y terminar.
pub fn run(
    rt: &tokio::runtime::Handle,
    jobs: Vec<HostJob>,
    prompter: Prompter,
    parallel: usize,
    timeout: Duration,
) -> FleetRun {
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let waker = prompter.waker();
    let emit: Arc<dyn Fn(Event) + Send + Sync> = Arc::new(move |event| {
        let _ = event_tx.send(event);
        waker();
    });
    let hosts = jobs
        .iter()
        .map(|job| HostResult {
            id: job.id,
            name: job.name.clone(),
            state: HostState::Waiting,
            output: String::new(),
            truncated: false,
            started: None,
            elapsed: Duration::ZERO,
        })
        .collect::<Vec<_>>();
    let partial = vec![vec![]; hosts.len()];
    let turns = Arc::new(Semaphore::new(parallel.max(1)));
    for (index, job) in jobs.into_iter().enumerate() {
        let emit = emit.clone();
        let prompter = prompter.clone();
        let turns = turns.clone();
        let mut cancelled = cancel_rx.clone();
        rt.spawn(async move {
            let work = async {
                let Ok(_turn) = turns.acquire().await else {
                    return HostState::Cancelled;
                };
                match tokio::time::timeout(timeout, run_host(index, &job, &prompter, &emit)).await {
                    Ok(Ok(code)) => HostState::Done(code),
                    Ok(Err(e)) => HostState::Failed(format!("{e:#}")),
                    Err(_) => {
                        HostState::Failed(format!("no terminó en {} s", timeout.as_secs().max(1)))
                    }
                }
            };
            let state = tokio::select! {
                state = work => state,
                _ = cancelled.wait_for(|stop| *stop) => HostState::Cancelled,
            };
            emit(Event::State(index, state));
        });
    }
    FleetRun {
        events: event_rx,
        cancel: cancel_tx,
        hosts,
        secrets_to_save: vec![],
        partial,
    }
}

/// Conecta, ejecuta la orden y devuelve su código de salida.
async fn run_host(
    index: usize,
    job: &HostJob,
    prompter: &Prompter,
    emit: &Arc<dyn Fn(Event) + Send + Sync>,
) -> Result<Option<u32>> {
    emit(Event::State(
        index,
        HostState::Connecting("Conectando…".into()),
    ));
    let status = {
        let emit = emit.clone();
        move |text: String| emit(Event::State(index, HostState::Connecting(text)))
    };
    let connected = crate::ssh::connect_endpoint(&job.endpoint, prompter, &status).await?;
    if !connected.to_save.is_empty() {
        emit(Event::SaveSecrets(connected.to_save));
    }
    let handle = connected.handle;
    let outcome = async {
        let mut channel = handle.channel_open_session().await?;
        channel.exec(true, job.command.as_bytes()).await?;
        emit(Event::State(index, HostState::Running));
        let mut code = None;
        let mut accepted = false;
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Success) => accepted = true,
                Some(ChannelMsg::Failure) if !accepted => {
                    bail!("el servidor no aceptó ejecutar la orden")
                }
                Some(ChannelMsg::Data { data }) => emit(Event::Output(index, data.to_vec())),
                Some(ChannelMsg::ExtendedData { data, .. }) => {
                    emit(Event::Output(index, data.to_vec()))
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => code = Some(exit_status),
                // Murió por una señal: como en la shell, 128 + algo.
                Some(ChannelMsg::ExitSignal { .. }) => code = Some(128),
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            }
        }
        Ok(code)
    }
    .await;
    let _ = handle.disconnect(Disconnect::ByApplication, "", "es").await;
    outcome
}
