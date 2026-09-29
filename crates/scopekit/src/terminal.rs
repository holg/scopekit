//! Terminal mode: ratatui on crossterm, each GPU view rendered offscreen
//! and sent with the terminal's graphics protocol.

use crate::app::{App, Flow, ViewSlot};
use crate::config::{Config, Protocol};
use crate::gpu::{headless, Gpu, Offscreen, Views};
use crate::waker::Waker;
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
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
        }
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

/// What the loop waits for.
enum Msg {
    Input(Event),
    Wake,
}

/// Run `app` in the terminal until it returns [`Flow::Quit`]. The terminal
/// is restored on the way out, panics included.
pub fn run(app: &mut dyn App, views: Views, config: &Config) -> Result<(), String> {
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
) -> Result<(), String> {
    // The graphics query reads stdin, so it runs before the input thread.
    let mut driver = Driver::new(picker(config.protocol), views, config);
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
        let mut quit = false;
        match rx.recv_timeout(wait.saturating_sub(last_tick.elapsed())) {
            Ok(first) => {
                // Handle everything already queued, then draw once.
                for msg in std::iter::once(first).chain(rx.try_iter()) {
                    if let Msg::Input(ev) = msg {
                        let release =
                            matches!(&ev, Event::Key(k) if k.kind == KeyEventKind::Release);
                        if !release && app.event(ev) == Flow::Quit {
                            quit = true;
                            break;
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
        }
        if quit {
            break Ok(());
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
