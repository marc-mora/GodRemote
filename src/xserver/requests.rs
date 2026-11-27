//! Peticiones del núcleo de X: ventanas, propiedades, selecciones, eventos,
//! capturas, teclado y varios. Las de dibujo, fuentes y color están en
//! `requests_draw.rs`.

use super::draw::{Image, Rect};
use super::keymap::{KEYSYMS_PER_KEYCODE, MAX_KEYCODE, MIN_KEYCODE};
use super::proto::*;
use super::server::{
    Background, ClientId, DEFAULT_COLORMAP, Focus, KeyboardGrab, PassiveGrab, PointerGrab,
    Property, ROOT, ROOT_VISUAL, Server, swap_property_data,
};
use super::wm::{FRAME, TITLE_H};

/// Código de operación de la extensión BIG-REQUESTS (el mismo que usa X.Org).
pub const BIG_REQUESTS_OPCODE: u8 = 133;
/// Tamaño máximo de petición con BIG-REQUESTS, en unidades de 4 bytes.
pub const MAX_BIG_REQUEST: u32 = 4 * 1024 * 1024;

/// Con la variable de entorno `GODREMOTE_X11_TRACE` se anota en la salida de
/// errores cada petición y cada error X: sirve para depurar un cliente que
/// se cierra sin dar explicaciones.
fn tracing() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("GODREMOTE_X11_TRACE").is_some())
}

impl Server {
    /// Procesa una petición de un cliente. `data` es el segundo byte de la
    /// cabecera y `body` lo que sigue a la cabecera.
    pub fn request(&mut self, client: ClientId, opcode: u8, data: u8, body: &[u8]) {
        let Some(c) = self.clients.get_mut(&client) else {
            return;
        };
        c.seq = c.seq.wrapping_add(1);
        let (be, seq) = (c.be, c.seq);
        let trace = tracing();
        if trace {
            eprintln!(
                "[x11] cliente {client} #{seq}: petición {opcode} ({data}), {} bytes",
                body.len()
            );
        }
        let mut rd = Reader::new(body, be);
        if let Err(err) = self.dispatch(client, opcode, data, &mut rd)
            && let Some(c) = self.clients.get_mut(&client)
        {
            if trace {
                eprintln!(
                    "[x11] cliente {client} #{seq}: error {} (valor {:#x}) en la petición {opcode}",
                    err.code, err.value
                );
            }
            let minor = if opcode >= 128 { data as u16 } else { 0 };
            let packet = encode_error(c.be, c.seq, err, opcode, minor);
            c.out.extend_from_slice(&packet);
        }
    }

    /// Empieza la respuesta a la petición en curso de un cliente.
    pub(super) fn reply(&self, client: ClientId, data: u8) -> Writer {
        let c = &self.clients[&client];
        Writer::reply(c.be, c.seq, data)
    }

    pub(super) fn send_reply(&mut self, client: ClientId, w: Writer) {
        let bytes = w.finish_reply();
        if let Some(c) = self.clients.get_mut(&client) {
            c.out.extend_from_slice(&bytes);
        }
    }

