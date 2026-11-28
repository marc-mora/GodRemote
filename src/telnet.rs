//! Cliente Telnet: negociación mínima de opciones (tipo de terminal, tamaño
//! de ventana, eco) sobre una conexión TCP **sin cifrar**.

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use crate::ssh::{Endpoint, Prompter, Route, Waker};
use crate::terminal::{TermBackend, TermCmd, TermEvent};

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;

const OPT_BINARY: u8 = 0;
const OPT_ECHO: u8 = 1;
const OPT_SGA: u8 = 3;
const OPT_TTYPE: u8 = 24;
const OPT_NAWS: u8 = 31;

const TERMINAL_TYPE: &[u8] = b"xterm-256color";
/// Límite defensivo para una subnegociación enviada por el servidor.
const MAX_SUBNEGOTIATION: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Data,
    /// Se recibió CR: un NUL posterior se descarta.
    DataAfterCr,
    Iac,
    Negotiate(u8),
    Sub,
    SubIac,
}

/// Máquina de estados del protocolo Telnet (sin E/S, para poder probarla).
pub struct TelnetCodec {
    state: State,
    sub: Vec<u8>,
    cols: u16,
    rows: u16,
    /// Opciones que hemos aceptado activar en nuestro lado.
    local: [bool; 256],
    /// Opciones que hemos aceptado que active el servidor.
    remote: [bool; 256],
}

impl TelnetCodec {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            state: State::Data,
            sub: vec![],
            cols,
            rows,
            local: [false; 256],
            remote: [false; 256],
        }
    }

    fn naws(&self) -> Vec<u8> {
        let mut out = vec![IAC, SB, OPT_NAWS];
        for b in self
            .cols
            .to_be_bytes()
            .into_iter()
            .chain(self.rows.to_be_bytes())
        {
            out.push(b);
            if b == IAC {
                out.push(IAC);
            }
        }
        out.extend_from_slice(&[IAC, SE]);
        out
    }

    /// Nuevo tamaño de ventana; devuelve lo que hay que enviar al servidor.
    pub fn resize(&mut self, cols: u16, rows: u16) -> Vec<u8> {
        self.cols = cols;
        self.rows = rows;
        if self.local[OPT_NAWS as usize] {
            self.naws()
        } else {
            vec![]
        }
    }

    /// Prepara datos del usuario para enviarlos (escapa IAC; CR → CR NUL).
    pub fn encode(&self, input: &[u8]) -> Vec<u8> {
        let binary = self.local[OPT_BINARY as usize];
        let mut out = Vec::with_capacity(input.len() + 4);
        for &b in input {
            out.push(b);
            if b == IAC {
                out.push(IAC);
            } else if b == b'\r' && !binary {
                out.push(0);
            }
        }
        out
    }

    /// Procesa bytes recibidos: `data` recibe el texto para el terminal y
    /// `reply` las respuestas de negociación que hay que enviar.
    pub fn receive(&mut self, input: &[u8], data: &mut Vec<u8>, reply: &mut Vec<u8>) {
        for &b in input {
            match self.state {
                State::Data | State::DataAfterCr => {
                    let after_cr = self.state == State::DataAfterCr;
                    self.state = State::Data;
                    match b {
                        IAC => self.state = State::Iac,
                        0 if after_cr && !self.remote[OPT_BINARY as usize] => {}
                        b'\r' => {
                            data.push(b);
                            self.state = State::DataAfterCr;
                        }
                        _ => data.push(b),
                    }
                }
                State::Iac => match b {
                    IAC => {
                        data.push(IAC);
                        self.state = State::Data;
                    }
                    DO | DONT | WILL | WONT => self.state = State::Negotiate(b),
                    SB => {
                        self.sub.clear();
                        self.state = State::Sub;
                    }
                    // NOP, GA, etc.: se ignoran.
                    _ => self.state = State::Data,
                },
                State::Negotiate(cmd) => {
                    self.negotiate(cmd, b, reply);
                    self.state = State::Data;
                }
                State::Sub => {
                    if b == IAC {
                        self.state = State::SubIac;
                    } else if self.sub.len() < MAX_SUBNEGOTIATION {
                        self.sub.push(b);
                    }
                }
                State::SubIac => match b {
                    SE => {
                        self.subnegotiation(reply);
                        self.state = State::Data;
                    }
                    IAC => {
                        if self.sub.len() < MAX_SUBNEGOTIATION {
                            self.sub.push(IAC);
                        }
                        self.state = State::Sub;
                    }
                    _ => self.state = State::Data,
                },
            }
        }
    }

    fn negotiate(&mut self, cmd: u8, opt: u8, reply: &mut Vec<u8>) {
        let i = opt as usize;
        match cmd {
            DO => {
                let accept = matches!(opt, OPT_TTYPE | OPT_NAWS | OPT_SGA | OPT_BINARY);
                // Solo se contesta si cambia el estado (evita bucles).
                if accept != self.local[i] || !accept {
                    self.local[i] = accept;
                    reply.extend_from_slice(&[IAC, if accept { WILL } else { WONT }, opt]);
                    if accept && opt == OPT_NAWS {
                        reply.extend(self.naws());
                    }
                }
            }
            DONT => {
                if self.local[i] {
                    self.local[i] = false;
                    reply.extend_from_slice(&[IAC, WONT, opt]);
                }
            }
            WILL => {
                let accept = matches!(opt, OPT_ECHO | OPT_SGA | OPT_BINARY);
                if accept != self.remote[i] || !accept {
                    self.remote[i] = accept;
                    reply.extend_from_slice(&[IAC, if accept { DO } else { DONT }, opt]);
                }
            }
            WONT if self.remote[i] => {
                self.remote[i] = false;
                reply.extend_from_slice(&[IAC, DONT, opt]);
            }
            _ => {}
        }
    }

    fn subnegotiation(&mut self, reply: &mut Vec<u8>) {
        // El servidor pregunta el tipo de terminal: IAC SB TTYPE SEND IAC SE.
        if self.sub.as_slice() == [OPT_TTYPE, 1] && self.local[OPT_TTYPE as usize] {
            reply.extend_from_slice(&[IAC, SB, OPT_TTYPE, 0]);
            reply.extend_from_slice(TERMINAL_TYPE);
            reply.extend_from_slice(&[IAC, SE]);
        }
        self.sub.clear();
    }
}

