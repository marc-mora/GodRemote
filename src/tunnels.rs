//! Túneles SSH: reenvío local (`-L`), remoto (`-R`) y dinámico SOCKS5 (`-D`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::model::{Tunnel, TunnelKind};
use crate::ssh::SshHandle;
use crate::trf;

#[derive(Debug, Clone, PartialEq)]
pub enum TunnelStatus {
    Active,
    Failed(String),
}

/// Destinos de los reenvíos remotos activos, por puerto del servidor.
pub type RemoteForwards = Mutex<HashMap<u32, (String, u16)>>;

/// Función que informa del estado del túnel número `index`.
pub type Report = Arc<dyn Fn(usize, TunnelStatus) + Send + Sync>;

/// Túneles en marcha; al soltarlo dejan de aceptarse conexiones.
#[derive(Default)]
pub struct TunnelSet {
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for TunnelSet {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Pone en marcha los túneles de una sesión sobre una conexión ya abierta.
pub async fn start(handle: &SshHandle, tunnels: &[Tunnel], report: Report) -> TunnelSet {
    let mut set = TunnelSet::default();
    for (index, tunnel) in tunnels.iter().enumerate() {
        let result = match tunnel.kind {
            TunnelKind::Local | TunnelKind::Dynamic => listen(handle.clone(), tunnel.clone()).await,
            TunnelKind::Remote => remote(handle, tunnel).await.map(|()| None),
        };
        match result {
            Ok(task) => {
                set.tasks.extend(task);
                report(index, TunnelStatus::Active);
            }
            Err(e) => report(index, TunnelStatus::Failed(format!("{e:#}"))),
        }
    }
    set
}

async fn remote(handle: &SshHandle, tunnel: &Tunnel) -> Result<()> {
    let host = tunnel.listen_host.trim();
    match handle.tcpip_forward(host, tunnel.listen_port as u32).await {
        Ok(_) => {
            handle.shared.remote_forwards.lock().unwrap().insert(
                tunnel.listen_port as u32,
                (tunnel.target_host.trim().to_string(), tunnel.target_port),
            );
            Ok(())
        }
        Err(_) => bail!(
            "{}",
            trf!(
                "el servidor rechazó escuchar en {host}:{port} (¿AllowTcpForwarding / \
                 GatewayPorts?)",
                host,
                port = tunnel.listen_port
            )
        ),
    }
}

async fn listen(handle: SshHandle, tunnel: Tunnel) -> Result<Option<JoinHandle<()>>> {
    let addr = (tunnel.listen_host.trim().to_string(), tunnel.listen_port);
    let listener = TcpListener::bind((addr.0.as_str(), addr.1))
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "{}",
                trf!(
                    "no se pudo escuchar en {host}:{port}: {e}",
                    host = addr.0,
                    port = addr.1,
                    e
                )
            )
        })?;
    Ok(Some(tokio::spawn(async move {
        loop {
            let Ok((socket, peer)) = listener.accept().await else {
                break;
            };
            let handle = handle.clone();
            let tunnel = tunnel.clone();
            tokio::spawn(async move {
                let _ = socket.set_nodelay(true);
                let _ = serve(handle, tunnel, socket, peer).await;
            });
        }
    })))
}

async fn serve(
    handle: SshHandle,
    tunnel: Tunnel,
    mut socket: TcpStream,
    peer: std::net::SocketAddr,
) -> Result<()> {
    let (host, port) = match tunnel.kind {
        TunnelKind::Dynamic => match socks5_request(&mut socket).await {
            Ok(target) => target,
            Err(e) => return Err(e),
        },
        _ => (tunnel.target_host.trim().to_string(), tunnel.target_port),
    };
    let opened = handle
        .channel_open_direct_tcpip(host, port as u32, peer.ip().to_string(), peer.port() as u32)
        .await;
    let channel = match opened {
        Ok(c) => c,
        Err(e) => {
            if tunnel.kind == TunnelKind::Dynamic {
                // Respuesta SOCKS5: conexión rechazada.
                let _ = socket.write_all(&[5, 5, 0, 1, 0, 0, 0, 0, 0, 0]).await;
            }
            return Err(e.into());
        }
    };
    if tunnel.kind == TunnelKind::Dynamic {
        socket.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    }
    let mut stream = channel.into_stream();
    tokio::io::copy_bidirectional(&mut socket, &mut stream).await?;
    Ok(())
}