    fn dispatch(&mut self, c: ClientId, opcode: u8, data: u8, rd: &mut Reader) -> XResult {
        match opcode {
            1 => self.create_window(c, data, rd),
            2 => {
                let window = rd.u32()?;
                self.win(window)?;
                let mask = rd.u32()?;
                self.apply_window_attributes(c, window, mask, rd)
            }
            3 => self.get_window_attributes(c, rd),
            4 => {
                let window = rd.u32()?;
                self.win(window)?;
                self.destroy_window(window);
                Ok(())
            }
            5 => {
                let window = rd.u32()?;
                let children = self.win(window)?.children.clone();
                for child in children {
                    self.destroy_window(child);
                }
                Ok(())
            }
            6 => {
                let window = rd.u32()?;
                self.win(window)?;
                if Self::owner_of(window) == c {
                    return Err(XError::new(BAD_MATCH, window));
                }
                if let Some(client) = self.clients.get_mut(&c) {
                    client.save_set.retain(|w| *w != window);
                    if data == 0 {
                        client.save_set.push(window);
                    }
                }
                Ok(())
            }
            7 => {
                let (window, parent) = (rd.u32()?, rd.u32()?);
                let (x, y) = (rd.i16()? as i32, rd.i16()? as i32);
                self.win(window)?;
                self.reparent_window(window, parent, x, y)
            }
            8 => {
                let window = rd.u32()?;
                self.win(window)?;
                self.map_window(c, window);
                Ok(())
            }
            9 => {
                let window = rd.u32()?;
                let children = self.win(window)?.children.clone();
                for child in children.iter().rev() {
                    self.map_window(c, *child);
                }
                Ok(())
            }
            10 => {
                let window = rd.u32()?;
                self.win(window)?;
                self.unmap_window(window, false);
                Ok(())
            }
            11 => {
                let window = rd.u32()?;
                let children = self.win(window)?.children.clone();
                for child in children {
                    self.unmap_window(child, false);
                }
                Ok(())
            }
            12 => self.configure_window(c, rd),
            13 => self.circulate_window(c, data, rd),
            14 => self.get_geometry(c, rd),
            15 => self.query_tree(c, rd),
            16 => self.intern_atom(c, data, rd),
            17 => {
                let atom = rd.u32()?;
                let name = self
                    .atoms
                    .name(atom)
                    .ok_or(XError::new(BAD_ATOM, atom))?
                    .as_bytes()
                    .to_vec();
                let mut w = self.reply(c, 0);
                w.u16(name.len() as u16).zeros(22).bytes(&name);
                self.send_reply(c, w);
                Ok(())
            }
            18 => self.change_property(c, data, rd),
            19 => {
                let (window, name) = (rd.u32()?, rd.u32()?);
                self.win(window)?;
                self.check_atom(name)?;
                self.delete_property(window, name);
                Ok(())
            }
            20 => self.get_property(c, data, rd),
            21 => {
                let window = rd.u32()?;
                let names: Vec<Xid> = self.win(window)?.props.iter().map(|p| p.name).collect();
                let mut w = self.reply(c, 0);
                w.u16(names.len() as u16).zeros(22);
                for name in names {
                    w.u32(name);
                }
                self.send_reply(c, w);
                Ok(())
            }
            22 => {
                let (owner, selection, _time) = (rd.u32()?, rd.u32()?, rd.u32()?);
                self.check_atom(selection)?;
                if owner != 0 {
                    self.win(owner)?;
                }
                self.set_selection_owner(selection, owner, if owner == 0 { 0 } else { c });
                Ok(())
            }
            23 => {
                let selection = rd.u32()?;
                self.check_atom(selection)?;
                let owner = self.selections.get(&selection).map_or(0, |s| s.window);
                let mut w = self.reply(c, 0);
                w.u32(owner);
                self.send_reply(c, w);
                Ok(())
            }
            24 => self.convert_selection(c, rd),
            25 => self.send_event(c, data, rd),
            26 => self.grab_pointer(c, data, rd),
            27 => {
                if self.grab.as_ref().is_some_and(|g| g.client == c) {
                    self.end_grab();
                }
                Ok(())
            }
            28 => self.grab_button(c, data, rd),
            29 => {
                let window = rd.u32()?;
                let modifiers = rd.u16()?;
                self.win_mut(window)?.grabs.retain(|g| {
                    g.key
                        || g.client != c
                        || !(data == 0 || g.detail == data)
                        || !(modifiers == S_ANY_MODIFIER || g.modifiers == modifiers)
                });
                Ok(())
            }
            30 => {
                let (_cursor, _time, mask) = (rd.u32()?, rd.u32()?, rd.u16()?);
                if let Some(g) = &mut self.grab
                    && g.client == c
                {
                    g.mask = mask as u32;
                }
                Ok(())
            }
            31 => {
                let (window, _time) = (rd.u32()?, rd.u32()?);
                self.win(window)?;
                let status = if self.keyboard_grab.as_ref().is_some_and(|g| g.client != c) {
                    1 // AlreadyGrabbed
                } else if !self.viewable(window) {
                    3 // NotViewable
                } else {
                    self.keyboard_grab = Some(KeyboardGrab {
                        client: c,
                        window,
                        owner_events: data != 0,
                        by_key: None,
                    });
                    0
                };
                let w = self.reply(c, status);
                self.send_reply(c, w);
                Ok(())
            }
            32 => {
                if self.keyboard_grab.as_ref().is_some_and(|g| g.client == c) {
                    self.keyboard_grab = None;
                }
                Ok(())
            }
            33 => {
                let window = rd.u32()?;
                let (modifiers, key) = (rd.u16()?, rd.u8()?);
                let grab = PassiveGrab {
                    client: c,
                    key: true,
                    detail: key,
                    modifiers,
                    owner_events: data != 0,
                    mask: 0,
                    sync: false,
                };
                let w = self.win_mut(window)?;
                w.grabs
                    .retain(|g| !(g.key && g.detail == key && g.modifiers == modifiers));
                w.grabs.push(grab);
                Ok(())
            }
            34 => {
                let window = rd.u32()?;
                let modifiers = rd.u16()?;
                self.win_mut(window)?.grabs.retain(|g| {
                    !g.key
                        || g.client != c
                        || !(data == 0 || g.detail == data)
                        || !(modifiers == S_ANY_MODIFIER || g.modifiers == modifiers)
                });
                Ok(())
            }
            35 => {
                // AllowEvents: no se congela nada, así que solo importa
                // «repetir» la pulsación de una captura pasiva síncrona.
                if data == 2 {
                    self.replay_pointer(c);
                } else if let Some(g) = &mut self.grab
                    && g.client == c
                {
                    g.replay = None;
                }
                Ok(())
            }
            36 | 37 => Ok(()), // GrabServer / UngrabServer
            38 => self.query_pointer(c, rd),
            39 => {
                let window = rd.u32()?;
                self.win(window)?;
                let mut w = self.reply(c, 0);
                w.u32(0);
                self.send_reply(c, w);
                Ok(())
            }
            40 => self.translate_coordinates(c, rd),
            41 => {
                let (_src, dst) = (rd.u32()?, rd.u32()?);
                rd.skip(8)?;
                let (dx, dy) = (rd.i16()? as i32, rd.i16()? as i32);
                let (x, y) = if dst == 0 {
                    (self.pointer.0 + dx, self.pointer.1 + dy)
                } else {
                    self.win(dst)?;
                    let (ox, oy) = self.abs_origin(dst);
                    (ox + dx, oy + dy)
                };
                self.pointer_motion(x, y);
                Ok(())
            }
            42 => {
                let (focus, _time) = (rd.u32()?, rd.u32()?);
                let new = match focus {
                    0 => Focus::None,
                    1 => Focus::PointerRoot,
                    w => {
                        self.win(w)?;
                        if !self.viewable(w) {
                            return Err(XError::new(BAD_MATCH, w));
                        }
                        Focus::Window(w)
                    }
                };
                self.focus_revert = data;
                self.set_focus(new);
                Ok(())
            }
            43 => {
                let focus = match self.focus {
                    Focus::None => 0,
                    Focus::PointerRoot => 1,
                    Focus::Window(w) => w,
                };
                let mut w = self.reply(c, self.focus_revert);
                w.u32(focus);
                self.send_reply(c, w);
                Ok(())
            }
            44 => {
                let mut w = self.reply(c, 0);
                w.bytes(&self.keymap.down);
                self.send_reply(c, w);
                Ok(())
            }
            45..=97 => self.dispatch_draw(c, opcode, data, rd),
            98 => {
                let len = rd.u16()? as usize;
                rd.skip(2)?;
                let name = rd.bytes(len)?;
                let present = name == b"BIG-REQUESTS";
                let mut w = self.reply(c, 0);
                w.u8(present as u8)
                    .u8(if present { BIG_REQUESTS_OPCODE } else { 0 })
                    .u8(0)
                    .u8(0);
                self.send_reply(c, w);
                Ok(())
            }
            99 => {
                let name = b"BIG-REQUESTS";
                let mut w = self.reply(c, 1);
                w.zeros(24).u8(name.len() as u8).bytes(name);
                self.send_reply(c, w);
                Ok(())
            }
            100 => {
                let (first, per) = (rd.u8()?, rd.u8()? as usize);
                rd.skip(2)?;
                if first < MIN_KEYCODE || per == 0 {
                    return Err(XError::new(BAD_VALUE, first as u32));
                }
                for i in 0..data {
                    let mut syms = [0u32; KEYSYMS_PER_KEYCODE];
                    for k in 0..per {
                        let ks = rd.u32()?;
                        if k < KEYSYMS_PER_KEYCODE {
                            syms[k] = ks;
                        }
                    }
                    let Some(code) = first.checked_add(i) else {
                        return Err(XError::new(BAD_VALUE, first as u32));
                    };
                    self.keymap.set_keysyms(code, syms);
                }
                self.mapping_changed(1, first, data);
                Ok(())
            }
            101 => {
                let (first, count) = (rd.u8()?, rd.u8()?);
                if first < MIN_KEYCODE || first as u32 + count as u32 > MAX_KEYCODE as u32 + 1 {
                    return Err(XError::new(BAD_VALUE, first as u32));
                }
                let mut w = self.reply(c, KEYSYMS_PER_KEYCODE as u8);
                w.zeros(24);
                for code in first as u32..first as u32 + count as u32 {
                    for ks in self.keymap.keysyms(code as u8) {
                        w.u32(ks);
                    }
                }
                self.send_reply(c, w);
                Ok(())
            }
            102 | 104 | 105 | 107 | 109 | 111 | 112 | 115 => Ok(()),
            103 => {
                // GetKeyboardControl: repetición automática activada.
                let mut w = self.reply(c, 1);
                w.u32(0).u8(0).u8(50).u16(400).u16(100).zeros(2);
                w.bytes(&[0xff; 32]);
                self.send_reply(c, w);
                Ok(())
            }
            106 => {
                let mut w = self.reply(c, 0);
                w.u16(2).u16(1).u16(4);
                self.send_reply(c, w);
                Ok(())
            }
            108 => {
                let mut w = self.reply(c, 0);
                w.u16(0).u16(0).u8(0).u8(0);
                self.send_reply(c, w);
                Ok(())
            }
            110 => {
                // ListHosts: control de acceso activado, lista vacía.
                let mut w = self.reply(c, 1);
                w.u16(0).zeros(22);
                self.send_reply(c, w);
                Ok(())
            }
            113 => {
                let resource = rd.u32()?;
                let owner = Self::owner_of(resource);
                if resource != 0 {
                    if owner == 0 || !self.clients.contains_key(&owner) {
                        return Err(XError::new(BAD_VALUE, resource));
                    }
                    self.remove_client(owner);
                }
                Ok(())
            }
            114 => self.rotate_properties(rd),
            116 => {
                // SetPointerMapping: se acepta sin cambiar nada.
                let w = self.reply(c, 0);
                self.send_reply(c, w);
                Ok(())
            }
            117 => {
                let map = [1u8, 2, 3, 4, 5, 6, 7];
                let mut w = self.reply(c, map.len() as u8);
                w.zeros(24).bytes(&map);
                self.send_reply(c, w);
                Ok(())
            }
            118 => {
                // SetModifierMapping: el mapa es fijo; se contesta «Failed».
                let w = self.reply(c, 2);
                self.send_reply(c, w);
                Ok(())
            }
            119 => {
                let mut w = self.reply(c, 1);
                w.zeros(24).bytes(&self.keymap.modifier_map());
                self.send_reply(c, w);
                Ok(())
            }
            127 => Ok(()),
            BIG_REQUESTS_OPCODE if data == 0 => {
                if let Some(client) = self.clients.get_mut(&c) {
                    client.big_requests = true;
                }
                let mut w = self.reply(c, 0);
                w.u32(MAX_BIG_REQUEST);
                self.send_reply(c, w);
                Ok(())
            }
            _ => {
                if !self.unimplemented.contains(&opcode) {
                    self.unimplemented.push(opcode);
                }
                Err(XError::new(BAD_REQUEST, 0))
            }
        }
    }

