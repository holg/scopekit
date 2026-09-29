//! Terminal mode: ratatui on crossterm, each GPU view rendered offscreen
//! and sent with the terminal's graphics protocol.

use crate::app::{is_copy_key, is_help_key, is_switch_key, App, End, Flow, ViewSlot};
use crate::config::{Config, Protocol};
use crate::gesture::{Button, Pointer, Recognizer};
use crate::gpu::PixelRect;
use crate::gpu::{headless, Gpu, Offscreen, Views};
use crate::waker::Waker;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, KeyModifiers, MouseButton,
    MouseEvent, MouseEventKind,
};
use image::{DynamicImage, RgbaImage};
use ratatui::backend::Backend;
use ratatui::layout::{Rect, Size};
use ratatui::Terminal;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol as Graphic;
use ratatui_image::{Image, Resize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// A picker for `protocol`. The terminal is asked for its cell size and
/// graphics support; one that does not answer gets half blocks. Call it
/// after entering the alternate screen and before reading events.
pub fn picker(protocol: Protocol) -> Picker {
    let mut p = match protocol {
        Protocol::Halfblocks => Picker::halfblocks(),
        _ => Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks()),
    };
    if let Some(t) = protocol_type(protocol) {
        p.set_protocol_type(t);
    }
    p
}

fn protocol_type(protocol: Protocol) -> Option<ProtocolType> {
    match protocol {
        Protocol::Auto => None,
        Protocol::Kitty => Some(ProtocolType::Kitty),
        Protocol::Iterm2 => Some(ProtocolType::Iterm2),
        Protocol::Sixel => Some(ProtocolType::Sixel),
        Protocol::Halfblocks => Some(ProtocolType::Halfblocks),
    }
}

/// `"kitty"`, `"iTerm2"`, `"sixel"` or `"half blocks"`.
pub fn protocol_name(p: &Picker) -> &'static str {
    match p.protocol_type() {
        ProtocolType::Halfblocks => "half blocks",
        ProtocolType::Sixel => "sixel",
        ProtocolType::Kitty => "kitty",
        ProtocolType::Iterm2 => "iTerm2",
    }
}

/// One view's offscreen target and the image last sent for it.
struct Shown {
    offscreen: Offscreen,
    graphic: Option<(Rect, Graphic)>,
}

/// Draws frames and routes events for one app on any ratatui backend.
/// [`run`] wraps it in the terminal's event loop; tests drive it directly
/// on a `TestBackend`.
pub struct Driver {
    picker: Picker,
    views: Views,
    backend: crate::Backend,
    gpu: Option<Result<Gpu, String>>,
    shown: HashMap<String, Shown>,
    error: Option<String>,
    config: Config,
    /// The built-in help box is open.
    help_open: bool,
    /// Where views were placed last frame, and the cell size, for gestures.
    layout: Vec<(String, Rect)>,
    cell_px: (f32, f32),
    recognizer: Recognizer,
    /// The cell last under the mouse, for the copy key.
    last_mouse: Option<(u16, u16)>,
}

impl Driver {
    /// A driver showing `views` with `picker`'s protocol.
    pub fn new(picker: Picker, views: Views, config: &Config) -> Driver {
        Driver {
            picker,
            views,
            backend: config.backend,
            gpu: None,
            shown: HashMap::new(),
            error: None,
            config: config.clone(),
            help_open: false,
            layout: Vec::new(),
            cell_px: (8.0, 16.0),
            recognizer: Recognizer::default(),
            last_mouse: None,
        }
    }

    /// Copy the view under the mouse (or the first one) to the clipboard
    /// as an image, at the pixel size it is shown at. Reports to the app.
    fn copy_view(&mut self, app: &mut dyn App) {
        let under = self.last_mouse.and_then(|(x, y)| {
            self.layout
                .iter()
                .rev()
                .find(|(_, r)| r.contains(ratatui::layout::Position::new(x, y)))
        });
        let Some((name, rect)) = under.or(self.layout.first()).cloned() else {
            app.message("no view to copy");
            return;
        };
        let Some(view) = self.views.get(&name).cloned() else {
            return;
        };
        let (cw, ch) = self.cell_px;
        let (w, h) = (
            (f32::from(rect.width) * cw).round() as u32,
            (f32::from(rect.height) * ch).round() as u32,
        );
        let Some(shown) = self.shown.get_mut(&name) else {
            app.message("the view has not been drawn yet");
            return;
        };
        let result = shown
            .offscreen
            .render(&mut *view.borrow_mut(), w, h)
            .and_then(|rgba| copy_image(&rgba, w.max(1), h.max(1)));
        match result {
            Ok(()) => app.message(&format!("copied {name} ({w} × {h}) to the clipboard")),
            Err(e) => app.message(&format!("copy failed: {e}")),
        }
        // The next frame shows the view as before.
        if let Some(s) = self.shown.get_mut(&name) {
            s.graphic = None;
        }
    }

