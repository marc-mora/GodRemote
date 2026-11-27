//! Teclado y ratón: a qué ventana y a qué cliente va cada evento, cruces de
//! ventana (`Enter`/`Leave`) y capturas activas y pasivas.

use super::keymap::Keymap;
use super::proto::*;
use super::server::{ClientId, Focus, KeyboardGrab, PointerGrab, ROOT, Server};
use crate::desktop::char_keysym;

// Modos de los eventos de cruce.
const MODE_NORMAL: u8 = 0;
const MODE_GRAB: u8 = 1;
const MODE_UNGRAB: u8 = 2;

impl Server {
    /// Estado de modificadores y botones que llevan los eventos.
    pub fn input_state(&self) -> u16 {
        self.keymap.mods | self.buttons
    }

    fn button_bit(button: u8) -> u16 {
        match button {
            1..=5 => S_BUTTON1 << (button - 1),
            _ => 0,
        }
    }

    /// Evento de teclado o ratón visto desde una ventana.
    fn device_event(&self, code: u8, detail: u8, window: Xid, state: u16) -> Event {
        let (ox, oy) = self.abs_origin(window);
        let child = self.child_towards(window, self.pointer_window);
        Ev::new(code, detail)
            .u32(self.now())
            .u32(ROOT)
            .u32(window)
            .u32(child)
            .i16(self.pointer.0 as i16)
            .i16(self.pointer.1 as i16)
            .i16((self.pointer.0 - ox) as i16)
            .i16((self.pointer.1 - oy) as i16)
            .u16(state)
            .u8(1)
            .done()
    }

    /// Busca, subiendo desde `start`, la primera ventana donde algún cliente
    /// seleccionó `mask`. `stop_bit` es el bit que corta la propagación
    /// (`do-not-propagate`), `only` limita a un cliente y `last` es la última
    /// ventana que se mira.
    fn find_recipient(
        &self,
        start: Xid,
        mask: u32,
        stop_bit: u32,
        only: Option<ClientId>,
        last: Option<Xid>,
    ) -> Option<(Xid, Vec<ClientId>)> {
        let mut cur = start;
        loop {
            let w = self.windows.get(&cur)?;
            let clients: Vec<ClientId> = w
                .masks
                .iter()
                .filter(|(c, m)| m & mask != 0 && only.is_none_or(|o| o == *c))
                .map(|(c, _)| *c)
                .collect();
            if !clients.is_empty() {
                return Some((cur, clients));
            }
            if cur == ROOT || Some(cur) == last || w.do_not_propagate & stop_bit != 0 {
                return None;
            }
            cur = w.parent;
        }
    }

    // --- Cruces de ventana --------------------------------------------------

    fn cross_event(&mut self, code: u8, detail: u8, window: Xid, target: Xid, mode: u8) {
        let Some(w) = self.windows.get(&window) else {
            return;
        };
        let mask = if code == ENTER_NOTIFY {
            M_ENTER_WINDOW
        } else {
            M_LEAVE_WINDOW
        };
        let mut clients: Vec<ClientId> = w
            .masks
            .iter()
            .filter(|(_, m)| m & mask != 0)
            .map(|(c, _)| *c)
            .collect();
        // Con el puntero capturado solo se entera el cliente que lo tiene.
        if mode == MODE_NORMAL
            && let Some(g) = &self.grab
        {
            if g.owner_events {
                clients.retain(|c| *c == g.client);
            } else if g.window == window && g.mask & mask != 0 {
                clients = vec![g.client];
            } else {
                clients.clear();
            }
        }
        if clients.is_empty() {
            return;
        }
        let (ox, oy) = self.abs_origin(window);
        let focused = match self.focus {
            Focus::Window(f) => f == window || self.is_ancestor(f, window),
            Focus::PointerRoot => true,
            Focus::None => false,
        };
        let ev = Ev::new(code, detail)
            .u32(self.now())
            .u32(ROOT)
            .u32(window)
            .u32(self.child_towards(window, target))
            .i16(self.pointer.0 as i16)
            .i16(self.pointer.1 as i16)
            .i16((self.pointer.0 - ox) as i16)
            .i16((self.pointer.1 - oy) as i16)
            .u16(self.input_state())
            .u8(mode)
            .u8(2 | focused as u8)
            .done();
        for client in clients {
            if let Some(c) = self.clients.get_mut(&client) {
                c.motion_hint_sent = false;
            }
            self.send(client, ev);
        }
    }