    fn check_atom(&self, atom: Xid) -> XResult {
        if self.atoms.valid(atom) {
            Ok(())
        } else {
            Err(XError::new(BAD_ATOM, atom))
        }
    }

    // --- Ventanas -----------------------------------------------------------

    fn create_window(&mut self, c: ClientId, depth: u8, rd: &mut Reader) -> XResult {
        let (id, parent) = (rd.u32()?, rd.u32()?);
        let (x, y) = (rd.i16()? as i32, rd.i16()? as i32);
        let (w, h, bw) = (rd.u16()? as u32, rd.u16()? as u32, rd.u16()? as u32);
        let (class, visual, mask) = (rd.u16()?, rd.u32()?, rd.u32()?);
        self.check_new_id(c, id)?;
        let p = self.win(parent)?;
        let input_only = match class {
            0 => p.input_only,
            1 => false,
            2 => true,
            _ => return Err(XError::new(BAD_VALUE, class as u32)),
        };
        if w == 0 || h == 0 {
            return Err(XError::new(BAD_VALUE, 0));
        }
        if input_only && (depth != 0 || bw != 0) || !input_only && p.input_only {
            return Err(XError::new(BAD_MATCH, id));
        }
        let depth = if input_only { 0 } else { depth };
        if !input_only && depth != 0 && depth != 24 || visual != 0 && visual != ROOT_VISUAL {
            return Err(XError::new(BAD_MATCH, id));
        }
        let mut win = self.blank_window(id, c, parent, if input_only { 0 } else { 24 }, input_only);
        (win.x, win.y, win.w, win.h, win.bw) = (x, y, w, h, bw);
        if !input_only {
            win.image = Some(Image::new(w, h, 24));
        }
        self.windows.insert(id, win);
        if let Err(e) = self.apply_window_attributes(c, id, mask, rd) {
            self.windows.remove(&id);
            return Err(e);
        }
        // El fondo de color se pinta ya: así no asoma basura si la ventana
        // se ve antes de que el cliente dibuje.
        if let Some(win) = self.windows.get_mut(&id)
            && let (Background::Pixel(p), Some(img)) = (&win.bg, &mut win.image)
        {
            img.fill(img.bounds(), *p);
        }
        self.win_mut(parent)?.children.push(id);
        let override_redirect = self.win(id)?.override_redirect;
        let ev = Ev::new(CREATE_NOTIFY, 0)
            .u32(parent)
            .u32(id)
            .i16(x as i16)
            .i16(y as i16)
            .u16(w as u16)
            .u16(h as u16)
            .u16(bw as u16)
            .u8(override_redirect as u8)
            .done();
        self.deliver(parent, M_SUBSTRUCTURE_NOTIFY, ev);
        Ok(())
    }