/// Abre una sesión Telnet directa en segundo plano.
pub fn spawn(
    rt: &tokio::runtime::Handle,
    host: String,
    port: u16,
    waker: Waker,
    cols: u16,
    rows: u16,
) -> TermBackend {
    // Sin saltos no hay nada que preguntar al usuario.
    let (tx, _rx) = std::sync::mpsc::channel();
    spawn_via(rt, host, port, None, Prompter::new(tx, waker), cols, rows)
}

/// Abre una sesión Telnet, opcionalmente a través de saltos SSH (`via` es
/// el último salto de la cadena).
pub fn spawn_via(
    rt: &tokio::runtime::Handle,
    host: String,
    port: u16,
    via: Option<Endpoint>,
    prompter: Prompter,
    cols: u16,
    rows: u16,
) -> TermBackend {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    rt.spawn(async move {
        let emit = {
            let prompter = prompter.clone();
            move |ev: TermEvent| {
                let _ = ev_tx.send(ev);
                prompter.wake();
            }
        };
        let result = run(&host, port, via, &prompter, cmd_rx, cols, rows, &emit).await;
        emit(TermEvent::Closed(result.err().map(|e| format!("{e:#}"))));
    });
    TermBackend {
        cmd: cmd_tx,
        events: ev_rx,
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    host: &str,
    port: u16,
    via: Option<Endpoint>,
    prompter: &Prompter,
    mut cmd_rx: mpsc::UnboundedReceiver<TermCmd>,
    cols: u16,
    rows: u16,
    emit: &(dyn Fn(TermEvent) + Send + Sync),
) -> Result<()> {
    // La cadena de saltos (si la hay) vive mientras dure la sesión.
    let status = |s: String| emit(TermEvent::Status(s));
    let (route, to_save) = Route::establish(via.as_ref(), prompter, &status).await?;
    if !to_save.is_empty() {
        emit(TermEvent::SaveSecrets(to_save));
    }
    emit(TermEvent::Status(if route.is_direct() {
        crate::trf!(
            "Conectando a {host}:{port} por Telnet (sin cifrar)…",
            host,
            port
        )
    } else {
        crate::trf!(
            "Conectando a {host}:{port} por Telnet (sin cifrar tras el último salto)…",
            host,
            port
        )
    }));
    let mut stream = route.connect(host, port).await?;
    emit(TermEvent::Connected(None));

    let mut codec = TelnetCodec::new(cols, rows);
    let mut buf = vec![0u8; 8192];
    let mut data = Vec::new();
    let mut reply = Vec::new();
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(TermCmd::Input(bytes)) => stream.write_all(&codec.encode(&bytes)).await?,
                Some(TermCmd::Resize { cols, rows }) => {
                    let out = codec.resize(cols, rows);
                    if !out.is_empty() {
                        stream.write_all(&out).await?;
                    }
                }
                Some(TermCmd::Close) | None => break,
            },
            n = stream.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    break;
                }
                data.clear();
                reply.clear();
                codec.receive(&buf[..n], &mut data, &mut reply);
                if !reply.is_empty() {
                    stream.write_all(&reply).await?;
                }
                if !data.is_empty() {
                    emit(TermEvent::Data(data.clone()));
                }
            }
        }
    }
    let _ = stream.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(codec: &mut TelnetCodec, input: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let (mut data, mut reply) = (vec![], vec![]);
        codec.receive(input, &mut data, &mut reply);
        (data, reply)
    }

    #[test]
    fn negotiation() {
        let mut c = TelnetCodec::new(80, 24);
        // El servidor pide tipo de terminal y tamaño, y anuncia eco.
        let (data, reply) = feed(
            &mut c,
            &[
                IAC, DO, OPT_TTYPE, IAC, DO, OPT_NAWS, IAC, WILL, OPT_ECHO, b'h', b'i',
            ],
        );
        assert_eq!(data, b"hi");
        assert_eq!(
            reply,
            [
                IAC, WILL, OPT_TTYPE, IAC, WILL, OPT_NAWS, IAC, SB, OPT_NAWS, 0, 80, 0, 24, IAC,
                SE, IAC, DO, OPT_ECHO
            ]
        );
        // Repetir la petición no provoca otra respuesta (sin bucles).
        assert!(
            feed(&mut c, &[IAC, DO, OPT_TTYPE, IAC, WILL, OPT_ECHO])
                .1
                .is_empty()
        );
        // Tipo de terminal.
        let (_, reply) = feed(&mut c, &[IAC, SB, OPT_TTYPE, 1, IAC, SE]);
        let mut expected = vec![IAC, SB, OPT_TTYPE, 0];
        expected.extend_from_slice(TERMINAL_TYPE);
        expected.extend_from_slice(&[IAC, SE]);
        assert_eq!(reply, expected);
        // Opciones desconocidas se rechazan.
        assert_eq!(feed(&mut c, &[IAC, DO, 39]).1, [IAC, WONT, 39]);
        assert_eq!(feed(&mut c, &[IAC, WILL, 5]).1, [IAC, DONT, 5]);
        // Cambio de tamaño (255 columnas obliga a duplicar el IAC).
        assert_eq!(
            c.resize(255, 50),
            [IAC, SB, OPT_NAWS, 0, IAC, IAC, 0, 50, IAC, SE]
        );
    }

    #[test]
    fn data_escaping_and_split_sequences() {
        let mut c = TelnetCodec::new(80, 24);
        // IAC IAC es un 0xFF literal; CR NUL es un CR; secuencia partida.
        let (d1, _) = feed(&mut c, &[b'a', IAC]);
        let (d2, _) = feed(&mut c, &[IAC, b'\r', 0, b'\r', b'\n', IAC]);
        let (d3, r3) = feed(&mut c, &[WILL, OPT_SGA, b'z']);
        assert_eq!(d1, b"a");
        assert_eq!(d2, [IAC, b'\r', b'\r', b'\n']);
        assert_eq!(d3, b"z");
        assert_eq!(r3, [IAC, DO, OPT_SGA]);
        assert_eq!(c.encode(&[b'x', IAC, b'\r']), [b'x', IAC, IAC, b'\r', 0]);
        // Una subnegociación enorme no hace crecer la memoria sin límite.
        let mut big = vec![IAC, SB, 99];
        big.extend(std::iter::repeat_n(b'A', 100_000));
        feed(&mut c, &big);
        assert!(c.sub.len() <= MAX_SUBNEGOTIATION);
    }
}