    /// Eventos `Leave`/`Enter` de pasar el puntero de una ventana a otra.
    fn crossing(&mut self, from: Xid, to: Xid, mode: u8) {
        if from == to {
            return;
        }
        // Detalles: 0 antecesora, 1 virtual, 2 inferior, 3 no lineal,
        // 4 no lineal virtual.
        let (chain_from, chain_to) = (self.ancestry(from), self.ancestry(to));
        if chain_to.contains(&from) {
            // Entra en una descendiente.
            self.cross_event(LEAVE_NOTIFY, 2, from, to, mode);
            let between: Vec<Xid> = chain_to
                .iter()
                .skip(1)
                .take_while(|w| **w != from)
                .copied()
                .collect();
            for w in between.iter().rev() {
                self.cross_event(ENTER_NOTIFY, 1, *w, to, mode);
            }
            self.cross_event(ENTER_NOTIFY, 0, to, to, mode);
        } else if chain_from.contains(&to) {
            // Sale hacia una antecesora.
            self.cross_event(LEAVE_NOTIFY, 0, from, to, mode);
            for w in chain_from.iter().skip(1).take_while(|w| **w != to) {
                self.cross_event(LEAVE_NOTIFY, 1, *w, to, mode);
            }
            self.cross_event(ENTER_NOTIFY, 2, to, to, mode);
        } else {
            let common = chain_from
                .iter()
                .find(|w| chain_to.contains(w))
                .copied()
                .unwrap_or(ROOT);
            self.cross_event(LEAVE_NOTIFY, 3, from, to, mode);
            for w in chain_from.iter().skip(1).take_while(|w| **w != common) {
                self.cross_event(LEAVE_NOTIFY, 4, *w, to, mode);
            }
            let down: Vec<Xid> = chain_to
                .iter()
                .skip(1)
                .take_while(|w| **w != common)
                .copied()
                .collect();
            for w in down.iter().rev() {
                self.cross_event(ENTER_NOTIFY, 4, *w, to, mode);
            }
            self.cross_event(ENTER_NOTIFY, 3, to, to, mode);
        }
    }

    /// Recalcula qué ventana está bajo el puntero (el árbol o el puntero
    /// cambiaron) y genera los cruces.
    pub fn refresh_pointer_window(&mut self) {
        let new = self.pointer_target();
        let old = if self.windows.contains_key(&self.pointer_window) {
            self.pointer_window
        } else {
            ROOT
        };
        self.pointer_window = new;
        self.crossing(old, new, MODE_NORMAL);
    }

    // --- Capturas -----------------------------------------------------------

    pub fn start_grab(&mut self, grab: PointerGrab) {
        let already = self.grab.as_ref().map(|g| g.window);
        let window = grab.window;
        self.grab = Some(grab);
        if already.is_none() {
            self.crossing(self.pointer_window, window, MODE_GRAB);
        }
    }

    pub fn end_grab(&mut self) {
        if let Some(g) = self.grab.take() {
            self.crossing(g.window, self.pointer_window, MODE_UNGRAB);
        }
    }