    /// Aplica una lista de atributos de ventana (`CreateWindow` y
    /// `ChangeWindowAttributes`).
    fn apply_window_attributes(
        &mut self,
        c: ClientId,
        id: Xid,
        mask: u32,
        rd: &mut Reader,
    ) -> XResult {
        if mask >> 15 != 0 {
            return Err(XError::new(BAD_VALUE, mask));
        }
        for bit in 0..15 {
            if mask & (1 << bit) == 0 {
                continue;
            }
            let v = rd.u32()?;
            match bit {
                0 => {
                    let bg = match v {
                        0 => Background::None,
                        1 => Background::ParentRelative,
                        pixmap => {
                            let p = self
                                .pixmaps
                                .get(&pixmap)
                                .ok_or(XError::new(BAD_PIXMAP, pixmap))?;
                            if p.image.depth != 24 {
                                return Err(XError::new(BAD_MATCH, pixmap));
                            }
                            Background::Pixmap(p.image.clone())
                        }
                    };
                    self.win_mut(id)?.bg = bg;
                }
                1 => self.win_mut(id)?.bg = Background::Pixel(v & 0x00ff_ffff),
                2 => {
                    // Borde con mosaico: se usa su primer píxel como color.
                    if v != 0 {
                        let p = self.pixmaps.get(&v).ok_or(XError::new(BAD_PIXMAP, v))?;
                        let pixel = p.image.get(0, 0);
                        self.win_mut(id)?.border_pixel = pixel;
                    }
                }
                3 => {
                    self.win_mut(id)?.border_pixel = v & 0x00ff_ffff;
                    self.damage_frame(id);
                }
                4 => self.win_mut(id)?.bit_gravity = v as u8,
                5 => self.win_mut(id)?.win_gravity = v as u8,
                6 => self.win_mut(id)?.backing_store = v as u8,
                7 => self.win_mut(id)?.backing_planes = v,
                8 => self.win_mut(id)?.backing_pixel = v,
                9 => {
                    let w = self.win_mut(id)?;
                    if w.override_redirect != (v != 0) {
                        w.override_redirect = v != 0;
                        self.damage_frame(id);
                    }
                }
                10 => self.win_mut(id)?.save_under = v != 0,
                11 => self.select_events(c, id, v)?,
                12 => self.win_mut(id)?.do_not_propagate = v & M_ALL,
                13 => self.win_mut(id)?.colormap = if v == 0 { DEFAULT_COLORMAP } else { v },
                _ => self.win_mut(id)?.cursor = v,
            }
        }
        Ok(())
    }

