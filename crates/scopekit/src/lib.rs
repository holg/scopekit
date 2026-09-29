#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod app;
mod args;
pub mod config;
#[cfg(feature = "toml")]
mod config_file;
pub mod gpu;
#[cfg(feature = "terminal")]
pub mod terminal;
mod waker;
#[cfg(feature = "window")]
pub mod window;

pub use app::{App, Flow, ViewSlot};
pub use config::{Backend, Config, Mode, Palette, Protocol};
pub use gpu::{render_to_rgba, share, Gpu, GpuView, PixelRect, SharedView, Target, Views};
pub use waker::Waker;

// The versions apps must match: a view renders with this crate's wgpu.
pub use crossterm;
pub use ratatui;
pub use wgpu;

/// Run `app` as configured: in the terminal or in a window. `views` are the
/// app's GPU views by name (see [`share`] and [`Views`]); `Views::new()`
/// for a text-only app.
pub fn run(app: &mut dyn App, views: Views, config: &Config) -> Result<(), String> {
    #[cfg(not(any(feature = "terminal", feature = "window")))]
    let _ = (app, views);
    match config.mode {
        #[cfg(feature = "terminal")]
        Mode::Terminal => terminal::run(app, views, config),
        #[cfg(feature = "window")]
        Mode::Window => window::run(app, views, config),
        #[allow(unreachable_patterns)]
        other => Err(format!(
            "{other:?} mode is not compiled in; enable the scopekit feature"
        )),
    }
}

/// The README's examples, compiled as doctests.
#[cfg(doctest)]
#[doc = include_str!("../HOWTO.md")]
struct HowtoDoctests;
