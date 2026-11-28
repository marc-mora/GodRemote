//! Ventanas sueltas del servidor X integrado: cada ventana de primer nivel
//! de las aplicaciones remotas se muestra como una ventana nativa del
//! sistema (un *viewport* de egui), con su título y su marco, en lugar de
//! ir todas dentro de la pestaña «Servidor X».
//!
//! El servidor publica la lista de ventanas con la imagen de cada una
//! ([`RootlessView`](crate::xserver::RootlessView)); aquí se crea una
//! ventana nativa por cada una y se le cuenta al servidor lo que pasa en
//! ellas ([`RootlessInput`]): el ratón (en coordenadas de su ventana), el
//! teclado de la que tiene el foco, dónde las deja el usuario al moverlas o
//! cambiarlas de tamaño y cuándo las cierra. La pantalla X tiene el tamaño
//! del monitor y cada ventana ocupa en ella el mismo sitio que en el
//! escritorio: así los menús y las listas desplegables, que las
//! aplicaciones colocan con coordenadas de pantalla, salen donde deben
//! (como ventanas sin marco).
//!
//! Hace falta que el sistema diga dónde está cada ventana y deje colocarlas
//! (no es así en Wayland) y que egui pueda abrir más de una ventana; si no,
//! [`XWindowsView::available`] lo dice y las aplicaciones se quedan en la
//! pestaña.

use std::collections::{HashMap, VecDeque};

use egui::{Color32, Event, EventFilter, Pos2, Rect, Sense, TextureHandle, TextureOptions, Vec2};

use super::desktop_view::{DesktopView, KeyRelay};
use crate::desktop::{MouseButton, SharedFrame};
use crate::i18n::tr;
use crate::trf;
use crate::xserver::{RootlessInput, RootlessRequest, RootlessWindow, XServer};

/// Color de lo que una ventana no pinta (fuera de su forma, o mientras la
/// aplicación no ha dibujado lo nuevo al agrandarla).
const BACKGROUND: Color32 = Color32::from_rgb(0xec, 0xec, 0xec);
/// Geometrías contadas al servidor que se recuerdan para reconocer su eco.
const ECHOES: usize = 8;
/// Segundos que se espera a que el sistema aplique un cambio de geometría
/// pedido por la aplicación antes de dar por buena la que tenga la ventana.
const SETTLE: f64 = 1.0;

/// Posición del interior y tamaño de una ventana, en píxeles de pantalla.
type Geometry = (i32, i32, u32, u32);

/// Textura con la imagen de una ventana X.
#[derive(Default)]
struct FrameTexture {
    texture: Option<TextureHandle>,
    epoch: u64,
    /// Tamaño de la textura (el de la imagen cuando se creó).
    size: (usize, usize),
}

impl FrameTexture {
    /// Copia a la textura lo que haya cambiado en la imagen.
    fn update(&mut self, ctx: &egui::Context, frame: &SharedFrame) {
        let mut frame = frame.lock().unwrap_or_else(|e| e.into_inner());
        let (w, h) = (frame.width() as usize, frame.height() as usize);
        if w == 0 || h == 0 {
            return;
        }
        let options = TextureOptions::LINEAR;
        // (Al cambiar de tamaño la ventana, el servidor le da una imagen
        // nueva: no basta con mirar su «epoch».)
        if self.texture.is_none() || self.epoch != frame.epoch() || self.size != (w, h) {
            frame.take_dirty();
            let image = egui::ColorImage::from_rgba_premultiplied([w, h], &frame.pixels);
            match &mut self.texture {
                Some(texture) => texture.set(image, options),
                None => self.texture = Some(ctx.load_texture("ventana-x", image, options)),
            }
            self.epoch = frame.epoch();
            self.size = (w, h);
        } else if let Some(dirty) = frame.take_dirty()
            && let Some(texture) = &mut self.texture
        {
            let (dx, dy) = (dirty.x as usize, dirty.y as usize);
            let (dw, dh) = (dirty.width as usize, dirty.height as usize);
            if dx + dw > w || dy + dh > h {
                return;
            }
            let mut region = Vec::with_capacity(dw * dh * 4);
            for row in dy..dy + dh {
                let start = (row * w + dx) * 4;
                region.extend_from_slice(&frame.pixels[start..start + dw * 4]);
            }
            let image = egui::ColorImage::from_rgba_premultiplied([dw, dh], &region);
            texture.set_partial([dx, dy], image, options);
        }
    }
}

