#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
// Without a backend nothing drives the shared runtime (keys, gestures,
// help); only the public types are left to use.
#![cfg_attr(not(any(feature = "terminal", feature = "window")), allow(dead_code))]

mod app;
mod args;
#[cfg(feature = "clipboard")]
pub mod clipboard;
pub mod config;
#[cfg(feature = "toml")]
mod config_file;
pub mod gesture;
pub mod gpu;
mod help;
#[cfg(feature = "terminal")]
pub mod terminal;
mod waker;
#[cfg(feature = "window")]
pub mod window;

#[cfg(any(feature = "terminal", feature = "window"))]
use app::End;
pub use app::{App, Flow, ViewSlot};
pub use config::{Backend, Config, Mode, Palette, Protocol};
pub use gesture::{DragAction, Gesture, GestureKind, GestureType, Input, ScrollAction};
pub use gpu::{render_to_rgba, share, Gpu, GpuView, PixelRect, SharedView, Target, Views};
pub use help::Help;
pub use waker::Waker;

// The versions apps must match: a view renders with this crate's wgpu.
pub use crossterm;
pub use ratatui;
pub use wgpu;

/// Run `app` as configured: in the terminal or in a window. `views` are the
/// app's GPU views by name (see [`share`] and [`Views`]); `Views::new()`
/// for a text-only app.
///
/// With [`Config::switch_key`] set, pressing that key moves the running app
/// to the other mode and back, with the same `App` and views.
pub fn run(app: &mut dyn App, views: Views, config: &Config) -> Result<(), String> {
    #[cfg(not(any(feature = "terminal", feature = "window")))]
    return {
        let _ = (app, views);
        Err(format!(
            "{:?} mode is not compiled in; enable the scopekit feature",
            config.mode
        ))
    };
    #[cfg(any(feature = "terminal", feature = "window"))]
    {
        let started = config.mode;
        let mut mode = started;
        loop {
            // A window we switched into closes back to the terminal.
            let end = match mode {
                #[cfg(feature = "terminal")]
                Mode::Terminal => terminal::session(app, views.clone(), config)?,
                #[cfg(feature = "window")]
                Mode::Window => window::session(app, views.clone(), config, mode != started)?,
                #[allow(unreachable_patterns)]
                other => {
                    return Err(format!(
                        "{other:?} mode is not compiled in; enable the scopekit feature"
                    ))
                }
            };
            if end == End::Quit {
                return Ok(());
            }
            mode = match mode {
                Mode::Terminal => Mode::Window,
                Mode::Window => Mode::Terminal,
            };
            if mode == Mode::Window {
                if let Some(k) = config.switch_key {
                    eprintln!(
                        "{} runs in its window; press {k} there to come back.",
                        config.title
                    );
                }
            }
            app.mode_changed(mode);
        }
    }
}

/// The README's examples, compiled as doctests.
#[cfg(doctest)]
#[doc = include_str!("../HOWTO.md")]
struct HowtoDoctests;
