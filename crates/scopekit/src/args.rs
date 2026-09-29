//! The command-line flags every scopekit app understands, parsed once so
//! they are the same everywhere:
//!
//! | Flag | Sets |
//! |---|---|
//! | `--window`, `-w` / `--terminal` | [`Config::mode`] |
//! | `--backend auto\|metal\|vulkan\|dx12\|gl` | [`Config::backend`] |
//! | `--protocol auto\|kitty\|iterm2\|sixel\|halfblocks` | [`Config::protocol`] |
//! | `--font FILE`, `--font-size N` | window text |
//! | `--title TEXT` | window title |
//! | `--no-mouse` | [`Config::mouse`] off |
//! | `--no-gestures` | [`Input::gestures`](crate::Input::gestures) off: raw mouse events only |
//! | `--switch-key C` | [`Config::switch_key`]: C moves the app between terminal and window |
//! | `--copy-key C` | [`Config::copy_key`]: C copies the view under the pointer as an image |
//! | `--config FILE` | a TOML file first, flags on top (`toml` feature) |
//!
//! `SCOPEKIT_MODE=window` (or `terminal`) sets the mode from the
//! environment; a flag wins over it. Everything else is handed back for the
//! app's own parsing, in order:
//!
//! ```
//! use scopekit::{Config, Mode, Protocol};
//!
//! let args = ["viewer", "--window", "study.zip", "--protocol", "kitty", "-v"];
//! let (config, rest) = Config::default().with_args(args.map(String::from)).unwrap();
//! assert_eq!(config.mode, Mode::Window);
//! assert_eq!(config.protocol, Protocol::Kitty);
//! assert_eq!(rest, ["study.zip", "-v"]);
//! ```

use crate::config::{Backend, Config, Mode, Protocol};
use std::path::PathBuf;

fn value<T: Copy>(flag: &str, v: &str, options: &[(&str, T)]) -> Result<T, String> {
    options
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(v))
        .map(|(_, t)| *t)
        .ok_or_else(|| {
            let names: Vec<&str> = options.iter().map(|(n, _)| *n).collect();
            format!("{flag} {v:?}: expected one of {}", names.join(", "))
        })
}

impl Config {
    /// Apply the common flags from `args` (the first item is the program
    /// name, as in `std::env::args()`), and return the configuration with
    /// the arguments it did not use.
    pub fn with_args(
        self,
        args: impl IntoIterator<Item = String>,
    ) -> Result<(Config, Vec<String>), String> {
        let mut c = self;
        if let Ok(m) = std::env::var("SCOPEKIT_MODE") {
            c.mode = value(
                "SCOPEKIT_MODE",
                &m,
                &[("terminal", Mode::Terminal), ("window", Mode::Window)],
            )?;
        }
        let mut rest = Vec::new();
        let mut it = args.into_iter().skip(1);
        while let Some(a) = it.next() {
            // `--flag=value` and `--flag value` both work.
            let (flag, inline) = match a.split_once('=') {
                Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
                _ => (a.clone(), None),
            };
            let mut arg = |name: &str| -> Result<String, String> {
                inline
                    .clone()
                    .or_else(|| it.next())
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            match flag.as_str() {
                "--window" | "-w" => c.mode = Mode::Window,
                "--terminal" => c.mode = Mode::Terminal,
                "--no-mouse" => c.mouse = false,
                "--no-gestures" => c.input.gestures = false,
                "--backend" => {
                    c.backend = value(
                        "--backend",
                        &arg("--backend")?,
                        &[
                            ("auto", Backend::Auto),
                            ("metal", Backend::Metal),
                            ("vulkan", Backend::Vulkan),
                            ("dx12", Backend::Dx12),
                            ("gl", Backend::Gl),
                        ],
                    )?
                }
                "--protocol" => {
                    c.protocol = value(
                        "--protocol",
                        &arg("--protocol")?,
                        &[
                            ("auto", Protocol::Auto),
                            ("kitty", Protocol::Kitty),
                            ("iterm2", Protocol::Iterm2),
                            ("sixel", Protocol::Sixel),
                            ("halfblocks", Protocol::Halfblocks),
                        ],
                    )?
                }
                "--font" => c.font = Some(PathBuf::from(arg("--font")?)),
                "--font-size" => {
                    let v = arg("--font-size")?;
                    c.font_size =
                        v.parse().ok().filter(|s: &f64| *s >= 4.0).ok_or_else(|| {
                            format!("--font-size {v:?}: expected a number of pixels")
                        })?;
                }
                "--title" => c.title = arg("--title")?,
                "--copy-key" => {
                    let v = arg("--copy-key")?;
                    let mut chars = v.chars();
                    c.copy_key = match (chars.next(), chars.next()) {
                        (Some(k), None) => Some(k),
                        _ => return Err(format!("--copy-key {v:?}: expected one character")),
                    };
                }
                "--switch-key" => {
                    let v = arg("--switch-key")?;
                    let mut chars = v.chars();
                    c.switch_key = match (chars.next(), chars.next()) {
                        (Some(k), None) => Some(k),
                        _ => return Err(format!("--switch-key {v:?}: expected one character")),
                    };
                }
                #[cfg(feature = "toml")]
                "--config" => {
                    // A file sets the base; flags already given stay on top.
                    let file = Config::load(std::path::Path::new(&arg("--config")?))?;
                    c = Config {
                        mode: c.mode,
                        ..file
                    };
                }
                _ => rest.push(a),
            }
        }
        Ok((c, rest))
    }

    /// [`Config::with_args`] on this process's command line: the usual
    /// one-liner in `main`.
    pub fn from_args() -> Result<(Config, Vec<String>), String> {
        Config::default().with_args(std::env::args())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<(Config, Vec<String>), String> {
        let v = std::iter::once("app")
            .chain(args.iter().copied())
            .map(String::from);
        Config::default().with_args(v)
    }

    #[test]
    fn flags_values_and_leftovers() {
        let (c, rest) = run(&[
            "-w",
            "--backend=metal",
            "--font-size",
            "13",
            "x",
            "--title",
            "t",
        ])
        .unwrap();
        assert_eq!(
            (c.mode, c.backend, c.font_size, c.title.as_str()),
            (Mode::Window, Backend::Metal, 13.0, "t")
        );
        assert_eq!(rest, ["x"]);
        assert!(run(&["--protocol", "vt100"]).unwrap_err().contains("kitty"));
        assert!(run(&["--backend"]).unwrap_err().contains("needs a value"));
        assert!(!run(&["--no-mouse"]).unwrap().0.mouse);
        assert_eq!(run(&["--switch-key", "p"]).unwrap().0.switch_key, Some('p'));
        assert!(run(&["--switch-key", "pq"])
            .unwrap_err()
            .contains("one character"));
    }
}