    /// Activa la captura pasiva de botón que corresponda, buscando desde la
    /// raíz hacia la ventana del puntero (`below`: solo por debajo de esa).
    fn passive_button_grab(&mut self, button: u8, state: u16, below: Option<Xid>) -> bool {
        let mods = state & 0xff;
        let mut skipping = below.is_some();
        for win in self.ancestry(self.pointer_window).iter().rev() {
            if skipping {
                skipping = Some(*win) != below;
                continue;
            }
            let found = self.windows.get(win).and_then(|w| {
                w.grabs
                    .iter()
                    .find(|g| {
                        !g.key
                            && (g.detail == 0 || g.detail == button)
                            && (g.modifiers == S_ANY_MODIFIER || g.modifiers == mods)
                    })
                    .cloned()
            });
            if let Some(g) = found {
                self.start_grab(PointerGrab {
                    client: g.client,
                    window: *win,
                    owner_events: g.owner_events,
                    mask: g.mask,
                    by_button: true,
                    replay: g.sync.then_some((button, *win)),
                });
                return true;
            }
        }
        false
    }

    /// Entrega un evento de ratón con el puntero capturado.
    fn deliver_grabbed(&mut self, code: u8, detail: u8, mask: u32, state: u16) {
        let Some(g) = self.grab.clone() else {
            return;
        };
        if g.owner_events
            && let Some((win, _)) =
                self.find_recipient(self.pointer_window, mask, mask, Some(g.client), None)
        {
            let ev = self.device_event(code, detail, win, state);
            self.send(g.client, ev);
            return;
        }
        if g.mask & mask != 0 {
            let ev = self.device_event(code, detail, g.window, state);
            self.send(g.client, ev);
        }
    }

    /// `AllowEvents(ReplayPointer)`: la pulsación que activó una captura
    /// pasiva síncrona se reparte como si esa captura no existiera.
    pub fn replay_pointer(&mut self, client: ClientId) {
        let Some((button, window)) = self
            .grab
            .as_ref()
            .filter(|g| g.client == client)
            .and_then(|g| g.replay)
        else {
            return;
        };
        self.end_grab();
        let state = self.input_state() & !Self::button_bit(button);
        self.dispatch_button_press(button, state, Some(window));
    }

    // --- Ratón --------------------------------------------------------------

    /// Bits de máscara que seleccionan el movimiento actual.
    fn motion_mask(&self) -> u32 {
        let mut mask = M_POINTER_MOTION;
        if self.buttons != 0 {
            mask |= M_BUTTON_MOTION;
            for b in 0..5 {
                if self.buttons & (S_BUTTON1 << b) != 0 {
                    mask |= M_BUTTON1_MOTION << b;
                }
            }
        }
        mask
    }

    fn send_motion(&mut self, client: ClientId, window: Xid, selected: u32) {
        // Con «aviso de movimiento» se manda uno solo hasta que el cliente
        // pregunte dónde está el puntero.
        let hint = selected & M_POINTER_MOTION_HINT != 0;
        if hint {
            let Some(c) = self.clients.get_mut(&client) else {
                return;
            };
            if c.motion_hint_sent {
                return;
            }
            c.motion_hint_sent = true;
        }
        let ev = self.device_event(MOTION_NOTIFY, hint as u8, window, self.input_state());
        self.send(client, ev);
    }

    pub fn pointer_motion(&mut self, x: i32, y: i32) {
        let x = x.clamp(0, self.width as i32 - 1);
        let y = y.clamp(0, self.height as i32 - 1);
        if (x, y) == self.pointer {
            return;
        }
        self.pointer = (x, y);
        if self.wm_drag_motion() {
            return;
        }
        self.refresh_pointer_window();
        let mask = self.motion_mask();
        if let Some(g) = self.grab.clone() {
            if g.owner_events
                && let Some((win, _)) = self.find_recipient(
                    self.pointer_window,
                    mask,
                    M_POINTER_MOTION,
                    Some(g.client),
                    None,
                )
            {
                let selected = self.windows[&win].mask_of(g.client);
                self.send_motion(g.client, win, selected);
            } else if g.mask & mask != 0 {
                self.send_motion(g.client, g.window, g.mask);
            }
            return;
        }
        if let Some((win, clients)) =
            self.find_recipient(self.pointer_window, mask, M_POINTER_MOTION, None, None)
        {
            for client in clients {
                let selected = self.windows[&win].mask_of(client);
                self.send_motion(client, win, selected);
            }
        }
    }

