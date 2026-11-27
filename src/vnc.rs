//! Cliente VNC (protocolo RFB 3.3 / 3.7 / 3.8) para el escritorio integrado.
//!
//! Admite autenticación «None» y «VNC» (contraseña) y las codificaciones
//! Raw, CopyRect, Hextile y ZRLE. La conexión VNC **no va cifrada**.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::desktop::{
    DesktopBackend, DesktopEvent, DesktopInput, Framebuffer, MouseButton, SharedFrame, char_keysym,
};
use crate::model::Session;
use crate::ssh::{Endpoint, Prompter, Route};

pub const ENC_RAW: i32 = 0;
pub const ENC_COPYRECT: i32 = 1;
pub const ENC_HEXTILE: i32 = 5;
pub const ENC_ZRLE: i32 = 16;
const ENC_DESKTOP_SIZE: i32 = -223;

/// Codificaciones que se piden al servidor, por orden de preferencia.
pub const DEFAULT_ENCODINGS: [i32; 4] = [ENC_ZRLE, ENC_HEXTILE, ENC_COPYRECT, ENC_RAW];

/// Rectángulos recibidos con cada codificación (Raw, CopyRect, Hextile, ZRLE)
/// desde que arrancó el proceso; sirve para diagnóstico y pruebas.
pub static RECTS_BY_ENCODING: [std::sync::atomic::AtomicU64; 4] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];

/// Límites defensivos frente a un servidor malicioso o averiado.
const MAX_DIMENSION: u32 = 16_384;
const MAX_TEXT: usize = 1024 * 1024;
const MAX_ZRLE_INPUT: usize = 64 * 1024 * 1024;

type Emit = Arc<dyn Fn(DesktopEvent) + Send + Sync>;

/// Respuesta al desafío de la autenticación VNC: DES con la contraseña
/// (8 bytes, cada uno con los bits invertidos) como clave.
pub fn vnc_auth_response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    use des::cipher::{Array, BlockCipherEncrypt, KeyInit};
    let mut key = [0u8; 8];
    for (k, b) in key.iter_mut().zip(password.bytes()) {
        *k = b.reverse_bits();
    }
    let cipher = des::Des::new(&Array::from(key));
    let mut out = *challenge;
    for chunk in out.chunks_exact_mut(8) {
        let mut block = Array::from(<[u8; 8]>::try_from(&*chunk).unwrap_or_default());
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    out
}

/// Decodifica un rectángulo Hextile a RGBA (`w * h * 4` bytes).
pub async fn decode_hextile<R: AsyncRead + Unpin>(
    rd: &mut R,
    w: usize,
    h: usize,
) -> Result<Vec<u8>> {
    let mut out = vec![255u8; w * h * 4];
    let mut bg = [0u8; 3];
    let mut fg = [0u8; 3];
    let mut px = [0u8; 4];
    let fill = |out: &mut [u8], x: usize, y: usize, fw: usize, fh: usize, c: [u8; 3]| {
        for row in y..y + fh {
            for col in x..x + fw {
                let i = (row * w + col) * 4;
                out[i..i + 3].copy_from_slice(&c);
            }
        }
    };
    for ty in (0..h).step_by(16) {
        for tx in (0..w).step_by(16) {
            let (tw, th) = (16.min(w - tx), 16.min(h - ty));
            let sub = rd.read_u8().await?;
            if sub & 1 != 0 {
                // Tesela en bruto.
                let mut raw = vec![0u8; tw * th * 4];
                rd.read_exact(&mut raw).await?;
                for row in 0..th {
                    for col in 0..tw {
                        let s = (row * tw + col) * 4;
                        let d = ((ty + row) * w + tx + col) * 4;
                        out[d..d + 3].copy_from_slice(&raw[s..s + 3]);
                    }
                }
                continue;
            }
            if sub & 2 != 0 {
                rd.read_exact(&mut px).await?;
                bg = [px[0], px[1], px[2]];
            }
            fill(&mut out, tx, ty, tw, th, bg);
            if sub & 4 != 0 {
                rd.read_exact(&mut px).await?;
                fg = [px[0], px[1], px[2]];
            }
            if sub & 8 != 0 {
                let count = rd.read_u8().await?;
                for _ in 0..count {
                    let color = if sub & 16 != 0 {
                        rd.read_exact(&mut px).await?;
                        [px[0], px[1], px[2]]
                    } else {
                        fg
                    };
                    let xy = rd.read_u8().await?;
                    let wh = rd.read_u8().await?;
                    let (sx, sy) = ((xy >> 4) as usize, (xy & 15) as usize);
                    let (sw, sh) = ((wh >> 4) as usize + 1, (wh & 15) as usize + 1);
                    if sx + sw > tw || sy + sh > th {
                        bail!("subrectángulo Hextile fuera de la tesela");
                    }
                    fill(&mut out, tx + sx, ty + sy, sw, sh, color);
                }
            }
        }
    }
    Ok(out)
}

