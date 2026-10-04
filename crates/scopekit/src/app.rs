//! What an application implements: draw the text UI, say where its GPU
//! views go, react to events.

use crate::config::{Config, Mode};
use crate::waker::Waker;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Frame;
use std::time::Duration;

/// What to do after an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Keep running (the screen is redrawn).
    Continue,
    /// Stop: the terminal is restored, the window closed.
    Quit,
}

/// Where the GPU views go this frame, and facts about the screen an app
/// needs to map the mouse into a view.
#[derive(Debug, Clone, Default)]
pub struct ViewSlot {
    placed: Vec<(String, Rect)>,
    overlays: Vec<Rect>,
    cell_px: (f32, f32),
    describe: String,
}

impl ViewSlot {
    /// A slot for a frame. scopekit makes these; custom drivers and tests
    /// can too.
    pub fn new(cell_px: (f32, f32), describe: String) -> ViewSlot {
        ViewSlot {
            placed: Vec::new(),
            overlays: Vec::new(),
            cell_px,
            describe,
        }
    }

    /// Show the view registered as `name` in `area` (in cells) this frame.
    /// Views not placed are hidden: do that while a popup covers one,
    /// because in a window views are drawn on top of the text. Placing the
    /// same name twice moves it.
    pub fn place(&mut self, name: &str, area: Rect) {
        self.placed.retain(|(n, _)| n != name);
        if area.width > 0 && area.height > 0 {
            self.placed.push((name.to_string(), area));
        }
    }

    /// Where view `name` was placed this frame, if anywhere.
    pub fn rect(&self, name: &str) -> Option<Rect> {
        self.placed.iter().find(|(n, _)| n == name).map(|(_, r)| *r)
    }

    /// Every placement this frame, in the order they were made.
    pub fn placed(&self) -> &[(String, Rect)] {
        &self.placed
    }

    /// Show the text drawn in `area` (in cells) on top of the GPU views
    /// this frame: labels, markers, a crosshair over a globe or an image.
    /// Draw the text into the frame as usual and mark its cells here.
    ///
    /// In a window the cells are drawn again after the views; in a
    /// terminal with half blocks they are written over the image. With the
    /// kitty, iTerm2 or sixel protocols the terminal draws the image above
    /// all text, so the overlay stays hidden there.
    pub fn overlay(&mut self, area: Rect) {
        if area.width > 0 && area.height > 0 {
            self.overlays.push(area);
        }
    }

    /// The overlay areas marked this frame.
    pub fn overlays(&self) -> &[Rect] {
        &self.overlays
    }

    /// Hide every view and overlay this frame (the help box covers them).
    pub(crate) fn clear_views(&mut self) {
        self.placed.clear();
        self.overlays.clear();
    }

    /// Screen pixels per text cell (width, height). In a terminal this is
    /// the font size the terminal reports; in a window, the window's pixel
    /// size divided by the grid.
    pub fn cell_px(&self) -> (f32, f32) {
        self.cell_px
    }

    /// A view's size in pixels for `area`, as it will be rendered.
    pub fn px_size(&self, area: Rect) -> (u32, u32) {
        (
            (f32::from(area.width) * self.cell_px.0).round() as u32,
            (f32::from(area.height) * self.cell_px.1).round() as u32,
        )
    }

    /// `"Metal · Apple M2 Max → kitty"`: GPU and how views are shown, for
    /// a status line. `no GPU (…)` when none could be opened; empty until
    /// the GPU is first used.
    pub fn describe(&self) -> &str {
        &self.describe
    }
}

/// A scopekit application.
pub trait App {
    /// Called once before the first frame, with a [`Waker`] that
    /// background threads (an emulator's serial port, a build) can use to
    /// request a redraw at once instead of at the next input or tick.
    fn start(&mut self, waker: Waker) {
        let _ = waker;
    }

    /// Draw one frame of the text UI with ratatui, and place GPU views
    /// with [`ViewSlot::place`].
    fn draw(&mut self, frame: &mut Frame, views: &mut ViewSlot);

    /// A key, mouse, paste or resize event. The window mode translates its
    /// input into the same crossterm types, so one handler serves both.
    fn event(&mut self, event: Event) -> Flow;

    /// Called between events. Return `true` when something changed and
    /// the screen should be redrawn (an animation step, cine playback).
    fn tick(&mut self) -> bool {
        false
    }

    /// How often to call [`tick`](App::tick) while it has work; `None`
    /// waits for events and wake-ups only.
    fn tick_interval(&self) -> Option<Duration> {
        None
    }

    /// `true` while a text field has the keyboard, so
    /// [`Config::switch_key`] reaches the app as a typed character instead
    /// of switching between terminal and window.
    fn captures_text(&self) -> bool {
        false
    }

    /// A gesture on one of the app's GPU views (tap, pan, zoom, rotate …),
    /// recognised from mouse, trackpad and touch input with the bindings in
    /// [`Config::input`]. The raw events still arrive in
    /// [`event`](App::event) too; handle a view's input in one of the two.
    fn gesture(&mut self, gesture: crate::gesture::Gesture) -> Flow {
        let _ = gesture;
        Flow::Continue
    }