    /// Reparte una pulsación sin captura activa: primero las capturas
    /// pasivas y, si no, al primer cliente que la seleccione (que se queda
    /// con una captura implícita hasta que se suelten los botones).
    fn dispatch_button_press(&mut self, button: u8, state: u16, below: Option<Xid>) {
        if self.passive_button_grab(button, state, below) {
            self.deliver_grabbed(BUTTON_PRESS, button, M_BUTTON_PRESS, state);
            return;
        }
        let Some((win, clients)) = self.find_recipient(
            self.pointer_window,
            M_BUTTON_PRESS,
            M_BUTTON_PRESS,
            None,
            None,
        ) else {
            return;
        };
        let client = clients[0];
        let mask = self.windows[&win].mask_of(client);
        self.start_grab(PointerGrab {
            client,
            window: win,
            owner_events: mask & M_OWNER_GRAB_BUTTON != 0,
            mask,
            by_button: true,
            replay: None,
        });
        let ev = self.device_event(BUTTON_PRESS, button, win, state);
        self.send(client, ev);
    }

    pub fn pointer_button(&mut self, button: u8, pressed: bool) {
        let state = self.input_state();
        let bit = Self::button_bit(button);
        for c in self.clients.values_mut() {
            c.motion_hint_sent = false;
        }
        if pressed {
            // El gestor integrado se queda con los clics en las decoraciones.
            if self.grab.is_none() && self.wm_button_press(button) {
                return;
            }
            self.buttons |= bit;
            if self.grab.is_some() {
                self.deliver_grabbed(BUTTON_PRESS, button, M_BUTTON_PRESS, state);
            } else {
                self.dispatch_button_press(button, state, None);
            }
            return;
        }
        if self.wm_button_release(button) {
            return;
        }
        self.buttons &= !bit;
        if self.grab.is_some() {
            self.deliver_grabbed(BUTTON_RELEASE, button, M_BUTTON_RELEASE, state);
            if self.buttons == 0 && self.grab.as_ref().is_some_and(|g| g.by_button) {
                self.end_grab();
            }
        } else if let Some((win, clients)) = self.find_recipient(
            self.pointer_window,
            M_BUTTON_RELEASE,
            M_BUTTON_RELEASE,
            None,
            None,
        ) {
            let ev = self.device_event(BUTTON_RELEASE, button, win, state);
            for client in clients {
                self.send(client, ev);
            }
        }
    }

    // --- Teclado ------------------------------------------------------------

    /// Ventana y clientes que reciben un evento de teclado según el foco.
    fn key_target(&self, mask: u32, only: Option<ClientId>) -> Option<(Xid, Vec<ClientId>)> {
        match self.focus {
            Focus::None => None,
            Focus::PointerRoot => self.find_recipient(self.pointer_window, mask, mask, only, None),
            Focus::Window(f) => {
                let inside = self.pointer_window == f || self.is_ancestor(f, self.pointer_window);
                let start = if inside { self.pointer_window } else { f };
                self.find_recipient(start, mask, mask, only, Some(f))
            }
        }
    }

    fn passive_key_grab(&mut self, code: u8, state: u16) -> bool {
        let target = match self.focus {
            Focus::Window(f) => f,
            Focus::PointerRoot => self.pointer_window,
            Focus::None => return false,
        };
        let mods = state & 0xff;
        for win in self.ancestry(target).iter().rev() {
            let found = self.windows.get(win).and_then(|w| {
                w.grabs
                    .iter()
                    .find(|g| {
                        g.key
                            && (g.detail == 0 || g.detail == code)
                            && (g.modifiers == S_ANY_MODIFIER || g.modifiers == mods)
                    })
                    .cloned()
            });
            if let Some(g) = found {
                self.keyboard_grab = Some(KeyboardGrab {
                    client: g.client,
                    window: *win,
                    owner_events: g.owner_events,
                    by_key: Some(code),
                });
                return true;
            }
        }
        false
    }

