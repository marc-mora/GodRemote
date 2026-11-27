//! Extensión RANDR (versión 1.3), lo justo: una pantalla con una salida,
//! un controlador («CRTC») y un único modo, que es el tamaño que tenga en
//! cada momento la pestaña. Sirve para que GTK y Qt pregunten por el
//! monitor y, sobre todo, para avisarles cuando la pestaña cambia de
//! tamaño. Nada se puede configurar desde los clientes.

use super::proto::*;
use super::server::{ClientId, ROOT, Server};

/// Código de operación de RANDR (el mismo que usa X.Org).
pub const RANDR_OPCODE: u8 = 140;
pub const RANDR_FIRST_EVENT: u8 = RR_SCREEN_CHANGE_NOTIFY;
pub const RANDR_FIRST_ERROR: u8 = 147;
const BAD_OUTPUT: u8 = RANDR_FIRST_ERROR;
const BAD_CRTC: u8 = RANDR_FIRST_ERROR + 1;

// Identificadores del servidor (por debajo de los de cualquier cliente).
const CRTC: Xid = 0x110;
const OUTPUT: Xid = 0x111;
const MODE: Xid = 0x112;
const OUTPUT_NAME: &[u8] = b"GodRemote-0";

/// Avisos que puede pedir un cliente (`SelectInput`).
const SELECT_SCREEN: u16 = 1;
const SELECT_CRTC: u16 = 2;
const SELECT_OUTPUT: u16 = 4;
/// Clientes apuntados a los avisos, entre todas las ventanas.
const MAX_LISTENERS: usize = 1024;

/// Un cliente que quiere avisos de RANDR referidos a una ventana.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Listener {
    pub client: ClientId,
    pub window: Xid,
    pub mask: u16,
}

fn millimetres(px: u32) -> u32 {
    (px as f32 * 25.4 / 96.0).round() as u32
}

impl Server {
    /// Nombre del modo: «1280x720».
    fn randr_mode_name(&self) -> Vec<u8> {
        format!("{}x{}", self.width, self.height).into_bytes()
    }

    /// La pantalla ha cambiado de tamaño: se avisa a quien lo pidió.
    pub(super) fn randr_screen_changed(&mut self) {
        self.randr_time = self.now();
        let (time, w, h) = (self.randr_time, self.width, self.height);
        for l in self.randr_listeners.clone() {
            if l.mask & SELECT_SCREEN != 0 {
                let ev = Ev::new(RR_SCREEN_CHANGE_NOTIFY, 1)
                    .u32(time)
                    .u32(time)
                    .u32(ROOT)
                    .u32(l.window)
                    .u16(0)
                    .u16(0)
                    .u16(w as u16)
                    .u16(h as u16)
                    .u16(millimetres(w) as u16)
                    .u16(millimetres(h) as u16)
                    .done();
                self.send(l.client, ev);
            }
            if l.mask & SELECT_CRTC != 0 {
                // RRNotify, subcódigo 0: cambio de CRTC.
                let ev = Ev::new(RR_NOTIFY, 0)
                    .u32(time)
                    .u32(l.window)
                    .u32(CRTC)
                    .u32(MODE)
                    .u16(1)
                    .u16(0)
                    .i16(0)
                    .i16(0)
                    .u16(w as u16)
                    .u16(h as u16)
                    .done();
                self.send(l.client, ev);
            }
            if l.mask & SELECT_OUTPUT != 0 {
                // RRNotify, subcódigo 1: cambio de salida.
                let ev = Ev::new(RR_NOTIFY, 1)
                    .u32(time)
                    .u32(time)
                    .u32(l.window)
                    .u32(OUTPUT)
                    .u32(CRTC)
                    .u32(MODE)
                    .u16(1)
                    .u8(0)
                    .u8(0)
                    .done();
                self.send(l.client, ev);
            }
        }
    }

    pub(super) fn randr_remove_client(&mut self, client: ClientId) {
        self.randr_listeners.retain(|l| l.client != client);
    }

    pub(super) fn randr_window_destroyed(&mut self, window: Xid) {
        if !self.randr_listeners.is_empty() {
            self.randr_listeners.retain(|l| l.window != window);
        }
    }

    /// Recursos de la pantalla (`GetScreenResources` y su variante
    /// `Current`): un CRTC, una salida y un modo.
    fn randr_resources(&mut self, c: ClientId) {
        let name = self.randr_mode_name();
        let (width, height) = (self.width, self.height);
        let mut w = self.reply(c, 0);
        w.u32(self.randr_time).u32(self.randr_time);
        w.u16(1).u16(1).u16(1).u16(name.len() as u16).zeros(8);
        w.u32(CRTC).u32(OUTPUT);
        // El modo, a 60 Hz: reloj de puntos = total horizontal × vertical × 60.
        let dot_clock = width.saturating_mul(height).saturating_mul(60);
        w.u32(MODE)
            .u16(width as u16)
            .u16(height as u16)
            .u32(dot_clock);
        w.u16(width as u16)
            .u16(width as u16)
            .u16(width as u16)
            .u16(0);
        w.u16(height as u16).u16(height as u16).u16(height as u16);
        w.u16(name.len() as u16).u32(0);
        w.bytes(&name).pad();
        self.send_reply(c, w);
    }