    /// Whether the built-in help box is open.
    pub fn help_open(&self) -> bool {
        self.help_open
    }

    /// Open or close the built-in help box (it needs [`App::help`]).
    pub fn set_help_open(&mut self, open: bool) {
        self.help_open = open;
    }

    /// Route one event: the help box, gestures on views, then the app.
    /// Returns what the app asked for.
    pub fn event(&mut self, app: &mut dyn App, ev: Event) -> Flow {
        if self.help_open {
            // Any key or click only closes the help.
            if matches!(
                ev,
                Event::Key(_)
                    | Event::Mouse(MouseEvent {
                        kind: MouseEventKind::Down(_),
                        ..
                    })
            ) {
                self.help_open = false;
            }
            return Flow::Continue;
        }
        if let Event::Key(k) = &ev {
            if is_help_key(&self.config, app, k) {
                self.help_open = true;
                return Flow::Continue;
            }
            if is_copy_key(&self.config, app, k) {
                self.copy_view(app);
                return Flow::Continue;
            }
        }
        if let Event::Mouse(m) = &ev {
            self.last_mouse = Some((m.column, m.row));
        }
        let pointer = match &ev {
            Event::Mouse(m) => pointer(m, self.cell_px),
            _ => None,
        };
        if app.event(ev) == Flow::Quit {
            return Flow::Quit;
        }
        if let Some(p) = pointer {
            let (cw, ch) = self.cell_px;
            let views: Vec<(String, PixelRect)> = self
                .layout
                .iter()
                .map(|(n, r)| {
                    (
                        n.clone(),
                        PixelRect {
                            x: (f32::from(r.x) * cw) as u32,
                            y: (f32::from(r.y) * ch) as u32,
                            width: (f32::from(r.width) * cw) as u32,
                            height: (f32::from(r.height) * ch) as u32,
                        },
                    )
                })
                .collect();
            for g in self
                .recognizer
                .feed(&self.config.input, p, &views, std::time::Instant::now())
            {
                if app.gesture(g) == Flow::Quit {
                    return Flow::Quit;
                }
            }
        }
        Flow::Continue
    }

    /// Why the last view render failed, if it did.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Use another graphics protocol from the next frame on. `Auto` keeps
    /// the current one.
    pub fn set_protocol(&mut self, protocol: Protocol) {
        if let Some(t) = protocol_type(protocol) {
            self.picker.set_protocol_type(t);
            for s in self.shown.values_mut() {
                s.graphic = None;
            }
        }
    }

    fn describe(&self) -> String {
        let proto = protocol_name(&self.picker);
        match &self.gpu {
            Some(Ok(g)) => format!("{} → {proto}", g.describe()),
            Some(Err(e)) => format!("no GPU ({e})"),
            None => proto.to_string(),
        }
    }