    /// Pulsación o suelta de una tecla por su código.
    pub fn key_event(&mut self, code: u8, pressed: bool) {
        let state = self.keymap.press(code, pressed) | self.buttons;
        let (kind, mask) = if pressed {
            (KEY_PRESS, M_KEY_PRESS)
        } else {
            (KEY_RELEASE, M_KEY_RELEASE)
        };
        for c in self.clients.values_mut() {
            c.motion_hint_sent = false;
        }
        if self.keyboard_grab.is_none() && pressed {
            self.passive_key_grab(code, state);
        }
        if let Some(g) = self.keyboard_grab.clone() {
            let window = match self.key_target(mask, Some(g.client)) {
                Some((win, _)) if g.owner_events => win,
                _ => g.window,
            };
            let ev = self.device_event(kind, code, window, state);
            self.send(g.client, ev);
            if !pressed && g.by_key == Some(code) {
                self.keyboard_grab = None;
            }
            return;
        }
        if let Some((win, clients)) = self.key_target(mask, None) {
            let ev = self.device_event(kind, code, win, state);
            for client in clients {
                self.send(client, ev);
            }
        }
    }

    /// Tecla especial o de atajo, por su *keysym*.
    pub fn input_key(&mut self, keysym: u32, pressed: bool) {
        if let Some(code) = self.keymap.keycode(keysym) {
            self.key_event(code, pressed);
        }
    }

    /// Texto ya compuesto: una pulsación y una suelta por carácter.
    pub fn input_text(&mut self, text: &str) {
        for c in text.chars() {
            let keysym = match c {
                '\n' | '\r' => 0xFF0D,
                '\t' => 0xFF09,
                '\u{8}' => 0xFF08,
                '\u{1b}' => 0xFF1B,
                '\u{7f}' => 0xFFFF,
                c if c < ' ' => continue,
                c => char_keysym(c),
            };
            let (code, changed) = self.keymap.keycode_or_assign(keysym);
            if changed {
                self.mapping_changed(1, code, 1);
            }
            // Cada carácter tiene su tecla: Mayús no pinta nada aquí (y en
            // las letras cambiaría la minúscula por la mayúscula).
            let shift = self.keymap.mods & S_SHIFT;
            self.keymap.mods &= !S_SHIFT;
            self.key_event(code, true);
            self.key_event(code, false);
            self.keymap.mods |= shift;
        }
    }

    /// `MappingNotify` a todos los clientes. `request`: 0 modificadores,
    /// 1 teclado, 2 puntero.
    pub fn mapping_changed(&mut self, request: u8, first: u8, count: u8) {
        let ev = Ev::new(MAPPING_NOTIFY, 0)
            .u8(request)
            .u8(first)
            .u8(count)
            .done();
        let clients: Vec<ClientId> = self.clients.keys().copied().collect();
        for client in clients {
            self.send(client, ev);
        }
    }

    /// Rueda: cada muesca es una pulsación de los botones 4/5 (vertical) o
    /// 6/7 (horizontal).
    pub fn input_wheel(&mut self, delta: i16, horizontal: bool) {
        let button = match (horizontal, delta > 0) {
            (false, true) => 4,
            (false, false) => 5,
            (true, false) => 6,
            (true, true) => 7,
        };
        for _ in 0..delta.unsigned_abs().min(20) {
            self.pointer_button(button, true);
            self.pointer_button(button, false);
        }
    }

    /// Símbolo principal de un código de tecla y si es un modificador.
    pub fn is_modifier_key(&self, code: u8) -> bool {
        Keymap::modifier_bit(self.keymap.keysyms(code)[0]) != 0
    }
}
