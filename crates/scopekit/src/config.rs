//! Everything a scopekit app can configure, in one struct with sensible
//! defaults. Build it with struct update syntax:
//!
//! ```
//! use scopekit::{Config, Mode, Protocol};
//!
//! let config = Config {
//!     title: "geodb globe".into(),
//!     mode: Mode::Window,
//!     font_size: 14.0,
//!     ..Config::default()
//! };
//! assert_eq!(config.protocol, Protocol::Auto);
//! ```

use std::path::PathBuf;
use std::time::Duration;

/// Where the app runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// In the terminal it was started from.
    #[default]
    Terminal,
    /// In its own native window.
    Window,
}

/// How the GPU view reaches the terminal (terminal mode only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Protocol {
    /// Ask the terminal what it supports; half blocks if it does not answer.
    #[default]
    Auto,
    /// The kitty graphics protocol (kitty, Ghostty, WezTerm).
    Kitty,
    /// iTerm2 inline images (iTerm2, WezTerm).
    Iterm2,
    /// DEC sixel graphics (foot, mlterm, xterm -ti vt340).
    Sixel,
    /// Unicode half blocks in 24-bit colour: works everywhere.
    Halfblocks,
}

/// The wgpu backend for the GPU view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backend {
    /// The best the platform offers (Metal, Vulkan or DX12, GL last).
    /// `WGPU_BACKEND` in the environment narrows it.
    #[default]
    Auto,
    /// Apple Metal.
    Metal,
    /// Vulkan.
    Vulkan,
    /// Direct3D 12.
    Dx12,
    /// OpenGL / OpenGL ES.
    Gl,
}

impl Backend {
    /// The wgpu backend set this selects.
    pub fn backends(self) -> wgpu::Backends {
        match self {
            Backend::Auto => wgpu::Backends::PRIMARY | wgpu::Backends::GL,
            Backend::Metal => wgpu::Backends::METAL,
            Backend::Vulkan => wgpu::Backends::VULKAN,
            Backend::Dx12 => wgpu::Backends::DX12,
            Backend::Gl => wgpu::Backends::GL,
        }
    }
}

/// The 16 named terminal colours as RGB, used in window mode, where there
/// is no terminal theme to take them from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub struct Palette {
    pub black: [u8; 3],
    pub red: [u8; 3],
    pub green: [u8; 3],
    pub yellow: [u8; 3],
    pub blue: [u8; 3],
    pub magenta: [u8; 3],
    pub cyan: [u8; 3],
    pub gray: [u8; 3],
    pub dark_gray: [u8; 3],
    pub light_red: [u8; 3],
    pub light_green: [u8; 3],
    pub light_yellow: [u8; 3],
    pub light_blue: [u8; 3],
    pub light_magenta: [u8; 3],
    pub light_cyan: [u8; 3],
    pub white: [u8; 3],
    /// Text colour for `Color::Reset`.
    pub foreground: [u8; 3],
    /// Background colour for `Color::Reset`.
    pub background: [u8; 3],
}

impl Palette {
    /// A dark terminal theme. Unlike CSS colour names, `dark_gray` is darker
    /// than `gray`, as it is in terminals.
    pub const DARK: Palette = Palette {
        black: [0x12, 0x12, 0x14],
        red: [0xe0, 0x5a, 0x5a],
        green: [0x6c, 0xc6, 0x6c],
        yellow: [0xe5, 0xc0, 0x5c],
        blue: [0x5c, 0x8f, 0xe5],
        magenta: [0xc6, 0x78, 0xdd],
        cyan: [0x56, 0xc8, 0xd8],
        gray: [0xb4, 0xb4, 0xb4],
        dark_gray: [0x4a, 0x4a, 0x50],
        light_red: [0xff, 0x7b, 0x7b],
        light_green: [0x8e, 0xe0, 0x8e],
        light_yellow: [0xff, 0xdc, 0x7c],
        light_blue: [0x82, 0xaa, 0xff],
        light_magenta: [0xe0, 0x9a, 0xf0],
        light_cyan: [0x7c, 0xe0, 0xee],
        white: [0xf0, 0xf0, 0xf0],
        foreground: [0xd8, 0xd8, 0xd8],
        background: [0x12, 0x12, 0x14],
    };

