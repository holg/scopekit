//! The built-in help box (`?`): the app's keys, scopekit's own keys, and
//! what the mouse, trackpad and touch do, generated from the active
//! [`Input`] bindings so it always matches them.

use crate::config::Config;
use crate::gesture::{GestureType, Input};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
use ratatui::Frame;

/// What an app contributes to the help box: its keys, and what each
/// gesture does in its views. Return it from [`App::help`](crate::App::help).
///
/// ```
/// use scopekit::{GestureType, Help};
///
/// let help = Help::new("geodb globe")
///     .key("/", "search a city")
///     .key("1 2 3", "tabs")
///     .gesture(GestureType::Pan, "rotate the globe")
///     .gesture(GestureType::Tap, "fly to the spot");
/// assert_eq!(help.keys.len(), 2);
/// ```
#[derive(Debug, Clone, Default)]
pub struct Help {
    /// Shown in the box's title.
    pub title: String,
    /// (keys, what they do), in order.
    pub keys: Vec<(String, String)>,
    /// What each gesture does; gestures left out are not listed.
    pub gestures: Vec<(GestureType, String)>,
}

impl Help {
    /// Empty help with a title.
    pub fn new(title: impl Into<String>) -> Help {
        Help {
            title: title.into(),
            ..Help::default()
        }
    }

    /// Add a key row.
    pub fn key(mut self, keys: impl Into<String>, what: impl Into<String>) -> Help {
        self.keys.push((keys.into(), what.into()));
        self
    }

    /// Say what gesture `t` does in the app's views.
    pub fn gesture(mut self, t: GestureType, what: impl Into<String>) -> Help {
        self.gestures.push((t, what.into()));
        self
    }

    /// The rows of the box: (section or key, description).
    pub(crate) fn rows(&self, config: &Config) -> Vec<(String, String)> {
        let mut rows: Vec<(String, String)> = self.keys.clone();
        if let Some(k) = config.switch_key {
            let keys = if k.is_ascii_alphabetic() {
                format!("{}  {}", k.to_ascii_lowercase(), k.to_ascii_uppercase())
            } else {
                k.to_string()
            };
            rows.push((keys, "move between terminal and window".into()));
        }
        if let Some(k) = config.copy_key.filter(|_| cfg!(feature = "clipboard")) {
            rows.push((
                k.to_string(),
                "copy the view under the pointer as an image".into(),
            ));
        }
        rows.push((
            "Shift + drag".into(),
            "select text (window: then Cmd-C or Ctrl-Shift-C copies it)".into(),
        ));
        if let Some(k) = config.help_key {
            rows.push((
                format!("{k}  Esc"),
                "this help (any key or click closes it)".into(),
            ));
        }
        rows.push(("Cmd-Q".into(), "quit (window)".into()));
        rows
    }

    /// Mouse, trackpad and touch rows from the bindings.
    pub(crate) fn gesture_rows(&self, input: &Input) -> Vec<(String, String)> {
        self.gestures
            .iter()
            .filter_map(|(t, what)| {
                let inputs = input.inputs_for(*t);
                (!inputs.is_empty()).then(|| (inputs.join(", "), what.clone()))
            })
            .collect()
    }
}

fn centred(r: Rect, w: u16, h: u16) -> Rect {
    let (w, h) = (w.min(r.width), h.min(r.height));
    Rect::new(r.x + (r.width - w) / 2, r.y + (r.height - h) / 2, w, h)
}

/// Draw the box over `area`.
pub(crate) fn draw(f: &mut Frame, area: Rect, help: &Help, config: &Config) {
    let accent = Style::new()
        .fg(ratatui::style::Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let keys = help.rows(config);
    let mouse = help.gesture_rows(&config.input);
    // The key column fits the keys; long input lists wrap at their commas.
    let width = keys
        .iter()
        .chain(mouse.iter())
        .flat_map(|(k, _)| k.split(", ").map(|p| p.chars().count()))
        .max()
        .unwrap_or(10)
        .clamp(10, 30);
    let row = |(k, what): &(String, String)| -> Vec<Line> {
        let mut parts: Vec<String> = Vec::new();
        for p in k.split(", ") {
            match parts.last_mut() {
                Some(last) if last.chars().count() + 2 + p.chars().count() <= width => {
                    last.push_str(", ");
                    last.push_str(p);
                }
                _ => parts.push(p.to_string()),
            }
        }
        let last = parts.len().saturating_sub(1);
        parts
            .iter()
            .enumerate()
            .map(|(i, part)| {
                let part = if i < last {
                    format!("{part},")
                } else {
                    part.clone()
                };
                let what = if i == 0 { what.clone() } else { String::new() };
                Line::from(vec![
                    Span::styled(format!(" {part:<width$}  "), accent),
                    Span::raw(what),
                ])
            })
            .collect()
    };
    let mut lines: Vec<Line> = vec![Line::from(" Keys").bold()];
    lines.extend(keys.iter().flat_map(row));
    if !mouse.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(" Mouse, trackpad, touch").bold());
        lines.extend(mouse.iter().flat_map(row));
    }
    lines.push(Line::from(""));
    lines.push(
        Line::from(" Bindings: the [input] section of a config file (--config), or --no-gestures.")
            .dim(),
    );
    let h = lines.len() as u16 + 2;
    let box_area = centred(area, 96, h);
    let title = if help.title.is_empty() {
        " Keys and mouse ".to_string()
    } else {
        format!(" {}: keys and mouse ", help.title)
    };
    f.render_widget(Clear, box_area);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .title(title),
        ),
        box_area,
    );
}