/// Negociación SOCKS5 (sin autenticación, solo CONNECT). Devuelve el destino.
pub async fn socks5_request<S>(socket: &mut S) -> Result<(String, u16)>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut head = [0u8; 2];
    socket.read_exact(&mut head).await?;
    if head[0] != 5 {
        bail!("no es SOCKS5");
    }
    let mut methods = vec![0u8; head[1] as usize];
    socket.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        socket.write_all(&[5, 0xFF]).await?;
        bail!("el cliente SOCKS exige autenticación");
    }
    socket.write_all(&[5, 0]).await?;

    let mut req = [0u8; 4];
    socket.read_exact(&mut req).await?;
    if req[0] != 5 || req[1] != 1 {
        // Orden no soportada (solo CONNECT).
        socket.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        bail!("orden SOCKS no soportada");
    }
    let host = match req[3] {
        1 => {
            let mut ip = [0u8; 4];
            socket.read_exact(&mut ip).await?;
            std::net::Ipv4Addr::from(ip).to_string()
        }
        3 => {
            let len = socket.read_u8().await? as usize;
            let mut name = vec![0u8; len];
            socket.read_exact(&mut name).await?;
            String::from_utf8(name).map_err(|_| anyhow::anyhow!("nombre de host no válido"))?
        }
        4 => {
            let mut ip = [0u8; 16];
            socket.read_exact(&mut ip).await?;
            std::net::Ipv6Addr::from(ip).to_string()
        }
        _ => {
            socket.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            bail!("tipo de dirección SOCKS no soportado");
        }
    };
    let port = socket.read_u16().await?;
    if host.is_empty() || port == 0 {
        bail!("destino SOCKS no válido");
    }
    Ok((host, port))
}

/// Atiende un canal abierto por el servidor para un reenvío remoto: lo une
/// con el destino local configurado.
pub async fn serve_remote<S>(mut channel: S, target: (String, u16)) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut local = TcpStream::connect((target.0.as_str(), target.1)).await?;
    let _ = local.set_nodelay(true);
    tokio::io::copy_bidirectional(&mut channel, &mut local).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn socks5_handshake() {
        // Dominio.
        let (mut client, mut server) = tokio::io::duplex(256);
        let task = tokio::spawn(async move { socks5_request(&mut server).await });
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut resp = [0u8; 2];
        client.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp, [5, 0]);
        client.write_all(&[5, 1, 0, 3, 11]).await.unwrap();
        client.write_all(b"example.org").await.unwrap();
        client.write_all(&443u16.to_be_bytes()).await.unwrap();
        assert_eq!(
            task.await.unwrap().unwrap(),
            ("example.org".to_string(), 443)
        );

        // IPv4.
        let (mut client, mut server) = tokio::io::duplex(256);
        let task = tokio::spawn(async move { socks5_request(&mut server).await });
        client
            .write_all(&[5, 2, 2, 0, 5, 1, 0, 1, 10, 0, 0, 7, 0, 22])
            .await
            .unwrap();
        assert_eq!(task.await.unwrap().unwrap(), ("10.0.0.7".to_string(), 22));

        // SOCKS4, autenticación obligatoria y BIND se rechazan.
        for bytes in [&[4u8, 1][..], &[5, 1, 2][..], &[5, 1, 0, 5, 2, 0, 1][..]] {
            let (mut client, mut server) = tokio::io::duplex(256);
            let task = tokio::spawn(async move { socks5_request(&mut server).await });
            client.write_all(bytes).await.unwrap();
            assert!(task.await.unwrap().is_err());
        }
    }
}