/// Decodifica los datos (ya descomprimidos) de un rectángulo ZRLE a RGBA.
pub fn decode_zrle(data: &[u8], w: usize, h: usize) -> Result<Vec<u8>> {
    let mut out = vec![255u8; w * h * 4];
    let mut pos = 0usize;
    let truncated = || anyhow!("datos ZRLE truncados");
    let mut take = |n: usize| -> Result<&[u8]> {
        let slice = data.get(pos..pos + n).ok_or_else(truncated)?;
        pos += n;
        Ok(slice)
    };
    for ty in (0..h).step_by(64) {
        for tx in (0..w).step_by(64) {
            let (tw, th) = (64.min(w - tx), 64.min(h - ty));
            let total = tw * th;
            let put = |out: &mut [u8], index: usize, c: &[u8]| {
                let (row, col) = (index / tw, index % tw);
                let i = ((ty + row) * w + tx + col) * 4;
                out[i..i + 3].copy_from_slice(&c[..3]);
            };
            let sub = take(1)?[0];
            match sub {
                0 => {
                    let raw = take(total * 3)?;
                    for i in 0..total {
                        put(&mut out, i, &raw[i * 3..]);
                    }
                }
                1 => {
                    let c = take(3)?.to_vec();
                    for i in 0..total {
                        put(&mut out, i, &c);
                    }
                }
                2..=16 => {
                    let size = sub as usize;
                    let palette = take(size * 3)?.to_vec();
                    let bits = match size {
                        2 => 1,
                        3..=4 => 2,
                        _ => 4,
                    };
                    let row_bytes = (tw * bits).div_ceil(8);
                    for row in 0..th {
                        let packed = take(row_bytes)?;
                        for col in 0..tw {
                            let bit = col * bits;
                            let shift = 8 - bits - (bit % 8);
                            let idx = ((packed[bit / 8] >> shift) & ((1 << bits) - 1)) as usize;
                            let c = palette
                                .get(idx * 3..idx * 3 + 3)
                                .ok_or_else(|| anyhow!("índice de paleta ZRLE no válido"))?;
                            put(&mut out, row * tw + col, c);
                        }
                    }
                }
                128 | 130..=255 => {
                    let palette = if sub == 128 {
                        vec![]
                    } else {
                        take((sub as usize - 128) * 3)?.to_vec()
                    };
                    let mut i = 0;
                    while i < total {
                        let (color, run) = if sub == 128 {
                            let c = take(3)?.to_vec();
                            (c, None)
                        } else {
                            let idx = take(1)?[0];
                            let n = (idx & 127) as usize;
                            let c = palette
                                .get(n * 3..n * 3 + 3)
                                .ok_or_else(|| anyhow!("índice de paleta ZRLE no válido"))?
                                .to_vec();
                            (c, (idx & 128 == 0).then_some(1usize))
                        };
                        let run = match run {
                            Some(r) => r,
                            None => {
                                let mut len = 1usize;
                                loop {
                                    let b = take(1)?[0];
                                    len += b as usize;
                                    if b != 255 {
                                        break len;
                                    }
                                }
                            }
                        };
                        if i + run > total {
                            bail!("tramo ZRLE más largo que la tesela");
                        }
                        for k in i..i + run {
                            put(&mut out, k, &color);
                        }
                        i += run;
                    }
                }
                _ => bail!("subcodificación ZRLE no válida: {sub}"),
            }
        }
    }
    Ok(out)
}