    /// A light theme for daylight.
    pub const LIGHT: Palette = Palette {
        black: [0x20, 0x20, 0x20],
        red: [0xc0, 0x30, 0x30],
        green: [0x2f, 0x8a, 0x2f],
        yellow: [0x9a, 0x70, 0x00],
        blue: [0x2a, 0x5d, 0xb8],
        magenta: [0x95, 0x3c, 0xb0],
        cyan: [0x1a, 0x80, 0x90],
        gray: [0x60, 0x60, 0x60],
        dark_gray: [0xc8, 0xc8, 0xcc],
        light_red: [0xe0, 0x50, 0x50],
        light_green: [0x40, 0xa0, 0x40],
        light_yellow: [0xb0, 0x88, 0x10],
        light_blue: [0x40, 0x78, 0xd8],
        light_magenta: [0xb0, 0x58, 0xc8],
        light_cyan: [0x20, 0x98, 0xa8],
        white: [0x10, 0x10, 0x10],
        foreground: [0x20, 0x20, 0x24],
        background: [0xfa, 0xfa, 0xf7],
    };
}

impl Default for Palette {
    fn default() -> Self {
        Palette::DARK
    }
}

/// How a scopekit app runs and looks.
#[derive(Debug, Clone)]
pub struct Config {
    /// Terminal or window.
    pub mode: Mode,
    /// GPU backend for the view.
    pub backend: Backend,
    /// Terminal graphics protocol (terminal mode).
    pub protocol: Protocol,
    /// Report mouse clicks, drags and the wheel to the app (terminal mode;
    /// the window always does). Users hold Shift to select text meanwhile.
    pub mouse: bool,
    /// Window title (window mode).
    pub title: String,
    /// Initial window size in logical pixels (window mode).
    pub window_size: (f64, f64),
    /// Text size in logical pixels, scaled by the display (window mode).
    pub font_size: f64,
    /// A TrueType or OpenType font file; `None` uses the bundled Cascadia
    /// Mono (window mode).
    pub font: Option<PathBuf>,
    /// Colours (window mode).
    pub palette: Palette,
    /// How long to wait for input before calling [`crate::App::tick`]
    /// when the app has no tick interval of its own.
    pub idle_poll: Duration,
    /// A key that moves the running app between the terminal and a window
    /// (`--switch-key p`). `None`, the default, turns switching off. While
    /// [`crate::App::captures_text`] returns `true` the key goes to the app
    /// instead, so a text field can still receive it. Needs both the
    /// `terminal` and `window` features.
    pub switch_key: Option<char>,
    /// The key that opens the built-in help box, for apps that describe
    /// themselves with [`crate::App::help`]. `None` leaves the key to the app.
    pub help_key: Option<char>,
    /// Mouse, trackpad and touch gesture bindings.
    pub input: crate::gesture::Input,
    /// A key that copies the GPU view under the pointer (or the first one)
    /// to the clipboard as an image (`--copy-key y`). `None`, the default,
    /// leaves the key to the app. Needs the `clipboard` feature.
    pub copy_key: Option<char>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mode: Mode::Terminal,
            backend: Backend::Auto,
            protocol: Protocol::Auto,
            mouse: true,
            title: "scopekit".into(),
            window_size: (1280.0, 800.0),
            font_size: 15.0,
            font: None,
            palette: Palette::DARK,
            idle_poll: Duration::from_millis(250),
            switch_key: None,
            help_key: Some('?'),
            input: crate::gesture::Input::default(),
            copy_key: None,
        }
    }
}
