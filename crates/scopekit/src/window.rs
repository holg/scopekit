//! Window mode: the same app in a native window. ratatui-wgpu draws the
//! text into a texture; the [`Compositor`] puts it on the window surface
//! and then lets each placed GPU view draw into its region, on the same device, in
//! the same frame, at the window's full resolution.

use crate::app::{App, Flow, ViewSlot};
use crate::config::{Config, Palette};
use crate::gpu::{instance, Gpu, PixelRect, Target, Views};
use crate::waker::Waker;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::backend::{Backend as _, WindowSize};
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::Terminal;
use ratatui_wgpu::shaders::DefaultPostProcessor;
use ratatui_wgpu::wgpu::{
    CommandEncoder, Device, Queue, SurfaceConfiguration, TextureFormat, TextureView,
};
use ratatui_wgpu::{Builder, ColorTable, Dimensions, Font, PostProcessor, WgpuBackend};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton as WinitButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

/// The bundled font: Cascadia Mono, SIL Open Font License 1.1
/// (`fonts/CascadiaOFL.txt`).
pub const FONT: &[u8] = include_bytes!("../fonts/CascadiaMono-Regular.ttf");

/// Wheel pixels per step (trackpads send many small deltas).
const WHEEL_STEP: f64 = 24.0;

/// What the event loop tells the compositor.
pub(crate) struct Shared {
    views: Views,
    /// Placed views and their pixel regions, this frame.
    regions: RefCell<Vec<(String, PixelRect)>>,
    dirty: Cell<bool>,
    describe: RefCell<String>,
}

/// ratatui-wgpu's post-processor: the text, then the GPU views on top.
pub(crate) struct Compositor {
    text: DefaultPostProcessor,
    device: Device,
    format: TextureFormat,
    gpu: Option<Gpu>,
    /// Views already given `prepare` on this device.
    prepared: HashSet<String>,
    shared: Rc<Shared>,
}

impl PostProcessor for Compositor {
    type UserData = Rc<Shared>;

    fn compile(
        device: &Device,
        text_view: &TextureView,
        config: &SurfaceConfiguration,
        shared: Rc<Shared>,
    ) -> Self {
        let i = device.adapter_info();
        *shared.describe.borrow_mut() = format!("{:?} · {} → window", i.backend, i.name);
        Compositor {
            text: DefaultPostProcessor::compile(device, text_view, config, ()),
            device: device.clone(),
            format: config.format,
            gpu: None,
            prepared: HashSet::new(),
            shared,
        }
    }

    fn resize(&mut self, device: &Device, text_view: &TextureView, config: &SurfaceConfiguration) {
        self.text.resize(device, text_view, config);
        self.shared.dirty.set(true);
    }

    fn process(
        &mut self,
        encoder: &mut CommandEncoder,
        queue: &Queue,
        text_view: &TextureView,
        config: &SurfaceConfiguration,
        surface_view: &TextureView,
    ) {
        self.text
            .process(encoder, queue, text_view, config, surface_view);
        self.shared.dirty.set(false);
        // The queue arrives only here: views are prepared on the first
        // frame that shows them.
        let gpu = self.gpu.get_or_insert_with(|| Gpu {
            device: self.device.clone(),
            queue: queue.clone(),
        });
        for (name, region) in self.shared.regions.borrow().iter() {
            let Some(view) = self.shared.views.get(name) else {
                continue;
            };
            let mut view = view.borrow_mut();
            if self.prepared.insert(name.clone()) {
                view.prepare(gpu, self.format);
            }
            let target = Target {
                view: surface_view,
                format: self.format,
                size: (config.width, config.height),
                region: *region,
            };
            view.render(gpu, encoder, &target);
        }
    }

    fn needs_update(&self) -> bool {
        self.shared.dirty.get()
            || self.shared.regions.borrow().iter().any(|(n, _)| {
                self.shared
                    .views
                    .get(n)
                    .is_some_and(|v| v.borrow().changed())
            })
    }
}

type WindowTerminal = Terminal<WgpuBackend<'static, 'static, Compositor>>;

fn color_table(p: &Palette) -> ColorTable {
    ColorTable {
        BLACK: p.black,
        RED: p.red,
        GREEN: p.green,
        YELLOW: p.yellow,
        BLUE: p.blue,
        MAGENTA: p.magenta,
        CYAN: p.cyan,
        GRAY: p.gray,
        DARKGRAY: p.dark_gray,
        LIGHTRED: p.light_red,
        LIGHTGREEN: p.light_green,
        LIGHTYELLOW: p.light_yellow,
        LIGHTBLUE: p.light_blue,
        LIGHTMAGENTA: p.light_magenta,
        LIGHTCYAN: p.light_cyan,
        WHITE: p.white,
    }
}