/// Descomprime un bloque del flujo zlib continuo de ZRLE.
fn inflate(zlib: &mut flate2::Decompress, input: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(limit.min(1 << 20));
    let mut consumed = 0;
    while consumed < input.len() {
        if out.len() == out.capacity() {
            if out.len() >= limit {
                bail!("datos ZRLE demasiado grandes");
            }
            out.reserve((limit - out.len()).min(1 << 20));
        }
        let before_in = zlib.total_in();
        let before_out = zlib.total_out();
        zlib.decompress_vec(&input[consumed..], &mut out, flate2::FlushDecompress::Sync)
            .map_err(|e| anyhow!("flujo ZRLE dañado: {e}"))?;
        consumed += (zlib.total_in() - before_in) as usize;
        if zlib.total_in() == before_in && zlib.total_out() == before_out {
            break;
        }
    }
    Ok(out)
}

fn latin1(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
        .collect()
}

/// Abre una sesión VNC en segundo plano.
pub fn spawn(
    rt: &tokio::runtime::Handle,
    session: Session,
    password: Option<String>,
    prompter: Prompter,
) -> DesktopBackend {
    spawn_via(rt, session, password, None, prompter)
}

/// Abre una sesión VNC, opcionalmente a través de saltos SSH (`via` es el
/// último salto de la cadena).
pub fn spawn_via(
    rt: &tokio::runtime::Handle,
    session: Session,
    password: Option<String>,
    via: Option<Endpoint>,
    prompter: Prompter,
) -> DesktopBackend {
    spawn_full(
        rt,
        session,
        password,
        via,
        prompter,
        DEFAULT_ENCODINGS.to_vec(),
    )
}

/// Como [`spawn`], eligiendo las codificaciones que se piden al servidor.
pub fn spawn_with_encodings(
    rt: &tokio::runtime::Handle,
    session: Session,
    password: Option<String>,
    prompter: Prompter,
    encodings: Vec<i32>,
) -> DesktopBackend {
    spawn_full(rt, session, password, None, prompter, encodings)
}

fn spawn_full(
    rt: &tokio::runtime::Handle,
    session: Session,
    password: Option<String>,
    via: Option<Endpoint>,
    prompter: Prompter,
    encodings: Vec<i32>,
) -> DesktopBackend {
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    let frame = Framebuffer::shared(1, 1);
    let emit: Emit = {
        let prompter = prompter.clone();
        Arc::new(move |ev| {
            let _ = ev_tx.send(ev);
            prompter.wake();
        })
    };
    let task_frame = frame.clone();
    rt.spawn(async move {
        let result = connect_and_run(
            &session,
            password,
            via,
            &prompter,
            &encodings,
            input_rx,
            task_frame,
            emit.clone(),
        )
        .await;
        emit(DesktopEvent::Closed(result.err().map(|e| format!("{e:#}"))));
    });
    DesktopBackend {
        input: input_tx,
        events: ev_rx,
        frame,
    }
}

#[allow(clippy::too_many_arguments)]
async fn connect_and_run(
    session: &Session,
    password: Option<String>,
    via: Option<Endpoint>,
    prompter: &Prompter,
    encodings: &[i32],
    input_rx: mpsc::UnboundedReceiver<DesktopInput>,
    frame: SharedFrame,
    emit: Emit,
) -> Result<()> {
    let host = session.host.trim();
    // La cadena de saltos (si la hay) vive mientras dure la sesión.
    let status = {
        let emit = emit.clone();
        move |s: String| emit(DesktopEvent::Status(s))
    };
    let (route, to_save) = Route::establish(via.as_ref(), prompter, &status).await?;
    if !to_save.is_empty() {
        emit(DesktopEvent::SaveSecrets(to_save));
    }
    emit(DesktopEvent::Status(format!(
        "Conectando a {host}:{} por VNC (sin cifrar{})…",
        session.port,
        if route.is_direct() {
            ""
        } else {
            " tras el último salto"
        }
    )));
    let stream = route.connect(host, session.port).await?;
    let (rd, wr) = tokio::io::split(stream);
    run(
        BufReader::with_capacity(64 * 1024, rd),
        wr,
        session,
        password,
        prompter,
        encodings,
        input_rx,
        frame,
        emit,
    )
    .await
}