    /// A short notice from scopekit for a status line: "copied the view
    /// (1400 × 900) to the clipboard", or why copying failed.
    fn message(&mut self, text: &str) {
        let _ = text;
    }

    /// The app's keys and what gestures do, for the built-in help box
    /// ([`Config::help_key`], `?` by default). `None`, the default, leaves
    /// that key to the app.
    fn help(&self) -> Option<crate::help::Help> {
        None
    }

    /// Window mode: whether the app wants pictures of the whole window (text
    /// and views, as shown) in [`mirrored`](App::mirrored), e.g. to show
    /// them somewhere else. Asked before each frame.
    fn mirror(&mut self) -> Mirror {
        Mirror::Off
    }

    /// A picture of the window as it was drawn (RGBA rows, `width` x
    /// `height` window pixels), when [`mirror`](App::mirror) asked for it.
    fn mirrored(&mut self, rgba: Vec<u8>, width: u32, height: u32) {
        let _ = (rgba, width, height);
    }

    /// Window mode: a size (logical pixels) the window should have, asked
    /// before each frame; the window is resized when this changes.
    fn window_size(&mut self) -> Option<(u32, u32)> {
        None
    }

    /// Called after the app moved to `mode` with [`Config::switch_key`],
    /// before its first frame there. [`start`](App::start) runs again too,
    /// with the new mode's [`Waker`], and GPU views are prepared again for
    /// the new target.
    fn mode_changed(&mut self, mode: Mode) {
        let _ = mode;
    }
}

/// What [`App::mirror`] asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mirror {
    /// No pictures.
    Off,
    /// A picture of each frame that changed the window.
    Changes,
    /// A picture of the next frame, even if nothing changed.
    Now,
}

/// Why a mode's loop returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    /// The app quit (or its only window closed).
    Quit,
    /// The switch key was pressed: continue in the other mode.
    Switch,
}

/// Whether `key` opens the built-in help for this app.
pub(crate) fn is_help_key(config: &Config, app: &dyn App, key: &KeyEvent) -> bool {
    let Some(want) = config.help_key else {
        return false;
    };
    let plain = (key.modifiers - KeyModifiers::SHIFT).is_empty();
    plain && key.code == KeyCode::Char(want) && !app.captures_text() && app.help().is_some()
}

/// Whether `key` is the configured copy key.
pub(crate) fn is_copy_key(config: &Config, app: &dyn App, key: &KeyEvent) -> bool {
    let Some(want) = config.copy_key.filter(|_| cfg!(feature = "clipboard")) else {
        return false;
    };
    let plain = (key.modifiers - KeyModifiers::SHIFT).is_empty();
    plain && key.code == KeyCode::Char(want) && !app.captures_text()
}

/// Whether `key` is the configured switch key and the app lets it switch.
/// Letters match in either case: `p` and `P`.
pub(crate) fn is_switch_key(config: &Config, app: &dyn App, key: &KeyEvent) -> bool {
    // Switching needs both modes.
    let Some(want) = config
        .switch_key
        .filter(|_| cfg!(all(feature = "terminal", feature = "window")))
    else {
        return false;
    };
    let plain = (key.modifiers - KeyModifiers::SHIFT).is_empty();
    let matches = match key.code {
        KeyCode::Char(c) => c == want || c.eq_ignore_ascii_case(&want),
        _ => false,
    };
    plain && matches && !app.captures_text()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Typing(bool);

    impl App for Typing {
        fn draw(&mut self, _: &mut Frame, _: &mut ViewSlot) {}
        fn event(&mut self, _: Event) -> Flow {
            Flow::Continue
        }
        fn captures_text(&self) -> bool {
            self.0
        }
    }

    #[test]
    fn switch_key_rules() {
        let key = |c, m| KeyEvent::new(KeyCode::Char(c), m);
        let off = Config::default();
        assert!(!is_switch_key(
            &off,
            &Typing(false),
            &key('p', KeyModifiers::NONE)
        ));

        let on = Config {
            switch_key: Some('p'),
            ..Config::default()
        };
        let both = cfg!(all(feature = "terminal", feature = "window"));
        assert_eq!(
            is_switch_key(&on, &Typing(false), &key('p', KeyModifiers::NONE)),
            both
        );
        assert_eq!(
            is_switch_key(&on, &Typing(false), &key('p', KeyModifiers::SHIFT)),
            both
        );
        assert!(!is_switch_key(
            &on,
            &Typing(false),
            &key('p', KeyModifiers::CONTROL)
        ));
        assert!(!is_switch_key(
            &on,
            &Typing(false),
            &key('q', KeyModifiers::NONE)
        ));
        // Either case switches.
        assert_eq!(
            is_switch_key(&on, &Typing(false), &key('P', KeyModifiers::SHIFT)),
            both
        );
        // A text field keeps the key.
        assert!(!is_switch_key(
            &on,
            &Typing(true),
            &key('p', KeyModifiers::NONE)
        ));
    }
}