/// Lo que se recuerda de la ventana nativa de una ventana X.
struct NativeWindow {
    texture: FrameTexture,
    /// Posición (exterior) y tamaño que se le piden al sistema: cambian
    /// cuando es la aplicación quien mueve o redimensiona su ventana.
    wanted_position: Option<Pos2>,
    wanted_size: Vec2,
    /// Geometría de X y de la ventana nativa en el fotograma anterior.
    last_x: Option<Geometry>,
    last_native: Option<Geometry>,
    /// Geometrías que se le han contado al servidor (su eco no es un cambio
    /// hecho por la aplicación).
    told: VecDeque<Geometry>,
    /// Desde cuándo la ventana nativa y la de X no coinciden.
    mismatch_since: Option<f64>,
    /// Lo que mide el marco del sistema por arriba y por la izquierda.
    frame_offset: Vec2,
    buttons: [bool; 3],
    last_pointer: Option<(i32, i32)>,
    wheel: Vec2,
    had_focus: bool,
}

impl NativeWindow {
    fn new(w: &RootlessWindow) -> Self {
        // Los emergentes van donde dice la aplicación; las demás, donde las
        // ponga el sistema salvo que la aplicación haya pedido un sitio.
        let asked = w.popup || (w.x, w.y) != (0, 0);
        NativeWindow {
            texture: FrameTexture::default(),
            wanted_position: asked.then(|| Pos2::new(w.x as f32, w.y as f32)),
            wanted_size: Vec2::new(w.width as f32, w.height as f32),
            last_x: None,
            last_native: None,
            told: VecDeque::new(),
            mismatch_since: None,
            frame_offset: Vec2::ZERO,
            buttons: [false; 3],
            last_pointer: None,
            wheel: Vec2::ZERO,
            had_focus: false,
        }
    }

    /// Cómo debe ser la ventana nativa. egui aplica al sistema lo que
    /// cambie de un fotograma a otro.
    fn builder(&self, w: &RootlessWindow) -> egui::ViewportBuilder {
        let title = if w.title.is_empty() {
            tr("Aplicación X").to_string()
        } else {
            w.title.clone()
        };
        let mut builder = egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size(self.wanted_size)
            .with_drag_and_drop(false);
        if let Some(position) = self.wanted_position {
            builder = builder.with_position(position);
        }
        if w.popup {
            return builder
                .with_decorations(false)
                .with_override_redirect(true)
                .with_window_type(egui::X11WindowType::PopupMenu)
                .with_taskbar(false)
                .with_always_on_top()
                .with_active(false)
                .with_resizable(false);
        }
        let size = |(w, h): (u32, u32)| Vec2::new(w as f32, h as f32);
        if let Some(min) = w.min_size {
            builder = builder.with_min_inner_size(size(min));
        }
        if let Some(max) = w.max_size {
            builder = builder.with_max_inner_size(size(max));
        }
        builder
            .with_decorations(w.decorated)
            .with_resizable(w.min_size.is_none() || w.min_size != w.max_size)
            .with_maximized(w.maximized)
            .with_fullscreen(w.fullscreen)
    }