async fn read_reason<R: AsyncRead + Unpin>(rd: &mut R) -> String {
    let Ok(len) = rd.read_u32().await else {
        return String::new();
    };
    let mut buf = vec![0u8; (len as usize).min(4096)];
    if rd.read_exact(&mut buf).await.is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buf).to_string()
}

/// Negociación RFB y bucle de la sesión sobre un flujo ya conectado.
#[allow(clippy::too_many_arguments)]
pub async fn run<R, W>(
    mut rd: R,
    mut wr: W,
    session: &Session,
    password: Option<String>,
    prompter: &Prompter,
    encodings: &[i32],
    mut input_rx: mpsc::UnboundedReceiver<DesktopInput>,
    frame: SharedFrame,
    emit: Emit,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // --- Versión -------------------------------------------------------------
    let mut banner = [0u8; 12];
    tokio::time::timeout(Duration::from_secs(15), rd.read_exact(&mut banner))
        .await
        .map_err(|_| anyhow!("El servidor no respondió como un servidor VNC"))??;
    if &banner[..4] != b"RFB " || banner[11] != b'\n' {
        bail!("El servidor no habla el protocolo VNC (RFB)");
    }
    let minor: u32 = std::str::from_utf8(&banner[8..11])
        .ok()
        .and_then(|s| s.parse().ok())
        .context("Versión RFB no válida")?;
    let major: u32 = std::str::from_utf8(&banner[4..7])
        .ok()
        .and_then(|s| s.parse().ok())
        .context("Versión RFB no válida")?;
    let version = match (major, minor) {
        (3, 0..=6) => 3,
        (3, 7) => 7,
        _ => 8,
    };
    wr.write_all(format!("RFB 003.00{version}\n").as_bytes())
        .await?;

    // --- Seguridad -----------------------------------------------------------
    let security = if version == 3 {
        match rd.read_u32().await? {
            0 => bail!(
                "El servidor rechazó la conexión: {}",
                read_reason(&mut rd).await
            ),
            t => t as u8,
        }
    } else {
        let count = rd.read_u8().await?;
        if count == 0 {
            bail!(
                "El servidor rechazó la conexión: {}",
                read_reason(&mut rd).await
            );
        }
        let mut types = vec![0u8; count as usize];
        rd.read_exact(&mut types).await?;
        let chosen = if types.contains(&1) {
            1
        } else if types.contains(&2) {
            2
        } else {
            bail!("El servidor exige un tipo de seguridad no soportado ({types:?})");
        };
        wr.write_all(&[chosen]).await?;
        chosen
    };
    let mut to_save = None;
    match security {
        1 => {}
        2 => {
            let mut challenge = [0u8; 16];
            rd.read_exact(&mut challenge).await?;
            let (password, save) = match password {
                Some(p) => (p, false),
                None => {
                    let msg = format!("Contraseña VNC de {}:{}", session.host.trim(), session.port);
                    match prompter.secret("VNC", &msg, false, true).await {
                        Some(a) => (a.value, a.save),
                        None => bail!("Autenticación cancelada"),
                    }
                }
            };
            wr.write_all(&vnc_auth_response(&password, &challenge))
                .await?;
            if save {
                to_save = Some((session.password_key(), password));
            }
        }
        other => bail!("Tipo de seguridad VNC no soportado: {other}"),
    }
    // Con «None» el resultado solo se envía a partir de RFB 3.8.
    if security == 2 || version == 8 {
        match rd.read_u32().await? {
            0 => {}
            _ => {
                let reason = if version == 8 {
                    read_reason(&mut rd).await
                } else {
                    String::new()
                };
                bail!("Autenticación VNC fallida: contraseña incorrecta {reason}");
            }
        }
    }

    // --- Inicialización ------------------------------------------------------
    wr.write_all(&[1]).await?; // sesión compartida
    let width = rd.read_u16().await? as u32;
    let height = rd.read_u16().await? as u32;
    let mut pixel_format = [0u8; 16];
    rd.read_exact(&mut pixel_format).await?;
    let name_len = rd.read_u32().await? as usize;
    if name_len > MAX_TEXT {
        bail!("Nombre de escritorio demasiado largo");
    }
    let mut name = vec![0u8; name_len];
    rd.read_exact(&mut name).await?;
    if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
        bail!("Tamaño de escritorio no válido: {width}x{height}");
    }
    frame.lock().unwrap().resize(width, height);

    // Formato de píxel: 32 bpp, little-endian, R en el byte bajo => RGBX.
    let mut msg = vec![0u8, 0, 0, 0];
    msg.extend_from_slice(&[32, 24, 0, 1]);
    msg.extend_from_slice(&255u16.to_be_bytes());
    msg.extend_from_slice(&255u16.to_be_bytes());
    msg.extend_from_slice(&255u16.to_be_bytes());
    msg.extend_from_slice(&[0, 8, 16, 0, 0, 0]);
    wr.write_all(&msg).await?;
    // Codificaciones, por orden de preferencia (más el aviso de cambio de tamaño).
    let mut msg = vec![2u8, 0];
    msg.extend_from_slice(&(encodings.len() as u16 + 1).to_be_bytes());
    for e in encodings.iter().chain([&ENC_DESKTOP_SIZE]) {
        msg.extend_from_slice(&e.to_be_bytes());
    }
    wr.write_all(&msg).await?;
    let request = |incremental: bool, w: u32, h: u32| -> Vec<u8> {
        let mut m = vec![3u8, incremental as u8, 0, 0, 0, 0];
        m.extend_from_slice(&(w as u16).to_be_bytes());
        m.extend_from_slice(&(h as u16).to_be_bytes());
        m
    };
    wr.write_all(&request(false, width, height)).await?;
    wr.flush().await?;

    if let Some(secret) = to_save {
        emit(DesktopEvent::SaveSecrets(vec![secret]));
    }
    emit(DesktopEvent::Connected);

    // --- Sesión --------------------------------------------------------------
    let mut size = (width, height);
    let mut zlib = flate2::Decompress::new(true);
    let mut buttons = 0u8;
    let mut pointer = (0u16, 0u16);
    let pointer_msg = |mask: u8, x: u16, y: u16| -> [u8; 6] {
        let (x, y) = (x.to_be_bytes(), y.to_be_bytes());
        [5, mask, x[0], x[1], y[0], y[1]]
    };
    let key_msg = |down: bool, keysym: u32| -> [u8; 8] {
        let k = keysym.to_be_bytes();
        [4, down as u8, 0, 0, k[0], k[1], k[2], k[3]]
    };
    let mut kind = [0u8; 1];
    loop {
        tokio::select! {
            input = input_rx.recv() => match input {
                None | Some(DesktopInput::Close) => break,
                Some(DesktopInput::Key { key, pressed }) => {
                    wr.write_all(&key_msg(pressed, key.keysym())).await?;
                }
                Some(DesktopInput::Text(text)) => {
                    for c in text.chars() {
                        let keysym = match c {
                            '\n' | '\r' => 0xFF0D,
                            '\t' => 0xFF09,
                            c => char_keysym(c),
                        };
                        wr.write_all(&key_msg(true, keysym)).await?;
                        wr.write_all(&key_msg(false, keysym)).await?;
                    }
                }
                Some(DesktopInput::PointerMove { x, y }) => {
                    pointer = (x, y);
                    wr.write_all(&pointer_msg(buttons, x, y)).await?;
                }
                Some(DesktopInput::PointerButton { button, pressed, x, y }) => {
                    let bit = match button {
                        MouseButton::Left => 1,
                        MouseButton::Middle => 2,
                        MouseButton::Right => 4,
                    };
                    buttons = if pressed { buttons | bit } else { buttons & !bit };
                    pointer = (x, y);
                    wr.write_all(&pointer_msg(buttons, x, y)).await?;
                }
                Some(DesktopInput::Wheel { x, y, delta, horizontal }) => {
                    pointer = (x, y);
                    let bit = match (horizontal, delta > 0) {
                        (false, true) => 8,
                        (false, false) => 16,
                        (true, false) => 32,
                        (true, true) => 64,
                    };
                    for _ in 0..delta.unsigned_abs().min(20) {
                        wr.write_all(&pointer_msg(buttons | bit, x, y)).await?;
                        wr.write_all(&pointer_msg(buttons, x, y)).await?;
                    }
                }
                Some(DesktopInput::ClipboardText(text)) => {
                    let data = latin1(&text);
                    if data.len() <= MAX_TEXT {
                        let mut m = vec![6u8, 0, 0, 0];
                        m.extend_from_slice(&(data.len() as u32).to_be_bytes());
                        m.extend_from_slice(&data);
                        wr.write_all(&m).await?;
                    }
                }
            },
            n = rd.read(&mut kind) => {
                if n? == 0 {
                    break;
                }
                match kind[0] {
                    0 => {
                        rd.read_u8().await?;
                        let rects = rd.read_u16().await?;
                        for _ in 0..rects {
                            let x = rd.read_u16().await? as u32;
                            let y = rd.read_u16().await? as u32;
                            let w = rd.read_u16().await? as u32;
                            let h = rd.read_u16().await? as u32;
                            let encoding = rd.read_i32().await?;
                            if encoding == ENC_DESKTOP_SIZE {
                                if w == 0 || h == 0 || w > MAX_DIMENSION || h > MAX_DIMENSION {
                                    bail!("Tamaño de escritorio no válido: {w}x{h}");
                                }
                                size = (w, h);
                                frame.lock().unwrap().resize(w, h);
                                continue;
                            }
                            if x + w > size.0 || y + h > size.1 {
                                bail!("El servidor envió un rectángulo fuera de la pantalla");
                            }
                            let (uw, uh) = (w as usize, h as usize);
                            let counter = match encoding {
                                ENC_RAW => Some(0),
                                ENC_COPYRECT => Some(1),
                                ENC_HEXTILE => Some(2),
                                ENC_ZRLE => Some(3),
                                _ => None,
                            };
                            if let Some(i) = counter {
                                RECTS_BY_ENCODING[i]
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                            match encoding {
                                ENC_RAW => {
                                    let mut data = vec![0u8; uw * uh * 4];
                                    rd.read_exact(&mut data).await?;
                                    for px in data.chunks_exact_mut(4) {
                                        px[3] = 255;
                                    }
                                    frame.lock().unwrap().blit_rgba(x, y, w, h, &data);
                                }
                                ENC_COPYRECT => {
                                    let sx = rd.read_u16().await? as u32;
                                    let sy = rd.read_u16().await? as u32;
                                    if sx + w > size.0 || sy + h > size.1 {
                                        bail!("CopyRect fuera de la pantalla");
                                    }
                                    frame.lock().unwrap().copy_rect(sx, sy, w, h, x, y);
                                }
                                ENC_HEXTILE => {
                                    let data = decode_hextile(&mut rd, uw, uh).await?;
                                    frame.lock().unwrap().blit_rgba(x, y, w, h, &data);
                                }
                                ENC_ZRLE => {
                                    let len = rd.read_u32().await? as usize;
                                    if len > MAX_ZRLE_INPUT {
                                        bail!("Bloque ZRLE demasiado grande");
                                    }
                                    let mut packed = vec![0u8; len];
                                    rd.read_exact(&mut packed).await?;
                                    // Como mucho: 3 bytes por píxel + 1 por tesela.
                                    let limit = uw * uh * 3 + uw.div_ceil(64) * uh.div_ceil(64) + 64;
                                    let raw = inflate(&mut zlib, &packed, limit)?;
                                    let data = decode_zrle(&raw, uw, uh)?;
                                    frame.lock().unwrap().blit_rgba(x, y, w, h, &data);
                                }
                                other => bail!("Codificación VNC no soportada: {other}"),
                            }
                        }
                        prompter.wake();
                        wr.write_all(&request(true, size.0, size.1)).await?;
                    }
                    1 => {
                        // Paleta de colores: no se usa con color verdadero.
                        rd.read_u8().await?;
                        rd.read_u16().await?;
                        let count = rd.read_u16().await? as usize;
                        let mut skip = vec![0u8; count * 6];
                        rd.read_exact(&mut skip).await?;
                    }
                    2 => {}
                    3 => {
                        let mut pad = [0u8; 3];
                        rd.read_exact(&mut pad).await?;
                        let len = rd.read_u32().await? as usize;
                        if len > MAX_TEXT {
                            bail!("Texto de portapapeles demasiado grande");
                        }
                        let mut text = vec![0u8; len];
                        rd.read_exact(&mut text).await?;
                        let text: String = text.iter().map(|&b| b as char).collect();
                        emit(DesktopEvent::ClipboardText(text));
                    }
                    other => bail!("Mensaje VNC desconocido: {other}"),
                }
            }
        }
        wr.flush().await?;
    }
    let _ = pointer;
    let _ = wr.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_response_properties() {
        // (La respuesta correcta se comprueba contra un servidor real en
        // tests/vnc_integration.rs; aquí solo las propiedades básicas.)
        let challenge: [u8; 16] = std::array::from_fn(|i| i as u8);
        let response = vnc_auth_response("password", &challenge);
        assert_ne!(response, challenge);
        // Solo cuentan los 8 primeros caracteres.
        assert_eq!(response, vnc_auth_response("passwordEXTRA", &challenge));
        assert_ne!(response, vnc_auth_response("passwore", &challenge));
    }

    #[test]
    fn zrle_tiles() {
        // 2x2 sólido rojo.
        assert_eq!(
            decode_zrle(&[1, 255, 0, 0], 2, 2).unwrap(),
            [255, 0, 0, 255].repeat(4)
        );
        // 2x1 en bruto.
        assert_eq!(
            decode_zrle(&[0, 1, 2, 3, 4, 5, 6], 2, 1).unwrap(),
            [1, 2, 3, 255, 4, 5, 6, 255]
        );
        // Paleta de 2 colores, 3x2: filas 101 y 010 (1 bit por píxel).
        let data = [2, 9, 9, 9, 7, 7, 7, 0b1010_0000, 0b0100_0000];
        let out = decode_zrle(&data, 3, 2).unwrap();
        let firsts: Vec<u8> = out.chunks(4).map(|p| p[0]).collect();
        assert_eq!(firsts, [7, 9, 7, 9, 7, 9]);
        // RLE simple: 4 píxeles iguales (longitud 1 + 3).
        assert_eq!(
            decode_zrle(&[128, 5, 6, 7, 3], 2, 2).unwrap(),
            [5, 6, 7, 255].repeat(4)
        );
        // RLE con paleta: color 1 repetido 3 veces y luego un píxel del color 0.
        let data = [130, 1, 1, 1, 2, 2, 2, 129, 2, 0];
        let firsts: Vec<u8> = decode_zrle(&data, 2, 2)
            .unwrap()
            .chunks(4)
            .map(|p| p[0])
            .collect();
        assert_eq!(firsts, [2, 2, 2, 1]);
        // Datos corruptos: errores, nunca pánico.
        assert!(decode_zrle(&[1, 255], 2, 2).is_err());
        assert!(decode_zrle(&[128, 5, 6, 7, 200], 2, 2).is_err());
        assert!(decode_zrle(&[50], 2, 2).is_err());
        assert!(decode_zrle(&[130, 1, 1, 1, 2, 2, 2, 5], 2, 2).is_err());
        assert!(decode_zrle(&[], 2, 2).is_err());
    }

    #[tokio::test]
    async fn hextile_tiles() {
        // 2x2: fondo azul + un subrectángulo 1x1 verde en (1,1).
        let data = [2 | 4 | 8, 0, 0, 255, 0, 0, 255, 0, 0, 1, 0x11, 0x00];
        let out = decode_hextile(&mut &data[..], 2, 2).await.unwrap();
        assert_eq!(&out[0..4], &[0, 0, 255, 255]);
        assert_eq!(&out[12..16], &[0, 255, 0, 255]);
        // Tesela en bruto.
        let data = [1, 1, 2, 3, 0];
        assert_eq!(
            decode_hextile(&mut &data[..], 1, 1).await.unwrap(),
            [1, 2, 3, 255]
        );
        // Subrectángulo que se sale de la tesela y datos truncados.
        let data = [8, 1, 0x00, 0xFF];
        assert!(decode_hextile(&mut &data[..], 2, 2).await.is_err());
        assert!(decode_hextile(&mut &[2u8, 0][..], 2, 2).await.is_err());
    }

    #[test]
    fn clipboard_is_latin1() {
        assert_eq!(latin1("añ€"), [b'a', 0xF1, b'?']);
    }
}