    /// Fija la máscara de eventos de un cliente sobre una ventana.
    fn select_events(&mut self, c: ClientId, id: Xid, mask: u32) -> XResult {
        if mask & !M_ALL != 0 {
            return Err(XError::new(BAD_VALUE, mask));
        }
        // Estos tres solo los puede seleccionar un cliente a la vez.
        let exclusive = M_SUBSTRUCTURE_REDIRECT | M_RESIZE_REDIRECT | M_BUTTON_PRESS;
        let w = self.win_mut(id)?;
        let taken = w
            .masks
            .iter()
            .filter(|(other, _)| *other != c)
            .fold(0, |acc, (_, m)| acc | m);
        if mask & exclusive & taken != 0 {
            return Err(XError::new(BAD_ACCESS, id));
        }
        w.masks.retain(|(other, _)| *other != c);
        if mask != 0 {
            w.masks.push((c, mask));
        }
        if id == ROOT {
            // Un gestor de ventanas de verdad: fuera decoraciones propias.
            let redirects = mask & M_SUBSTRUCTURE_REDIRECT != 0;
            let changed = if redirects {
                self.wm_client.replace(c) != Some(c)
            } else if self.wm_client == Some(c) {
                self.wm_client = None;
                true
            } else {
                false
            };
            if changed {
                self.wm_drag = None;
                self.damage_screen(self.screen_rect());
            }
        }
        Ok(())
    }

    fn get_window_attributes(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let id = rd.u32()?;
        let viewable = self.viewable(id);
        let w = self.win(id)?;
        let map_state = match (w.mapped, viewable) {
            (false, _) => 0,
            (true, false) => 1,
            (true, true) => 2,
        };
        let mut out = self.reply(c, w.backing_store);
        out.u32(ROOT_VISUAL)
            .u16(if w.input_only { 2 } else { 1 })
            .u8(w.bit_gravity)
            .u8(w.win_gravity)
            .u32(w.backing_planes)
            .u32(w.backing_pixel)
            .u8(w.save_under as u8)
            .u8(1)
            .u8(map_state)
            .u8(w.override_redirect as u8)
            .u32(w.colormap)
            .u32(w.all_masks())
            .u32(w.mask_of(c))
            .u16(w.do_not_propagate as u16)
            .u16(0);
        self.send_reply(c, out);
        Ok(())
    }

    fn configure_window(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let id = rd.u32()?;
        let mask = rd.u16()?;
        rd.skip(2)?;
        let win = self.win(id)?;
        let (parent, redirectable) = (win.parent, !win.override_redirect);
        let (mut x, mut y, mut w, mut h, mut bw) = (win.x, win.y, win.w, win.h, win.bw);
        let (mut sibling, mut stack) = (None, None);
        for bit in 0..7 {
            if mask & (1 << bit) == 0 {
                continue;
            }
            let v = rd.u32()?;
            match bit {
                0 => x = v as i16 as i32,
                1 => y = v as i16 as i32,
                2 => w = v & 0xffff,
                3 => h = v & 0xffff,
                4 => bw = v & 0xffff,
                5 => sibling = Some(v),
                _ => stack = Some(v as u8),
            }
        }
        if id == ROOT {
            return Ok(());
        }
        if mask & 0x0c != 0 && (w == 0 || h == 0) {
            return Err(XError::new(BAD_VALUE, 0));
        }
        if let Some(s) = sibling {
            if self.win(s)?.parent != parent || stack.is_none() {
                return Err(XError::new(BAD_MATCH, s));
            }
        }
        if redirectable && let Some(wm) = self.redirector(parent, c) {
            let ev = Ev::new(CONFIGURE_REQUEST, stack.unwrap_or(0))
                .u32(parent)
                .u32(id)
                .u32(sibling.unwrap_or(0))
                .i16(x as i16)
                .i16(y as i16)
                .u16(w as u16)
                .u16(h as u16)
                .u16(bw as u16)
                .u16(mask)
                .done();
            self.send(wm, ev);
            return Ok(());
        }
        // Con el gestor integrado, la barra de título no puede acabar fuera
        // de la pantalla porque el cliente pida una posición rara.
        let win = self.win(id)?;
        if mask & 3 != 0 && self.decorated(win) {
            x = x.min(self.width as i32 - 48).max(FRAME);
            y = y.min(self.height as i32 - 8).max(TITLE_H);
        }
        let before = (win.x, win.y, win.w, win.h, win.bw);
        self.move_resize(id, x, y, w, h, bw);
        if let Some(mode) = stack {
            self.restack(id, sibling, mode);
        }
        let win = self.win(id)?;
        let changed = before != (win.x, win.y, win.w, win.h, win.bw) || stack.is_some();
        // A las ventanas de primer nivel se les confirma siempre, como hace
        // un gestor de ventanas aunque no cambie nada.
        if changed || (parent == ROOT && mask & 0x0f != 0) {
            self.notify_configure(id);
        }
        self.refresh_pointer_window();
        Ok(())
    }