/// The cell under a surface pixel. The grid is stretched over the whole
/// surface, so the mapping is a plain proportion.
fn cell_at(size: &WindowSize, (x, y): (f64, f64)) -> (u16, u16) {
    let cell = |p: f64, px: u16, cells: u16| {
        let v = p * f64::from(cells) / f64::from(px.max(1));
        (v.max(0.0) as u16).min(cells.saturating_sub(1))
    };
    (
        cell(x, size.pixels.width, size.columns_rows.width),
        cell(y, size.pixels.height, size.columns_rows.height),
    )
}

/// Pixels covered by `area`, by the same proportion.
fn region_of(size: &WindowSize, area: Rect) -> PixelRect {
    let (cols, rows) = (
        u32::from(size.columns_rows.width.max(1)),
        u32::from(size.columns_rows.height.max(1)),
    );
    let (pw, ph) = (u32::from(size.pixels.width), u32::from(size.pixels.height));
    let x0 = u32::from(area.x) * pw / cols;
    let y0 = u32::from(area.y) * ph / rows;
    let x1 = u32::from(area.x + area.width) * pw / cols;
    let y1 = u32::from(area.y + area.height) * ph / rows;
    PixelRect {
        x: x0,
        y: y0,
        width: x1.saturating_sub(x0),
        height: y1.saturating_sub(y0),
    }
}

struct Handler<'a> {
    app: &'a mut dyn App,
    config: &'a Config,
    font: &'static [u8],
    shared: Rc<Shared>,
    window: Option<Arc<Window>>,
    terminal: Option<WindowTerminal>,
    modifiers: ModifiersState,
    cursor: (f64, f64),
    held: Option<MouseButton>,
    wheel: f64,
    last_tick: Instant,
    error: Option<String>,
}

impl Handler<'_> {
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let (w, h) = self.config.window_size;
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title(self.config.title.clone())
                        .with_inner_size(winit::dpi::LogicalSize::new(w, h)),
                )
                .map_err(|e| format!("cannot open a window: {e}"))?,
        );
        let size = window.inner_size();
        let font = Font::new(self.font).ok_or("the font is not a TrueType or OpenType font")?;
        let px = (self.config.font_size * window.scale_factor())
            .round()
            .max(6.0) as u32;
        let p = &self.config.palette;
        let rgb = |c: [u8; 3]| Color::Rgb(c[0], c[1], c[2]);
        let backend = pollster::block_on(
            Builder::<Compositor>::from_font_and_user_data(font, self.shared.clone())
                // With the display handle: GL on Wayland (EGL) needs it.
                .with_instance(instance(
                    self.config.backend,
                    wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(
                        event_loop.owned_display_handle(),
                    )),
                ))
                .with_font_size_px(px)
                .with_color_table(color_table(p))
                .with_fg_color(rgb(p.foreground))
                .with_bg_color(rgb(p.background))
                .with_width_and_height(Dimensions {
                    width: NonZeroU32::new(size.width.max(1)).unwrap_or(NonZeroU32::MIN),
                    height: NonZeroU32::new(size.height.max(1)).unwrap_or(NonZeroU32::MIN),
                })
                .build_with_target(window.clone()),
        )
        .map_err(|e| format!("no GPU for the window: {e}"))?;
        self.terminal = Some(Terminal::new(backend).map_err(|e| e.to_string())?);
        window.request_redraw();
        self.window = Some(window);
        Ok(())
    }

    fn window_size(&mut self) -> Option<WindowSize> {
        self.terminal.as_mut()?.backend_mut().window_size().ok()
    }

    fn redraw(&mut self) -> Result<(), String> {
        let Some(size) = self.window_size() else {
            return Ok(());
        };
        let cell_px = (
            f32::from(size.pixels.width) / f32::from(size.columns_rows.width.max(1)),
            f32::from(size.pixels.height) / f32::from(size.columns_rows.height.max(1)),
        );
        let mut slot = ViewSlot::new(cell_px, self.shared.describe.borrow().clone());
        let (app, shared) = (&mut *self.app, &self.shared);
        let Some(terminal) = self.terminal.as_mut() else {
            return Ok(());
        };
        terminal
            .draw(|f| {
                app.draw(f, &mut slot);
                // Before the frame is flushed: the compositor reads this.
                let regions: Vec<(String, PixelRect)> = slot
                    .placed()
                    .iter()
                    .map(|(n, r)| (n.clone(), region_of(&size, *r)))
                    .filter(|(_, r)| !r.is_empty())
                    .collect();
                if regions != *shared.regions.borrow() {
                    *shared.regions.borrow_mut() = regions;
                    shared.dirty.set(true);
                }
            })
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn send(&mut self, event_loop: &ActiveEventLoop, ev: Event) {
        if self.app.event(ev) == Flow::Quit {
            event_loop.exit();
        } else if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn mouse(&mut self, event_loop: &ActiveEventLoop, kind: MouseEventKind) {
        let Some(size) = self.window_size() else {
            return;
        };
        let (column, row) = cell_at(&size, self.cursor);
        let modifiers = mouse_modifiers(self.modifiers);
        self.send(
            event_loop,
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers,
            }),
        );
    }
}