    pub(super) fn dispatch_randr(&mut self, c: ClientId, minor: u8, rd: &mut Reader) -> XResult {
        let (width, height) = (self.width, self.height);
        let time = self.randr_time;
        match minor {
            // QueryVersion
            0 => {
                let mut w = self.reply(c, 0);
                w.u32(1).u32(3);
                self.send_reply(c, w);
                Ok(())
            }
            // SelectInput
            4 => {
                let (window, mask) = (rd.u32()?, rd.u16()?);
                self.win(window)?;
                self.randr_listeners
                    .retain(|l| (l.client, l.window) != (c, window));
                if mask != 0 {
                    if self.randr_listeners.len() >= MAX_LISTENERS {
                        return Err(XError::new(BAD_ALLOC, window));
                    }
                    self.randr_listeners.push(Listener {
                        client: c,
                        window,
                        mask,
                    });
                }
                Ok(())
            }
            // GetScreenInfo: un tamaño, una frecuencia, sin giros.
            5 => {
                self.win(rd.u32()?)?;
                let mut w = self.reply(c, 1);
                w.u32(ROOT).u32(time).u32(time);
                w.u16(1).u16(0).u16(1).u16(60).u16(2).u16(0);
                w.u16(width as u16).u16(height as u16);
                w.u16(millimetres(width) as u16)
                    .u16(millimetres(height) as u16);
                w.u16(1).u16(60);
                self.send_reply(c, w);
                Ok(())
            }
            // GetScreenSizeRange: el tamaño lo decide la pestaña.
            6 => {
                self.win(rd.u32()?)?;
                let mut w = self.reply(c, 0);
                w.u16(width as u16).u16(height as u16);
                w.u16(width as u16).u16(height as u16);
                self.send_reply(c, w);
                Ok(())
            }
            // GetScreenResources y GetScreenResourcesCurrent
            8 | 25 => {
                self.win(rd.u32()?)?;
                self.randr_resources(c);
                Ok(())
            }
            // GetOutputInfo
            9 => {
                let output = rd.u32()?;
                if output != OUTPUT {
                    return Err(XError::new(BAD_OUTPUT, output));
                }
                let mut w = self.reply(c, 0);
                w.u32(time).u32(CRTC);
                w.u32(millimetres(width)).u32(millimetres(height));
                // Conectada; orden de subpíxeles desconocido.
                w.u8(0).u8(0);
                w.u16(1).u16(1).u16(1).u16(0).u16(OUTPUT_NAME.len() as u16);
                w.u32(CRTC).u32(MODE);
                w.bytes(OUTPUT_NAME).pad();
                self.send_reply(c, w);
                Ok(())
            }
            // ListOutputProperties: ninguna.
            10 => {
                let output = rd.u32()?;
                if output != OUTPUT {
                    return Err(XError::new(BAD_OUTPUT, output));
                }
                let mut w = self.reply(c, 0);
                w.u16(0);
                self.send_reply(c, w);
                Ok(())
            }
            // GetOutputProperty: no existe ninguna (tipo «None»).
            15 => {
                let output = rd.u32()?;
                if output != OUTPUT {
                    return Err(XError::new(BAD_OUTPUT, output));
                }
                let mut w = self.reply(c, 0);
                w.u32(0).u32(0).u32(0);
                self.send_reply(c, w);
                Ok(())
            }
            // GetCrtcInfo
            20 => {
                let crtc = rd.u32()?;
                if crtc != CRTC {
                    return Err(XError::new(BAD_CRTC, crtc));
                }
                let mut w = self.reply(c, 0);
                w.u32(time)
                    .i16(0)
                    .i16(0)
                    .u16(width as u16)
                    .u16(height as u16);
                w.u32(MODE).u16(1).u16(1).u16(1).u16(1);
                w.u32(OUTPUT).u32(OUTPUT);
                self.send_reply(c, w);
                Ok(())
            }
            // GetCrtcGammaSize y GetCrtcGamma: sin tabla de gamma.
            22 | 23 => {
                let crtc = rd.u32()?;
                if crtc != CRTC {
                    return Err(XError::new(BAD_CRTC, crtc));
                }
                let mut w = self.reply(c, 0);
                w.u16(0);
                self.send_reply(c, w);
                Ok(())
            }
            // GetPanning: sin desplazamiento.
            28 => {
                let crtc = rd.u32()?;
                if crtc != CRTC {
                    return Err(XError::new(BAD_CRTC, crtc));
                }
                let mut w = self.reply(c, 0);
                w.u32(time).zeros(24);
                self.send_reply(c, w);
                Ok(())
            }
            // GetOutputPrimary
            31 => {
                self.win(rd.u32()?)?;
                let mut w = self.reply(c, 0);
                w.u32(OUTPUT);
                self.send_reply(c, w);
                Ok(())
            }
            // SetScreenConfig, SetScreenSize, SetCrtcConfig, propiedades y
            // modos nuevos: aquí no se configura nada.
            2 | 7 | 11..=14 | 16..=19 | 21 | 24 | 26 | 27 | 29 | 30 => {
                Err(XError::new(BAD_ACCESS, 0))
            }
            _ => Err(XError::new(BAD_REQUEST, 0)),
        }
    }
}