    fn circulate_window(&mut self, c: ClientId, direction: u8, rd: &mut Reader) -> XResult {
        let id = rd.u32()?;
        let children = self.win(id)?.children.clone();
        let mapped: Vec<Xid> = children
            .iter()
            .copied()
            .filter(|w| self.windows.get(w).is_some_and(|w| w.mapped))
            .collect();
        if mapped.len() < 2 {
            return Ok(());
        }
        // 0: sube la de más abajo; 1: baja la de más arriba.
        let (target, place) = if direction == 0 {
            (mapped[0], 0u8)
        } else {
            (mapped[mapped.len() - 1], 1u8)
        };
        let make = |code: u8, event: Xid| {
            Ev::new(code, 0)
                .u32(event)
                .u32(target)
                .u32(0)
                .u8(place)
                .done()
        };
        if let Some(wm) = self.redirector(id, c) {
            self.send(wm, make(CIRCULATE_REQUEST, id));
            return Ok(());
        }
        self.restack(target, None, place);
        self.deliver(target, M_STRUCTURE_NOTIFY, make(CIRCULATE_NOTIFY, target));
        self.deliver(id, M_SUBSTRUCTURE_NOTIFY, make(CIRCULATE_NOTIFY, id));
        self.refresh_pointer_window();
        Ok(())
    }

    fn get_geometry(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let id = rd.u32()?;
        let (depth, x, y, w, h, bw) = if let Some(w) = self.windows.get(&id) {
            (w.depth, w.x, w.y, w.w, w.h, w.bw)
        } else if let Some(p) = self.pixmaps.get(&id) {
            (p.image.depth, 0, 0, p.width, p.height, 0)
        } else {
            return Err(XError::new(BAD_DRAWABLE, id));
        };
        let mut out = self.reply(c, depth);
        out.u32(ROOT)
            .i16(x as i16)
            .i16(y as i16)
            .u16(w as u16)
            .u16(h as u16)
            .u16(bw as u16);
        self.send_reply(c, out);
        Ok(())
    }

    fn query_tree(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let id = rd.u32()?;
        let w = self.win(id)?;
        let parent = if id == ROOT { 0 } else { w.parent };
        let children = w.children.clone();
        let mut out = self.reply(c, 0);
        out.u32(ROOT)
            .u32(parent)
            .u16(children.len() as u16)
            .zeros(14);
        for child in children {
            out.u32(child);
        }
        self.send_reply(c, out);
        Ok(())
    }

    // --- Átomos y propiedades -----------------------------------------------

    fn intern_atom(&mut self, c: ClientId, only_if_exists: u8, rd: &mut Reader) -> XResult {
        let len = rd.u16()? as usize;
        rd.skip(2)?;
        let name: String = rd.bytes(len)?.iter().map(|b| *b as char).collect();
        if name.is_empty() {
            return Err(XError::new(BAD_VALUE, 0));
        }
        let atom = if only_if_exists != 0 {
            self.atoms.find(&name).unwrap_or(0)
        } else {
            self.atoms.intern(&name)
        };
        let mut w = self.reply(c, 0);
        w.u32(atom);
        self.send_reply(c, w);
        Ok(())
    }