    /// Mantiene de acuerdo la ventana nativa y la de X: si el usuario mueve
    /// o redimensiona la nativa, se le cuenta al servidor; si es la
    /// aplicación la que cambia la suya, se le pide al sistema.
    fn sync_geometry(&mut self, ctx: &egui::Context, w: &RootlessWindow, server: &XServer) {
        let (info, now) = ctx.input(|i| (i.viewport().clone(), i.time));
        if let (Some(inner), Some(outer)) = (info.inner_rect, info.outer_rect) {
            self.frame_offset = inner.min - outer.min;
        }
        let x_geometry: Geometry = (w.x, w.y, w.width, w.height);
        if w.popup {
            // Los emergentes siguen siempre a la aplicación.
            self.wanted_position = Some(Pos2::new(w.x as f32, w.y as f32));
            self.wanted_size = Vec2::new(w.width as f32, w.height as f32);
            return;
        }
        let Some(inner) = info.inner_rect else {
            return;
        };
        let native: Geometry = (
            inner.min.x.round() as i32,
            inner.min.y.round() as i32,
            inner.width().round().max(1.0) as u32,
            inner.height().round().max(1.0) as u32,
        );
        // ¿Ha cambiado la aplicación su ventana? (Lo que coincide con algo
        // que le contamos nosotros es el eco de eso mismo.)
        let by_app = self.last_x.is_some()
            && self.last_x != Some(x_geometry)
            && !self.told.contains(&x_geometry);
        self.last_x = Some(x_geometry);
        if by_app {
            if (x_geometry.2, x_geometry.3) != (native.2, native.3) {
                self.wanted_size = Vec2::new(w.width as f32, w.height as f32);
            }
            if (x_geometry.0, x_geometry.1) != (native.0, native.1) {
                self.wanted_position = Some(Pos2::new(w.x as f32, w.y as f32) - self.frame_offset);
            }
            self.told.clear();
            self.mismatch_since = Some(now);
            ctx.request_repaint();
        }
        let tell = |this: &mut Self| {
            server.rootless_send(RootlessInput::Geometry {
                window: w.id,
                x: native.0,
                y: native.1,
                width: native.2,
                height: native.3,
            });
            this.told.push_back(native);
            while this.told.len() > ECHOES {
                this.told.pop_front();
            }
            this.mismatch_since = None;
        };
        if native == x_geometry {
            self.mismatch_since = None;
        } else if self.last_native != Some(native) && !by_app {
            // La ventana nativa ha cambiado (o acaba de aparecer): manda ella.
            // (Mientras el sistema aplica un cambio pedido por la aplicación
            // se espera a ver en qué queda.)
            let settling = self
                .mismatch_since
                .is_some_and(|since| now - since < SETTLE);
            if !settling || self.last_native.is_none() {
                tell(self);
            }
        } else if let Some(since) = self.mismatch_since {
            // El sistema no ha hecho (del todo) lo que pedía la aplicación:
            // vale lo que haya quedado.
            if now - since >= SETTLE {
                tell(self);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        } else {
            self.mismatch_since = Some(now);
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        self.last_native = Some(native);
    }
}

/// Las ventanas nativas de las aplicaciones X.
#[derive(Default)]
pub struct XWindowsView {
    windows: HashMap<u32, NativeWindow>,
    keys: KeyRelay,
    /// Ventana X cuya ventana nativa tenía el foco en el último fotograma.
    focused: Option<u32>,
    /// Tamaño de pantalla que se le dijo al servidor.
    screen: (u32, u32),
    /// Número de ventanas en el último fotograma (para la pestaña).
    count: usize,
}

impl XWindowsView {
    /// Se pueden mostrar ventanas sueltas: egui puede abrir varias ventanas
    /// y el sistema dice dónde están.
    pub fn available(ctx: &egui::Context) -> bool {
        !ctx.embed_viewports() && ctx.input(|i| i.viewport().inner_rect.is_some())
    }

    /// Ventanas de aplicaciones abiertas ahora mismo.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Muestra (o actualiza) una ventana nativa por cada ventana de primer
    /// nivel del servidor. Hay que llamarla en cada fotograma mientras el
    /// servidor esté en marcha en este modo. `view` es la pantalla del
    /// servidor, de la que salen el cursor y el portapapeles remotos.
    pub fn show(&mut self, ctx: &egui::Context, server: &XServer, view: &mut DesktopView) {
        let Some(shared) = server.rootless() else {
            return;
        };
        view.poll();
        if let Some(text) = view.take_remote_clipboard() {
            ctx.copy_text(text);
        }
        let (windows, requests) = {
            let mut published = shared.lock().unwrap_or_else(|e| e.into_inner());
            (
                published.windows.clone(),
                std::mem::take(&mut published.requests),
            )
        };
        self.count = windows.iter().filter(|w| !w.popup).count();
        self.windows
            .retain(|id, _| windows.iter().any(|w| w.id == *id));

        // La pantalla X es el escritorio: al menos el monitor, y lo que
        // haga falta para abarcar las ventanas que estén más allá (en otro
        // monitor). Cuenta dónde las tiene el sistema, no dónde dice la
        // aplicación que quiere ponerlas: de eso no hay que fiarse.
        let monitor = ctx.input(|i| i.viewport().monitor_size);
        let mut screen = monitor.map_or((1280, 800), |m| (m.x.round() as u32, m.y.round() as u32));
        for (x, y, width, height) in self.windows.values().filter_map(|n| n.last_native) {
            screen.0 = screen.0.max((x.max(0) as u32).saturating_add(width));
            screen.1 = screen.1.max((y.max(0) as u32).saturating_add(height));
        }
        let screen = (screen.0.clamp(320, 16_384), screen.1.clamp(240, 16_384));
        if screen != self.screen {
            self.screen = screen;
            server.rootless_send(RootlessInput::Screen {
                width: screen.0,
                height: screen.1,
            });
        }

        let mut focused = None;
        let mut popup_focused = false;
        let Self {
            windows: natives,
            keys,
            ..
        } = self;
        for w in &windows {
            let native = natives.entry(w.id).or_insert_with(|| NativeWindow::new(w));
            let id = egui::ViewportId::from_hash_of(("godremote-x11", w.id));
            let builder = native.builder(w);
            let mine: Vec<RootlessRequest> = requests
                .iter()
                .copied()
                .filter(|r| request_window(r) == w.id)
                .collect();
            ctx.show_viewport_immediate(id, builder, |ui, class| {
                if class == egui::ViewportClass::EmbeddedWindow {
                    // (Sin ventanas de verdad no hay nada que hacer aquí.)
                    return;
                }
                if window_ui(ui, w, native, keys, server, view, &mine) {
                    if w.popup {
                        popup_focused = true;
                    } else {
                        focused = Some(w.id);
                    }
                }
            });
        }
        // (Si el sistema le da el foco a un emergente —un menú—, para X lo
        // sigue teniendo la ventana de la que salió.)
        if focused.is_none() && popup_focused {
            focused = self.focused;
        }

        // El foco de X sigue al de las ventanas nativas.
        if focused != self.focused {
            match focused {
                Some(window) => server.rootless_send(RootlessInput::Focused(window)),
                None => {
                    let mut send = |input| server.rootless_send(RootlessInput::Desktop(input));
                    self.keys.release(&mut send);
                    server.rootless_send(RootlessInput::Unfocused);
                }
            }
            self.focused = focused;
        }
    }
}

/// Ventana a la que se refiere una petición de una aplicación.
fn request_window(request: &RootlessRequest) -> u32 {
    match request {
        RootlessRequest::Activate(w)
        | RootlessRequest::Minimize(w)
        | RootlessRequest::Move(w)
        | RootlessRequest::Resize(w, _) => *w,
    }
}

/// Contenido y entrada de la ventana nativa de una ventana X. Devuelve si
/// tiene el foco del teclado.
fn window_ui(
    ui: &mut egui::Ui,
    w: &RootlessWindow,
    native: &mut NativeWindow,
    keys: &mut KeyRelay,
    server: &XServer,
    view: &mut DesktopView,
    requests: &[RootlessRequest],
) -> bool {
    let ctx = &ui.ctx().clone();
    native.sync_geometry(ctx, w, server);
    native.texture.update(ctx, &w.frame);
    let (close, has_focus) = ctx.input(|i| {
        (
            i.viewport().close_requested(),
            i.viewport().focused.unwrap_or(false),
        )
    });
    // El aspa del marco: se le pide a la aplicación que cierre (y la
    // ventana nativa sigue ahí hasta que lo haga).
    if close {
        ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        server.rootless_send(RootlessInput::Close(w.id));
    }
    // Lo que la aplicación le pide al sistema para esta ventana.
    for request in requests {
        match request {
            RootlessRequest::Activate(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Focus),
            RootlessRequest::Minimize(_) => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }
            RootlessRequest::Move(_) | RootlessRequest::Resize(..) => {
                // El sistema se queda el ratón hasta que se suelte.
                native.buttons = [false; 3];
                server.rootless_send(RootlessInput::DragStarted);
                ctx.send_viewport_cmd(match request {
                    RootlessRequest::Resize(_, [left, right, top, bottom]) => {
                        use egui::ResizeDirection as Dir;
                        egui::ViewportCommand::BeginResize(match (left, right, top, bottom) {
                            (true, _, true, _) => Dir::NorthWest,
                            (_, true, true, _) => Dir::NorthEast,
                            (true, _, _, true) => Dir::SouthWest,
                            (_, true, _, true) => Dir::SouthEast,
                            (true, ..) => Dir::West,
                            (_, true, ..) => Dir::East,
                            (_, _, true, _) => Dir::North,
                            _ => Dir::South,
                        })
                    }
                    _ => egui::ViewportCommand::StartDrag,
                });
            }
        }
    }

    let frame = egui::Frame::NONE.fill(BACKGROUND);
    egui::CentralPanel::default().frame(frame).show(ui, |ui| {
        let (area, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        if let Some(texture) = &native.texture.texture {
            let size = Vec2::new(w.width as f32, w.height as f32);
            ui.painter_at(area).image(
                texture.id(),
                Rect::from_min_size(area.min, size),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let to_window = |pos: Pos2| -> (i32, i32) {
            (
                (pos.x - area.min.x).floor() as i32,
                (pos.y - area.min.y).floor() as i32,
            )
        };
        // El teclado de la ventana va entero a la aplicación.
        if has_focus {
            response.request_focus();
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }
        let (modifiers, now) = ui.input(|i| (i.modifiers, i.time));
        let over = response.contains_pointer();
        let mut send = |input| server.rootless_send(RootlessInput::Desktop(input));
        for event in ui.input(|i| i.events.clone()) {
            match event {
                Event::PointerMoved(pos) => {
                    if over || native.buttons.iter().any(|b| *b) {
                        let (x, y) = to_window(pos);
                        if native.last_pointer != Some((x, y)) {
                            native.last_pointer = Some((x, y));
                            server.rootless_send(RootlessInput::Pointer { window: w.id, x, y });
                        }
                    }
                }
                Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    ..
                } => {
                    let (index, button) = match button {
                        egui::PointerButton::Primary => (0, MouseButton::Left),
                        egui::PointerButton::Middle => (1, MouseButton::Middle),
                        egui::PointerButton::Secondary => (2, MouseButton::Right),
                        _ => continue,
                    };
                    if pressed {
                        if !over {
                            continue;
                        }
                        native.buttons[index] = true;
                    } else if !std::mem::take(&mut native.buttons[index]) {
                        continue;
                    }
                    let (x, y) = to_window(pos);
                    native.last_pointer = Some((x, y));
                    server.rootless_send(RootlessInput::Button {
                        window: w.id,
                        x,
                        y,
                        button,
                        pressed,
                    });
                }
                Event::MouseWheel { unit, delta, .. } if over => {
                    let notches = match unit {
                        egui::MouseWheelUnit::Point => delta / 50.0,
                        egui::MouseWheelUnit::Line => delta,
                        egui::MouseWheelUnit::Page => delta * 5.0,
                    };
                    native.wheel += notches;
                    let (x, y) = native.last_pointer.unwrap_or((0, 0));
                    for (value, horizontal) in
                        [(&mut native.wheel.y, false), (&mut native.wheel.x, true)]
                    {
                        let whole = value.trunc();
                        if whole != 0.0 {
                            *value -= whole;
                            server.rootless_send(RootlessInput::Wheel {
                                window: w.id,
                                x,
                                y,
                                delta: whole.clamp(-20.0, 20.0) as i16,
                                horizontal,
                            });
                        }
                    }
                }
                event if has_focus => {
                    keys.event(&event, modifiers, true, now, &mut send);
                }
                _ => {}
            }
        }
        if has_focus {
            keys.sync_modifiers(modifiers, &mut send);
            // Al volver a una de estas ventanas el portapapeles local puede
            // haber cambiado: se pide para entregarlo (las aplicaciones X
            // pegan con el botón central, sin Ctrl+V).
            if !native.had_focus {
                keys.clipboard_requested = Some(now);
                ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            }
        }
        native.had_focus = has_focus;
        // El puntero que pide la aplicación.
        if let Some(pos) = ui.input(|i| i.pointer.latest_pos())
            && (over || native.buttons.iter().any(|b| *b))
        {
            view.cursor().show(ctx, pos, 1.0);
        }
    });
    has_focus
}

/// Lo que la pestaña «Servidor X» enseña cuando las aplicaciones van en
/// ventanas sueltas.
pub fn placeholder(ui: &mut egui::Ui, windows: usize) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.label(tr(
            "Las aplicaciones gráficas se muestran cada una en su propia ventana.",
        ));
        ui.weak(match windows {
            0 => tr("Ahora mismo no hay ninguna abierta.").to_string(),
            1 => tr("Hay 1 ventana abierta.").to_string(),
            n => trf!("Hay {n} ventanas abiertas.", n),
        });
        ui.weak(tr(
            "Cerrar esta pestaña para el servidor X y cierra esas aplicaciones.",
        ));
    });
}
