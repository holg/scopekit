//! `Config` from TOML, so each project can ship its look and settings as
//! a file. Every key is optional; what is left out keeps its default.
//!
//! ```
//! let config = scopekit::Config::from_toml(r##"
//!     mode = "window"
//!     title = "geodb globe"
//!     font_size = 14
//!     protocol = "kitty"
//!     palette = "light"
//!
//!     [colors]            # individual colours over the palette
//!     background = "#101418"
//!     dark_gray = "#3a3f46"
//! "##).unwrap();
//! assert_eq!(config.mode, scopekit::Mode::Window);
//! assert_eq!(config.palette.background, [0x10, 0x14, 0x18]);
//! assert_eq!(config.palette.red, scopekit::Palette::LIGHT.red);
//! ```

use crate::config::{Backend, Config, Mode, Palette, Protocol};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    mode: Option<String>,
    backend: Option<String>,
    protocol: Option<String>,
    mouse: Option<bool>,
    title: Option<String>,
    window_size: Option<[f64; 2]>,
    font_size: Option<f64>,
    font: Option<PathBuf>,
    palette: Option<String>,
    colors: Option<BTreeMap<String, String>>,
    idle_poll_ms: Option<u64>,
}

fn pick<T: Copy>(what: &str, value: &str, options: &[(&str, T)]) -> Result<T, String> {
    options
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(value))
        .map(|(_, v)| *v)
        .ok_or_else(|| {
            let names: Vec<&str> = options.iter().map(|(n, _)| *n).collect();
            format!("{what} = {value:?}: expected one of {}", names.join(", "))
        })
}

fn hex(s: &str) -> Result<[u8; 3], String> {
    let h = s.strip_prefix('#').unwrap_or(s);
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("x"), 16);
    match (h.len(), byte(0), byte(2), byte(4)) {
        (6, Ok(r), Ok(g), Ok(b)) => Ok([r, g, b]),
        _ => Err(format!("{s:?} is not a colour like \"#1a2b3c\"")),
    }
}

impl Config {
    /// Read a configuration from TOML text, over the defaults.
    pub fn from_toml(text: &str) -> Result<Config, String> {
        let f: File = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut c = Config::default();
        if let Some(v) = &f.mode {
            c.mode = pick(
                "mode",
                v,
                &[("terminal", Mode::Terminal), ("window", Mode::Window)],
            )?;
        }
        if let Some(v) = &f.backend {
            c.backend = pick(
                "backend",
                v,
                &[
                    ("auto", Backend::Auto),
                    ("metal", Backend::Metal),
                    ("vulkan", Backend::Vulkan),
                    ("dx12", Backend::Dx12),
                    ("gl", Backend::Gl),
                ],
            )?;
        }
        if let Some(v) = &f.protocol {
            c.protocol = pick(
                "protocol",
                v,
                &[
                    ("auto", Protocol::Auto),
                    ("kitty", Protocol::Kitty),
                    ("iterm2", Protocol::Iterm2),
                    ("sixel", Protocol::Sixel),
                    ("halfblocks", Protocol::Halfblocks),
                ],
            )?;
        }
        if let Some(v) = f.mouse {
            c.mouse = v;
        }
        if let Some(v) = f.title {
            c.title = v;
        }
        if let Some([w, h]) = f.window_size {
            c.window_size = (w, h);
        }
        if let Some(v) = f.font_size {
            c.font_size = v;
        }
        if f.font.is_some() {
            c.font = f.font;
        }
        if let Some(v) = &f.palette {
            c.palette = pick(
                "palette",
                v,
                &[("dark", Palette::DARK), ("light", Palette::LIGHT)],
            )?;
        }
        for (name, value) in f.colors.unwrap_or_default() {
            let rgb = hex(&value).map_err(|e| format!("colors.{name}: {e}"))?;
            let p = &mut c.palette;
            let slot = match name.as_str() {
                "black" => &mut p.black,
                "red" => &mut p.red,
                "green" => &mut p.green,
                "yellow" => &mut p.yellow,
                "blue" => &mut p.blue,
                "magenta" => &mut p.magenta,
                "cyan" => &mut p.cyan,
                "gray" => &mut p.gray,
                "dark_gray" => &mut p.dark_gray,
                "light_red" => &mut p.light_red,
                "light_green" => &mut p.light_green,
                "light_yellow" => &mut p.light_yellow,
                "light_blue" => &mut p.light_blue,
                "light_magenta" => &mut p.light_magenta,
                "light_cyan" => &mut p.light_cyan,
                "white" => &mut p.white,
                "foreground" => &mut p.foreground,
                "background" => &mut p.background,
                other => return Err(format!("colors.{other}: not a palette colour")),
            };
            *slot = rgb;
        }
        if let Some(ms) = f.idle_poll_ms {
            c.idle_poll = Duration::from_millis(ms.max(1));
        }
        Ok(c)
    }

    /// Read a configuration file. A relative `font` path is taken
    /// relative to the file.
    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut c = Config::from_toml(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if let (Some(font), Some(dir)) = (&c.font, path.parent()) {
            if font.is_relative() {
                c.font = Some(dir.join(font));
            }
        }
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mistakes_are_named() {
        let e = Config::from_toml("mode = \"tv\"").unwrap_err();
        assert!(e.contains("terminal, window"), "{e}");
        let e = Config::from_toml("[colors]\nbackground = \"#12\"").unwrap_err();
        assert!(e.contains("colors.background"), "{e}");
        let e = Config::from_toml("titel = \"typo\"").unwrap_err();
        assert!(e.contains("titel"), "{e}");
        assert_eq!(Config::from_toml("").unwrap().title, "scopekit");
    }
}