    fn change_property(&mut self, c: ClientId, mode: u8, rd: &mut Reader) -> XResult {
        let (window, name, kind) = (rd.u32()?, rd.u32()?, rd.u32()?);
        let format = rd.u8()?;
        rd.skip(3)?;
        let units = rd.u32()? as usize;
        if !matches!(format, 8 | 16 | 32) {
            return Err(XError::new(BAD_VALUE, format as u32));
        }
        if mode > 2 {
            return Err(XError::new(BAD_VALUE, mode as u32));
        }
        self.check_atom(name)?;
        self.check_atom(kind)?;
        let len = units
            .checked_mul(format as usize / 8)
            .ok_or(XError::new(BAD_LENGTH, 0))?;
        let mut data = rd.bytes(len)?.to_vec();
        if self.clients[&c].be {
            swap_property_data(&mut data, format);
        }
        let existing = self.win(window)?.prop(name);
        let data = match (mode, existing) {
            (1 | 2, Some(old)) => {
                if old.kind != kind || old.format != format {
                    return Err(XError::new(BAD_MATCH, name));
                }
                if mode == 1 {
                    data.extend_from_slice(&old.data);
                    data
                } else {
                    let mut all = old.data.clone();
                    all.extend_from_slice(&data);
                    all
                }
            }
            _ => data,
        };
        self.set_property(
            window,
            Property {
                name,
                kind,
                format,
                data,
            },
        );
        Ok(())
    }

    fn get_property(&mut self, c: ClientId, delete: u8, rd: &mut Reader) -> XResult {
        let (window, name, kind) = (rd.u32()?, rd.u32()?, rd.u32()?);
        let (offset, length) = (rd.u32()? as usize, rd.u32()? as usize);
        self.check_atom(name)?;
        if kind != 0 {
            self.check_atom(kind)?;
        }
        let be = self.clients[&c].be;
        let Some(prop) = self.win(window)?.prop(name) else {
            let mut w = self.reply(c, 0);
            w.u32(0).u32(0).u32(0);
            self.send_reply(c, w);
            return Ok(());
        };
        if kind != 0 && kind != prop.kind {
            // Tipo distinto: se dice cuál es y cuánto ocupa, sin datos.
            let (format, actual, len) = (prop.format, prop.kind, prop.data.len());
            let mut w = self.reply(c, format);
            w.u32(actual).u32(len as u32).u32(0);
            self.send_reply(c, w);
            return Ok(());
        }
        let total = prop.data.len();
        let start = offset.saturating_mul(4);
        if start > total {
            return Err(XError::new(BAD_VALUE, offset as u32));
        }
        let end = total.min(start.saturating_add(length.saturating_mul(4)));
        let mut value = prop.data[start..end].to_vec();
        let (format, actual) = (prop.format, prop.kind);
        let after = total - end;
        if be {
            swap_property_data(&mut value, format);
        }
        let mut w = self.reply(c, format);
        w.u32(actual)
            .u32(after as u32)
            .u32((value.len() / (format as usize / 8).max(1)) as u32)
            .zeros(12)
            .bytes(&value);
        self.send_reply(c, w);
        if delete != 0 && after == 0 {
            self.delete_property(window, name);
        }
        Ok(())
    }

    fn rotate_properties(&mut self, rd: &mut Reader) -> XResult {
        let window = rd.u32()?;
        let (n, delta) = (rd.u16()? as usize, rd.i16()? as i64);
        let mut names = vec![];
        for _ in 0..n {
            names.push(rd.u32()?);
        }
        let w = self.win(window)?;
        for (i, name) in names.iter().enumerate() {
            self.check_atom(*name)?;
            if w.prop(*name).is_none() || names[..i].contains(name) {
                return Err(XError::new(BAD_MATCH, *name));
            }
        }
        if n == 0 || delta.rem_euclid(n as i64) == 0 {
            return Ok(());
        }
        // El valor de la propiedad i pasa a la que está «delta» más adelante.
        let values: Vec<Property> = names.iter().filter_map(|a| w.prop(*a).cloned()).collect();
        for (i, name) in names.iter().enumerate() {
            let from = (i as i64 - delta).rem_euclid(n as i64) as usize;
            let mut prop = values[from].clone();
            prop.name = *name;
            self.set_property(window, prop);
        }
        Ok(())
    }

    // --- Selecciones y eventos de cliente -----------------------------------

    fn convert_selection(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let (requestor, selection, target) = (rd.u32()?, rd.u32()?, rd.u32()?);
        let (property, time) = (rd.u32()?, rd.u32()?);
        self.win(requestor)?;
        self.check_atom(selection)?;
        self.check_atom(target)?;
        let notify = |property: Xid| {
            Ev::new(SELECTION_NOTIFY, 0)
                .u32(time)
                .u32(requestor)
                .u32(selection)
                .u32(target)
                .u32(property)
                .done()
        };
        match self.selections.get(&selection).copied() {
            // Nadie la tiene: se rechaza.
            None => self.send(c, notify(0)),
            // La tiene el propio servidor (portapapeles local).
            Some(owner) if owner.client == 0 => {
                let property = self.convert_own_selection(requestor, target, property);
                self.send(c, notify(property));
            }
            Some(owner) => {
                let ev = Ev::new(SELECTION_REQUEST, 0)
                    .u32(time)
                    .u32(owner.window)
                    .u32(requestor)
                    .u32(selection)
                    .u32(target)
                    .u32(property)
                    .done();
                self.send(owner.client, ev);
            }
        }
        Ok(())
    }