    /// Draw one frame: the app's UI, then each view where it was placed.
    pub fn draw<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        app: &mut dyn App,
    ) -> Result<(), String> {
        let font = self.picker.font_size();
        let cell_px = (f32::from(font.width.max(1)), f32::from(font.height.max(1)));
        let mut slot = ViewSlot::new(cell_px, self.describe());
        let mut result = Ok(());
        terminal
            .draw(|f| {
                app.draw(f, &mut slot);
                // The help box hides the views: it would sit under an image.
                if self.help_open {
                    match app.help() {
                        Some(h) => {
                            slot.clear_views();
                            crate::help::draw(f, f.area(), &h, &self.config);
                        }
                        None => self.help_open = false,
                    }
                }
                self.layout = slot.placed().to_vec();
                self.cell_px = slot.cell_px();
                // Overlay text the image would cover (half blocks only: the
                // other protocols draw the image above all text anyway).
                let keep_overlay = self.picker.protocol_type() == ProtocolType::Halfblocks;
                let saved: Vec<(u16, u16, ratatui::buffer::Cell)> = if keep_overlay {
                    let buf = f.buffer_mut();
                    slot.overlays()
                        .iter()
                        .flat_map(|r| r.positions())
                        .filter_map(|p| buf.cell(p).map(|c| (p.x, p.y, c.clone())))
                        .collect()
                } else {
                    Vec::new()
                };
                // The app draws first so we know where each view goes; the
                // image is then rendered for exactly that area, over it.
                for (name, rect) in slot.placed() {
                    // Keep the first failure; later ones are usually its echo.
                    if let (Err(e), true) = (self.update(name, *rect, &slot), result.is_ok()) {
                        result = Err(e);
                    }
                    if let Some(Some((r, g))) = self.shown.get(name).map(|s| &s.graphic) {
                        f.render_widget(Image::new(g), *r);
                    }
                }
                let buf = f.buffer_mut();
                for (x, y, cell) in saved {
                    if let Some(c) = buf.cell_mut((x, y)) {
                        *c = cell;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        if let Err(e) = &result {
            self.error = Some(e.clone());
        }
        result
    }

    /// Render view `name` again when it changed or its area moved.
    fn update(&mut self, name: &str, rect: Rect, slot: &ViewSlot) -> Result<(), String> {
        let Some(view) = self.views.get(name).cloned() else {
            return Err(format!("no view called {name:?} was registered"));
        };
        let moved = self
            .shown
            .get(name)
            .and_then(|s| s.graphic.as_ref())
            .map(|(r, _)| *r)
            != Some(rect);
        if !moved && !view.borrow().changed() {
            return Ok(());
        }
        let backend = self.backend;
        let gpu = match self.gpu.get_or_insert_with(|| headless(backend)) {
            Ok(g) => g.clone(),
            Err(e) => return Err(e.clone()),
        };
        let shown = self.shown.entry(name.to_string()).or_insert_with(|| Shown {
            offscreen: Offscreen::new(gpu),
            graphic: None,
        });
        let (w, h) = slot.px_size(rect);
        let rgba = shown.offscreen.render(&mut *view.borrow_mut(), w, h)?;
        let image =
            RgbaImage::from_raw(w.max(1), h.max(1), rgba).ok_or("read-back has the wrong size")?;
        let graphic = self
            .picker
            .new_protocol(
                DynamicImage::ImageRgba8(image),
                Size::new(rect.width, rect.height),
                Resize::Fit(None),
            )
            .map_err(|e| e.to_string())?;
        shown.graphic = Some((rect, graphic));
        self.error = None;
        Ok(())
    }
}

/// ratatui-image's capability query timeout (its `QueryStdioOptions` default).
const QUERY_TIMEOUT: Duration = Duration::from_millis(2000);

/// The picker for a terminal session. The terminal is asked what it
/// supports once per process: switching back from a window reuses the
/// answer.
///
/// When the query times out (tmux does not route the replies to its
/// passthrough back), ratatui-image's reader thread stays blocked on stdin
/// and would swallow the user's next key. A plain status request, which
/// every terminal and tmux answer themselves, gives it the reply it waits
/// for, so it finishes before scopekit starts reading input.
fn session_picker(protocol: Protocol) -> Picker {
    static DETECTED: std::sync::Mutex<Option<Picker>> = std::sync::Mutex::new(None);
    if matches!(protocol, Protocol::Halfblocks) {
        return picker(protocol);
    }
    let cached = DETECTED.lock().ok().and_then(|g| g.clone());
    let mut p = match cached {
        Some(p) => p,
        None => {
            // ratatui-image answers a timeout with a fallback picker, not an
            // error; a query that took (almost) the whole timeout timed out.
            let started = std::time::Instant::now();
            let p = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
            if started.elapsed() >= QUERY_TIMEOUT.mul_f32(0.9) {
                release_query_reader();
            }
            if let Ok(mut g) = DETECTED.lock() {
                *g = Some(p.clone());
            }
            p
        }
    };
    if let Some(t) = protocol_type(protocol) {
        p.set_protocol_type(t);
    }
    p
}

/// Answer a timed-out capability query's reader thread (see above), then
/// drop anything left of the reply.
fn release_query_reader() {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x1b[5n");
    let _ = out.flush();
    std::thread::sleep(Duration::from_millis(100));
    while event::poll(Duration::ZERO).unwrap_or(false) {
        if event::read().is_err() {
            break;
        }
    }
}

/// What the loop waits for.
enum Msg {
    Input(Event),
    Wake,
}

/// Run `app` in the terminal until it returns [`Flow::Quit`]. The terminal
/// is restored on the way out, panics included.
pub fn run(app: &mut dyn App, views: Views, config: &Config) -> Result<(), String> {
    session(app, views, config).map(|_| ())
}

/// One stay in the terminal: until the app quits or the switch key is pressed.
pub(crate) fn session(app: &mut dyn App, views: Views, config: &Config) -> Result<End, String> {
    let mut terminal = ratatui::init();
    if config.mouse {
        let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
    }
    let result = run_loop(&mut terminal, app, views, config);
    if config.mouse {
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    }
    ratatui::restore();
    result
}

fn run_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut dyn App,
    views: Views,
    config: &Config,
) -> Result<End, String> {
    // The graphics query reads stdin, so it runs before the input thread.
    let mut driver = Driver::new(session_picker(config.protocol), views, config);
    let (tx, rx) = mpsc::channel::<Msg>();
    let stop = Arc::new(AtomicBool::new(false));
    let input = {
        let (tx, stop) = (tx.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match event::poll(Duration::from_millis(50)) {
                    Ok(true) => match event::read() {
                        Ok(ev) => {
                            if tx.send(Msg::Input(ev)).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    },
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
        })
    };
    app.start(Waker::new(move || {
        let _ = tx.send(Msg::Wake);
    }));

    let mut last_tick = Instant::now();
    let result = loop {
        if let Err(e) = driver.draw(terminal, app) {
            break Err(e);
        }
        let wait = app.tick_interval().unwrap_or(config.idle_poll);
        let mut end = None;
        match rx.recv_timeout(wait.saturating_sub(last_tick.elapsed())) {
            Ok(first) => {
                // Handle everything already queued, then draw once.
                for msg in std::iter::once(first).chain(rx.try_iter()) {
                    if let Msg::Input(ev) = msg {
                        let release =
                            matches!(&ev, Event::Key(k) if k.kind == KeyEventKind::Release);
                        if release {
                            continue;
                        }
                        if let Event::Key(k) = &ev {
                            if !driver.help_open() && is_switch_key(config, app, k) {
                                end = Some(End::Switch);
                                break;
                            }
                        }
                        if driver.event(app, ev) == Flow::Quit {
                            end = Some(End::Quit);
                            break;
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(End::Quit),
        }
        if let Some(end) = end {
            break Ok(end);
        }
        if last_tick.elapsed() >= wait {
            last_tick = Instant::now();
            app.tick();
        }
    };
    stop.store(true, Ordering::Relaxed);
    let _ = input.join();
    result
}

#[cfg(feature = "clipboard")]
fn copy_image(rgba: &[u8], w: u32, h: u32) -> Result<(), String> {
    crate::clipboard::copy_image(rgba, w, h)
}

#[cfg(not(feature = "clipboard"))]
fn copy_image(_: &[u8], _: u32, _: u32) -> Result<(), String> {
    Err("scopekit was built without the clipboard feature".into())
}

/// Pointer id and kind of a mouse button.
fn button(b: MouseButton) -> (u64, Button) {
    match b {
        MouseButton::Left => (1, Button::Left),
        MouseButton::Right => (2, Button::Right),
        MouseButton::Middle => (3, Button::Middle),
    }
}

/// A terminal mouse event as pointer input at the centre of its cell.
fn pointer(m: &MouseEvent, (cw, ch): (f32, f32)) -> Option<Pointer> {
    let pos = (
        (f32::from(m.column) + 0.5) * cw,
        (f32::from(m.row) + 0.5) * ch,
    );
    let modifier = m
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
    let scroll = |delta| Pointer::Scroll {
        delta,
        precise: false,
        modifier,
        pos,
    };
    Some(match m.kind {
        MouseEventKind::Down(b) => {
            let (id, button) = button(b);
            Pointer::Down { id, button, pos }
        }
        MouseEventKind::Drag(b) => Pointer::Move {
            id: button(b).0,
            pos,
        },
        MouseEventKind::Up(b) => Pointer::Up {
            id: button(b).0,
            pos,
        },
        MouseEventKind::ScrollUp => scroll((0.0, 1.0)),
        MouseEventKind::ScrollDown => scroll((0.0, -1.0)),
        MouseEventKind::ScrollLeft => scroll((1.0, 0.0)),
        MouseEventKind::ScrollRight => scroll((-1.0, 0.0)),
        MouseEventKind::Moved => return None,
    })
}

/// Print an RGBA image into the terminal's scrollback, as wide as the
/// terminal at most (for command-line tools: `render --inline`).
pub fn print_image(
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    protocol: Protocol,
) -> Result<(), String> {
    let picker = picker(protocol);
    let image = RgbaImage::from_raw(width, height, rgba).ok_or("pixels do not match the size")?;
    let (tw, _) = crossterm::terminal::size().unwrap_or((80, 24));
    let font = picker.font_size();
    let (cw, ch) = (u32::from(font.width.max(1)), u32::from(font.height.max(1)));
    let cols = (width.div_ceil(cw) as u16).clamp(1, tw.max(1));
    let rows = ((height as f32 / width.max(1) as f32 * f32::from(cols) * cw as f32 / ch as f32)
        .ceil() as u16)
        .max(1);
    let graphic = picker
        .new_protocol(
            DynamicImage::ImageRgba8(image),
            Size::new(cols, rows),
            Resize::Fit(None),
        )
        .map_err(|e| e.to_string())?;
    let mut terminal = Terminal::with_options(
        ratatui::backend::CrosstermBackend::new(std::io::stdout()),
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Inline(rows),
        },
    )
    .map_err(|e| e.to_string())?;
    terminal
        .draw(|f| f.render_widget(Image::new(&graphic), f.area()))
        .map_err(|e| e.to_string())?;
    println!();
    Ok(())
}