/// A wake-up from another thread.
#[derive(Debug, Clone, Copy)]
struct Wake;

impl ApplicationHandler<Wake> for Handler<'_> {
    fn user_event(&mut self, _: &ActiveEventLoop, _: Wake) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(e) = self.start(event_loop) {
            self.error = Some(e);
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(t) = self.terminal.as_mut() {
                    t.backend_mut().resize(size.width, size.height);
                    // The backend starts a blank text texture on resize,
                    // but ratatui only sends changed cells: redraw all.
                    let _ = t.clear();
                }
                self.shared.dirty.set(true);
                let (w, h) = (
                    size.width.min(u32::from(u16::MAX)) as u16,
                    size.height.min(u32::from(u16::MAX)) as u16,
                );
                self.send(event_loop, Event::Resize(w, h));
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                // Cmd-Q and Cmd-W close the window, as any Mac app does.
                if self.modifiers.super_key()
                    && matches!(&event.logical_key, Key::Character(c) if c == "q" || c == "w")
                {
                    event_loop.exit();
                    return;
                }
                if let Some(k) = key_event(&event.logical_key, self.modifiers) {
                    self.send(event_loop, Event::Key(k));
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.wheel += match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y) * WHEEL_STEP,
                    MouseScrollDelta::PixelDelta(p) => p.y,
                };
                let steps = (self.wheel / WHEEL_STEP).trunc();
                self.wheel -= steps * WHEEL_STEP;
                let kind = if steps > 0.0 {
                    MouseEventKind::ScrollUp
                } else {
                    MouseEventKind::ScrollDown
                };
                for _ in 0..steps.abs() as u32 {
                    self.mouse(event_loop, kind);
                }
            }
            WindowEvent::PinchGesture { delta, .. } => {
                // No crossterm equivalent: a pinch arrives as Ctrl+wheel,
                // the usual zoom gesture.
                let kind = if delta > 0.0 {
                    MouseEventKind::ScrollUp
                } else {
                    MouseEventKind::ScrollDown
                };
                if delta.abs() > 0.01 {
                    let Some(size) = self.window_size() else {
                        return;
                    };
                    let (column, row) = cell_at(&size, self.cursor);
                    self.send(
                        event_loop,
                        Event::Mouse(MouseEvent {
                            kind,
                            column,
                            row,
                            modifiers: KeyModifiers::CONTROL,
                        }),
                    );
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let before = self.window_size().map(|s| cell_at(&s, self.cursor));
                self.cursor = (position.x, position.y);
                let now = self.window_size().map(|s| cell_at(&s, self.cursor));
                if before != now {
                    let kind = match self.held {
                        Some(b) => MouseEventKind::Drag(b),
                        None => MouseEventKind::Moved,
                    };
                    self.mouse(event_loop, kind);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    WinitButton::Left => MouseButton::Left,
                    WinitButton::Right => MouseButton::Right,
                    WinitButton::Middle => MouseButton::Middle,
                    _ => return,
                };
                let kind = if state == ElementState::Pressed {
                    self.held = Some(button);
                    MouseEventKind::Down(button)
                } else {
                    self.held = None;
                    MouseEventKind::Up(button)
                };
                self.mouse(event_loop, kind);
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.redraw() {
                    self.error = Some(e);
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(interval) = self.app.tick_interval() else {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        };
        if self.last_tick.elapsed() >= interval {
            self.last_tick = Instant::now();
            if self.app.tick() {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            self.last_tick + interval.max(Duration::from_millis(1)),
        ));
    }
}

/// A winit key as the crossterm key apps handle.
fn key_event(key: &Key, mods: ModifiersState) -> Option<KeyEvent> {
    let code = match key {
        Key::Named(n) => match n {
            NamedKey::ArrowUp => KeyCode::Up,
            NamedKey::ArrowDown => KeyCode::Down,
            NamedKey::ArrowLeft => KeyCode::Left,
            NamedKey::ArrowRight => KeyCode::Right,
            NamedKey::PageUp => KeyCode::PageUp,
            NamedKey::PageDown => KeyCode::PageDown,
            NamedKey::Home => KeyCode::Home,
            NamedKey::End => KeyCode::End,
            NamedKey::Enter => KeyCode::Enter,
            NamedKey::Escape => KeyCode::Esc,
            NamedKey::Backspace => KeyCode::Backspace,
            NamedKey::Delete => KeyCode::Delete,
            NamedKey::Insert => KeyCode::Insert,
            NamedKey::Space => KeyCode::Char(' '),
            NamedKey::Tab if mods.shift_key() => KeyCode::BackTab,
            NamedKey::Tab => KeyCode::Tab,
            NamedKey::F1 => KeyCode::F(1),
            NamedKey::F2 => KeyCode::F(2),
            NamedKey::F3 => KeyCode::F(3),
            NamedKey::F4 => KeyCode::F(4),
            NamedKey::F5 => KeyCode::F(5),
            NamedKey::F6 => KeyCode::F(6),
            NamedKey::F7 => KeyCode::F(7),
            NamedKey::F8 => KeyCode::F(8),
            NamedKey::F9 => KeyCode::F(9),
            NamedKey::F10 => KeyCode::F(10),
            NamedKey::F11 => KeyCode::F(11),
            NamedKey::F12 => KeyCode::F(12),
            _ => return None,
        },
        Key::Character(s) => KeyCode::Char(s.chars().next()?),
        _ => return None,
    };
    let mut m = KeyModifiers::NONE;
    if mods.control_key() {
        m |= KeyModifiers::CONTROL;
    }
    if mods.alt_key() {
        m |= KeyModifiers::ALT;
    }
    if mods.shift_key() && !matches!(code, KeyCode::Char(_) | KeyCode::BackTab) {
        m |= KeyModifiers::SHIFT;
    }
    Some(KeyEvent::new(code, m))
}

/// Modifiers on a mouse event. Cmd counts as Alt: on a Mac, Ctrl + scroll
/// is the system's screen zoom, so apps can take Alt (Option or Cmd) too.
fn mouse_modifiers(m: ModifiersState) -> KeyModifiers {
    let mut k = KeyModifiers::NONE;
    if m.control_key() {
        k |= KeyModifiers::CONTROL;
    }
    if m.alt_key() || m.super_key() {
        k |= KeyModifiers::ALT;
    }
    if m.shift_key() {
        k |= KeyModifiers::SHIFT;
    }
    k
}

/// Run `app` in a native window until it returns [`Flow::Quit`] or the
/// window is closed.
pub fn run(app: &mut dyn App, views: Views, config: &Config) -> Result<(), String> {
    let font: &'static [u8] = match &config.font {
        None => FONT,
        Some(p) => Box::leak(
            std::fs::read(p)
                .map_err(|e| format!("{}: {e}", p.display()))?
                .into_boxed_slice(),
        ),
    };
    let event_loop = EventLoop::<Wake>::with_user_event()
        .build()
        .map_err(|e| e.to_string())?;
    // A wake-up from any thread becomes a user event, then a redraw.
    let proxy = Mutex::new(event_loop.create_proxy());
    app.start(Waker::new(move || {
        if let Ok(p) = proxy.lock() {
            let _ = p.send_event(Wake);
        }
    }));
    let mut handler = Handler {
        app,
        config,
        font,
        shared: Rc::new(Shared {
            views,
            regions: RefCell::new(Vec::new()),
            dirty: Cell::new(true),
            describe: RefCell::new(String::new()),
        }),
        window: None,
        terminal: None,
        modifiers: ModifiersState::empty(),
        cursor: (0.0, 0.0),
        held: None,
        wheel: 0.0,
        last_tick: Instant::now(),
        error: None,
    };
    event_loop
        .run_app(&mut handler)
        .map_err(|e| e.to_string())?;
    match handler.error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
