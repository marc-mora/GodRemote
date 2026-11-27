//! Consola por puerto serie (COM / ttyUSB / ttyS).

use std::io::{Read, Write};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::mpsc;

use crate::model::{SerialFlow, SerialOptions, SerialParity};
use crate::ssh::Waker;
use crate::terminal::{TermBackend, TermCmd, TermEvent};

/// Puertos serie detectados en el equipo.
pub fn available_ports() -> Vec<String> {
    let mut ports: Vec<String> = serialport::available_ports()
        .map(|list| list.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default();
    ports.sort();
    ports
}

fn open(options: &SerialOptions) -> Result<Box<dyn serialport::SerialPort>> {
    use serialport::{DataBits, FlowControl, Parity, StopBits};
    let data_bits = match options.data_bits {
        5 => DataBits::Five,
        6 => DataBits::Six,
        7 => DataBits::Seven,
        _ => DataBits::Eight,
    };
    let parity = match options.parity {
        SerialParity::None => Parity::None,
        SerialParity::Odd => Parity::Odd,
        SerialParity::Even => Parity::Even,
    };
    let stop_bits = if options.stop_bits == 2 {
        StopBits::Two
    } else {
        StopBits::One
    };
    let flow = match options.flow {
        SerialFlow::None => FlowControl::None,
        SerialFlow::Software => FlowControl::Software,
        SerialFlow::Hardware => FlowControl::Hardware,
    };
    serialport::new(options.device.trim(), options.baud)
        .data_bits(data_bits)
        .parity(parity)
        .stop_bits(stop_bits)
        .flow_control(flow)
        .timeout(Duration::from_millis(100))
        .open()
        .with_context(|| format!("No se pudo abrir el puerto {}", options.device.trim()))
}

/// Abre el puerto serie y lo conecta a un terminal.
pub fn spawn(options: &SerialOptions, waker: Waker) -> Result<TermBackend> {
    let mut reader = open(options)?;
    let mut writer = reader
        .try_clone()
        .context("No se pudo duplicar el puerto serie")?;

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<TermCmd>();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    let _ = ev_tx.send(TermEvent::Connected(None));
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Lector: puerto -> terminal.
    let stop_reader = stop.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let error = loop {
            if stop_reader.load(std::sync::atomic::Ordering::Relaxed) {
                break None;
            }
            match reader.read(&mut buf) {
                Ok(0) => continue,
                Ok(n) => {
                    if ev_tx.send(TermEvent::Data(buf[..n].to_vec())).is_err() {
                        return;
                    }
                    waker();
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => break Some(format!("Error en el puerto serie: {e}")),
            }
        };
        let _ = ev_tx.send(TermEvent::Closed(error));
        waker();
    });

    // Escritor: teclado -> puerto.
    std::thread::spawn(move || {
        while let Some(cmd) = cmd_rx.blocking_recv() {
            match cmd {
                TermCmd::Input(data) => {
                    if writer
                        .write_all(&data)
                        .and_then(|_| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
                TermCmd::Resize { .. } => {}
                TermCmd::Close => break,
            }
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    });

    Ok(TermBackend {
        cmd: cmd_tx,
        events: ev_rx,
    })
}

// (Solo en Linux: macOS no deja abrir un pseudo-terminal como puerto serie.)
#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::time::Instant;

    /// Un pseudo-terminal hace de «cable»: lo que se escribe en un extremo
    /// sale por el otro, como en un puerto serie.
    #[test]
    fn serial_roundtrip_over_pty() {
        use portable_pty::{PtySize, native_pty_system};
        let pair = native_pty_system().openpty(PtySize::default()).unwrap();
        let device = pair.master.tty_name().expect("nombre del pty");
        let mut far_reader = pair.master.try_clone_reader().unwrap();
        let mut far_writer = pair.master.take_writer().unwrap();

        let options = SerialOptions {
            device: device.display().to_string(),
            baud: 9600,
            ..Default::default()
        };
        let backend = spawn(&options, std::sync::Arc::new(|| {})).unwrap();
        assert!(matches!(
            backend.events.recv_timeout(Duration::from_secs(5)),
            Ok(TermEvent::Connected(None))
        ));

        // Del «dispositivo» al terminal.
        far_writer.write_all(b"router> ").unwrap();
        far_writer.flush().unwrap();
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !got.ends_with(b"router> ") {
            match backend
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(TermEvent::Data(d)) => got.extend(d),
                Ok(_) => {}
                Err(_) => panic!("no llegaron datos: {got:?}"),
            }
        }

        // Del terminal al «dispositivo».
        backend
            .cmd
            .send(TermCmd::Input(b"show version\r".to_vec()))
            .unwrap();
        let mut buf = [0u8; 64];
        let mut received = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !received.ends_with(b"show version\r") && Instant::now() < deadline {
            let n = far_reader.read(&mut buf).unwrap();
            received.extend_from_slice(&buf[..n]);
        }
        assert!(received.ends_with(b"show version\r"), "{received:?}");

        // Un dispositivo inexistente da un error claro.
        let bad = SerialOptions {
            device: "/dev/no-existe-godremote".into(),
            ..Default::default()
        };
        let err = spawn(&bad, std::sync::Arc::new(|| {})).err().unwrap();
        assert!(format!("{err:#}").contains("No se pudo abrir"));
    }
}