    fn send_event(&mut self, c: ClientId, propagate: u8, rd: &mut Reader) -> XResult {
        let (destination, mask) = (rd.u32()?, rd.u32()?);
        let be = self.clients[&c].be;
        let mut ev = Event::from_wire(rd.bytes(32)?, be);
        if !(2..=34).contains(&ev.code()) && ev.code() < 64 {
            return Err(XError::new(BAD_VALUE, ev.code() as u32));
        }
        ev.0[0] |= 0x80; // marca de «evento enviado por un cliente»
        let window = match destination {
            0 => self.pointer_window,
            1 => match self.focus {
                Focus::Window(f) => {
                    if self.is_ancestor(f, self.pointer_window) {
                        self.pointer_window
                    } else {
                        f
                    }
                }
                Focus::PointerRoot => self.pointer_window,
                Focus::None => return Ok(()),
            },
            w => {
                self.win(w)?;
                w
            }
        };
        if mask == 0 {
            let owner = self.win(window)?.owner;
            self.send(owner, ev);
            return Ok(());
        }
        let mut cur = window;
        loop {
            if self.deliver(cur, mask, ev) || propagate == 0 || cur == ROOT {
                break;
            }
            let w = self.win(cur)?;
            if w.do_not_propagate & mask != 0 {
                break;
            }
            cur = w.parent;
        }
        Ok(())
    }

    // --- Puntero ------------------------------------------------------------

    fn grab_pointer(&mut self, c: ClientId, owner_events: u8, rd: &mut Reader) -> XResult {
        let window = rd.u32()?;
        let mask = rd.u16()? as u32;
        self.win(window)?;
        let status = if self.grab.as_ref().is_some_and(|g| g.client != c) {
            1 // AlreadyGrabbed
        } else if !self.viewable(window) {
            3 // NotViewable
        } else {
            self.start_grab(PointerGrab {
                client: c,
                window,
                owner_events: owner_events != 0,
                mask,
                by_button: false,
                replay: None,
            });
            0
        };
        let w = self.reply(c, status);
        self.send_reply(c, w);
        Ok(())
    }

    fn grab_button(&mut self, c: ClientId, owner_events: u8, rd: &mut Reader) -> XResult {
        let window = rd.u32()?;
        let mask = rd.u16()? as u32;
        let (pointer_mode, _keyboard_mode) = (rd.u8()?, rd.u8()?);
        let (_confine, _cursor) = (rd.u32()?, rd.u32()?);
        let button = rd.u8()?;
        rd.skip(1)?;
        let modifiers = rd.u16()?;
        let w = self.win_mut(window)?;
        let same = |g: &PassiveGrab| !g.key && g.detail == button && g.modifiers == modifiers;
        if w.grabs.iter().any(|g| same(g) && g.client != c) {
            return Err(XError::new(BAD_ACCESS, window));
        }
        w.grabs.retain(|g| !same(g));
        w.grabs.push(PassiveGrab {
            client: c,
            key: false,
            detail: button,
            modifiers,
            owner_events: owner_events != 0,
            mask,
            sync: pointer_mode == 0,
        });
        Ok(())
    }

    fn query_pointer(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let window = rd.u32()?;
        self.win(window)?;
        if let Some(client) = self.clients.get_mut(&c) {
            client.motion_hint_sent = false;
        }
        let (ox, oy) = self.abs_origin(window);
        let child = self.child_towards(window, self.pointer_window);
        let mut w = self.reply(c, 1);
        w.u32(ROOT)
            .u32(child)
            .i16(self.pointer.0 as i16)
            .i16(self.pointer.1 as i16)
            .i16((self.pointer.0 - ox) as i16)
            .i16((self.pointer.1 - oy) as i16)
            .u16(self.input_state());
        self.send_reply(c, w);
        Ok(())
    }

    fn translate_coordinates(&mut self, c: ClientId, rd: &mut Reader) -> XResult {
        let (src, dst) = (rd.u32()?, rd.u32()?);
        let (x, y) = (rd.i16()? as i32, rd.i16()? as i32);
        self.win(src)?;
        let dst_win = self.win(dst)?;
        let (sx, sy) = self.abs_origin(src);
        let (dx, dy) = self.abs_origin(dst);
        let (rx, ry) = (sx + x - dx, sy + y - dy);
        // Hija del destino que contiene el punto.
        let child = dst_win
            .children
            .iter()
            .rev()
            .copied()
            .find(|ch| {
                self.windows
                    .get(ch)
                    .is_some_and(|w| w.mapped && w.outer().contains(rx, ry))
            })
            .unwrap_or(0);
        let mut w = self.reply(c, 1);
        w.u32(child).i16(rx as i16).i16(ry as i16);
        self.send_reply(c, w);
        Ok(())
    }

    /// Rectángulo de un *drawable* (su interior) o error `BadDrawable`.
    pub(super) fn drawable_bounds(&self, id: Xid) -> XResult<Rect> {
        if let Some(w) = self.windows.get(&id) {
            Ok(Rect::new(0, 0, w.w as i32, w.h as i32))
        } else if let Some(p) = self.pixmaps.get(&id) {
            Ok(Rect::new(0, 0, p.width as i32, p.height as i32))
        } else {
            Err(XError::new(BAD_DRAWABLE, id))
        }
    }
}
