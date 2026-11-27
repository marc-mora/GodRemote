//! Reenvío del agente SSH (`ssh -A`): el equipo remoto habla con el agente
//! de este equipo a través de un canal de la conexión.

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncWrite};

/// Une un canal abierto por el servidor con el agente SSH local. El
/// protocolo del agente pasa tal cual: aquí no se mira ni se guarda nada.
pub async fn bridge<S>(mut channel: S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    #[cfg(unix)]
    {
        let socket = std::env::var_os("SSH_AUTH_SOCK")
            .filter(|path| !path.is_empty())
            .context("no hay agente SSH (variable SSH_AUTH_SOCK sin definir)")?;
        let mut agent = tokio::net::UnixStream::connect(&socket)
            .await
            .context("no se pudo conectar con el agente SSH")?;
        tokio::io::copy_bidirectional(&mut channel, &mut agent).await?;
    }
    #[cfg(windows)]
    {
        // El agente de OpenSSH para Windows. Pageant no ofrece un canal de
        // este tipo y no se puede reenviar.
        let mut agent = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(r"\\.\pipe\openssh-ssh-agent")
            .context("no se pudo conectar con el agente de OpenSSH (¿servicio ssh-agent?)")?;
        tokio::io::copy_bidirectional(&mut channel, &mut agent).await?;
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = &mut channel;
        anyhow::bail!("reenvío del agente no disponible en este sistema");
    }
    Ok(())
}

/// Hay un agente SSH local al que reenviar.
pub fn available() -> bool {
    if cfg!(unix) {
        std::env::var_os("SSH_AUTH_SOCK").is_some_and(|path| !path.is_empty())
    } else {
        cfg!(windows)
    }
}
