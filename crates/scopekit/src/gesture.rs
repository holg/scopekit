//! Mouse, trackpad and touch input as gestures on GPU views: tap, double
//! tap, long press, pan, zoom and rotate, with bindings in [`Input`].
//!
//! Gestures are extra: the app still gets every raw event in
//! [`App::event`](crate::App::event), and gets gestures over its views in
//! [`App::gesture`](crate::App::gesture) as well.

use crate::gpu::PixelRect;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// A gesture on one GPU view.
#[derive(Debug, Clone, PartialEq)]
pub struct Gesture {
    /// The view it happened on, as registered in [`Views`](crate::Views).
    pub view: String,
    /// The view's size in pixels.
    pub size: (f32, f32),
    /// Where: the pointer, or the centre of the fingers, in view pixels
    /// from the top-left corner. Exact in a window; the centre of the cell
    /// in a terminal.
    pub pos: (f32, f32),
    /// What happened.
    pub kind: GestureKind,
}

/// What a [`Gesture`] does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GestureKind {
    /// Click or tap; `count` is 2 for the second click of a double click
    /// (the first arrives as a single tap before it).
    Tap {
        /// 1, or 2 for a double click / tap.
        count: u8,
    },
    /// Held without moving, or a right click: the context action.
    LongPress,
    /// The content moves by (dx, dy) pixels: drag, scroll, two fingers.
    Pan {
        /// Pixels right.
        dx: f32,
        /// Pixels down.
        dy: f32,
    },
    /// Scale by `factor` about `pos`; above 1 zooms in.
    Zoom {
        /// Scale factor.
        factor: f32,
    },
    /// Turn by `radians` about `pos`, counter-clockwise positive.
    Rotate {
        /// Angle in radians.
        radians: f32,
    },
}

/// The kinds of gesture without their data, for bindings and help.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GestureType {
    /// [`GestureKind::Tap`] with `count` 1.
    Tap,
    /// [`GestureKind::Tap`] with `count` 2.
    DoubleTap,
    /// [`GestureKind::LongPress`].
    LongPress,
    /// [`GestureKind::Pan`].
    Pan,
    /// [`GestureKind::Zoom`].
    Zoom,
    /// [`GestureKind::Rotate`].
    Rotate,
}

/// What a drag does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragAction {
    /// [`GestureKind::Pan`] by the pointer's movement.
    Pan,
    /// [`GestureKind::Rotate`] about the view's centre.
    Rotate,
    /// [`GestureKind::Zoom`]: up zooms in, down zooms out.
    Zoom,
    /// Nothing (the raw events still arrive).
    None,
}

/// What scrolling does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollAction {
    /// [`GestureKind::Zoom`]: away from you / up zooms in.
    Zoom,
    /// [`GestureKind::Pan`] by the scrolled distance.
    Pan,
    /// Nothing (the raw events still arrive).
    None,
}

/// Gesture bindings. The defaults suit maps, images and 3D views; override
/// them in code, in the `[input]` section of a config file, or turn
/// gestures off with `--no-gestures`.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    /// Recognise gestures at all.
    pub gestures: bool,
    /// Left mouse button drag.
    pub left_drag: DragAction,
    /// Right mouse button drag.
    pub right_drag: DragAction,
    /// Middle mouse button drag.
    pub middle_drag: DragAction,
    /// One finger on a touch screen.
    pub one_finger: DragAction,
    /// A mouse wheel (steps).
    pub wheel: ScrollAction,
    /// Two-finger scrolling on a trackpad (pixel deltas; window mode).
    pub trackpad_scroll: ScrollAction,
    /// The wheel or trackpad with Ctrl, Option/Alt or Cmd held.
    pub modifier_scroll: ScrollAction,
    /// Zoom factor per wheel step.
    pub zoom_step: f32,
    /// Longest gap between the two clicks of a double click.
    pub double_tap: Duration,
    /// How long a press must last to be a long press.
    pub long_press: Duration,
    /// Movement in pixels before a press becomes a drag.
    pub slop: f32,
}

impl Default for Input {
    fn default() -> Self {
        Input {
            gestures: true,
            left_drag: DragAction::Pan,
            right_drag: DragAction::Rotate,
            middle_drag: DragAction::Pan,
            one_finger: DragAction::Pan,
            wheel: ScrollAction::Zoom,
            trackpad_scroll: ScrollAction::Pan,
            modifier_scroll: ScrollAction::Zoom,
            zoom_step: 1.2,
            double_tap: Duration::from_millis(350),
            long_press: Duration::from_millis(600),
            slop: 6.0,
        }
    }
}

impl Input {
    /// The inputs that produce `t` under these bindings, for help screens:
    /// `["left drag", "two-finger scroll", …]`.
    pub fn inputs_for(&self, t: GestureType) -> Vec<&'static str> {
        let mut v = Vec::new();
        if !self.gestures {
            return v;
        }
        let drag = |a: DragAction| {
            matches!(
                (t, a),
                (GestureType::Pan, DragAction::Pan)
                    | (GestureType::Zoom, DragAction::Zoom)
                    | (GestureType::Rotate, DragAction::Rotate)
            )
        };
        let scroll = |a: ScrollAction| {
            matches!(
                (t, a),
                (GestureType::Pan, ScrollAction::Pan) | (GestureType::Zoom, ScrollAction::Zoom)
            )
        };
        match t {
            GestureType::Tap => v.push("click, tap"),
            GestureType::DoubleTap => v.push("double click, double tap"),
            GestureType::LongPress => v.push("right click, long press"),
            _ => {}
        }
        for (a, name) in [
            (self.left_drag, "left drag"),
            (self.right_drag, "right drag"),
            (self.middle_drag, "middle drag"),
            (self.one_finger, "one-finger drag"),
        ] {
            if drag(a) {
                v.push(name);
            }
        }
        for (a, name) in [
            (self.wheel, "wheel"),
            (self.trackpad_scroll, "two-finger scroll"),
            (self.modifier_scroll, "Ctrl/Option/Cmd + wheel"),
        ] {
            if scroll(a) {
                v.push(name);
            }
        }
        match t {
            GestureType::Pan => v.push("two-finger drag (touch)"),
            GestureType::Zoom => v.push("pinch"),
            GestureType::Rotate => v.push("two-finger twist"),
            _ => {}
        }
        v
    }
}

/// A mouse button, or a finger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Button {
    Left,
    Right,
    Middle,
    Touch,
}

/// Raw pointer input in target pixels, from either mode.
#[derive(Debug, Clone, Copy)]
// Only the window sees trackpad gestures (pinch, twist, double tap).
#[cfg_attr(not(feature = "window"), allow(dead_code))]
pub(crate) enum Pointer {
    Down {
        id: u64,
        button: Button,
        pos: (f32, f32),
    },
    Move {
        id: u64,
        pos: (f32, f32),
    },
    Up {
        id: u64,
        pos: (f32, f32),
    },
    /// Wheel steps (`precise` false) or trackpad pixels; +y is up / away.
    Scroll {
        delta: (f32, f32),
        precise: bool,
        modifier: bool,
        pos: (f32, f32),
    },
    /// A trackpad pinch: scale by `factor`.
    Pinch {
        factor: f32,
        pos: (f32, f32),
    },
    /// A trackpad twist.
    Twist {
        radians: f32,
        pos: (f32, f32),
    },
    /// The trackpad's own double tap (macOS smart zoom).
    DoubleTap {
        pos: (f32, f32),
    },
}

struct Track {
    button: Button,
    view: (String, PixelRect),
    start: (f32, f32),
    last: (f32, f32),
    pressed: Instant,
    moved: bool,
}

struct LastTap {
    at: Instant,
    pos: (f32, f32),
    view: String,
}

/// Two fingers on one view: their centre, distance and angle last time.
struct Pair {
    ids: (u64, u64),
    centre: (f32, f32),
    dist: f32,
    angle: f32,
}

/// Turns [`Pointer`] input into gestures. One per window or terminal.
#[derive(Default)]
pub(crate) struct Recognizer {
    tracks: HashMap<u64, Track>,
    last_tap: Option<LastTap>,
    pair: Option<Pair>,
}

fn hit(views: &[(String, PixelRect)], (x, y): (f32, f32)) -> Option<(String, PixelRect)> {
    // Later placements are drawn on top.
    views
        .iter()
        .rev()
        .find(|(_, r)| {
            x >= r.x as f32
                && y >= r.y as f32
                && x < (r.x + r.width) as f32
                && y < (r.y + r.height) as f32
        })
        .cloned()
}

fn local(r: &PixelRect, (x, y): (f32, f32)) -> (f32, f32) {
    (x - r.x as f32, y - r.y as f32)
}

fn size(r: &PixelRect) -> (f32, f32) {
    (r.width as f32, r.height as f32)
}

fn gesture(view: &(String, PixelRect), pos: (f32, f32), kind: GestureKind) -> Gesture {
    Gesture {
        view: view.0.clone(),
        size: size(&view.1),
        pos: local(&view.1, pos),
        kind,
    }
}

impl Recognizer {
    /// Feed one input. `views` are the placed views in target pixels.
    pub fn feed(
        &mut self,
        input: &Input,
        p: Pointer,
        views: &[(String, PixelRect)],
        now: Instant,
    ) -> Vec<Gesture> {
        if !input.gestures {
            return Vec::new();
        }
        let mut out = Vec::new();
        match p {
            Pointer::Down { id, button, pos } => {
                let Some(view) = hit(views, pos) else {
                    return out;
                };
                self.tracks.insert(
                    id,
                    Track {
                        button,
                        view,
                        start: pos,
                        last: pos,
                        pressed: now,
                        moved: false,
                    },
                );
                self.start_pair();
            }
            Pointer::Move { id, pos } => {
                if self
                    .pair
                    .as_ref()
                    .is_some_and(|p| p.ids.0 == id || p.ids.1 == id)
                {
                    if let Some(t) = self.tracks.get_mut(&id) {
                        t.last = pos;
                    }
                    self.move_pair(&mut out);
                    return out;
                }
                let Some(t) = self.tracks.get_mut(&id) else {
                    return out;
                };
                if !t.moved {
                    let (dx, dy) = (pos.0 - t.start.0, pos.1 - t.start.1);
                    if dx.hypot(dy) < input.slop {
                        return out;
                    }
                    t.moved = true;
                }
                let action = match t.button {
                    Button::Left => input.left_drag,
                    Button::Right => input.right_drag,
                    Button::Middle => input.middle_drag,
                    Button::Touch => input.one_finger,
                };
                let (dx, dy) = (pos.0 - t.last.0, pos.1 - t.last.1);
                let r = &t.view.1;
                match action {
                    DragAction::Pan => out.push(gesture(&t.view, pos, GestureKind::Pan { dx, dy })),
                    DragAction::Zoom => out.push(gesture(
                        &t.view,
                        t.start,
                        GestureKind::Zoom {
                            factor: (-dy * 0.01).exp(),
                        },
                    )),
                    DragAction::Rotate => {
                        // The angle swept about the view's centre.
                        let c = (
                            r.x as f32 + r.width as f32 / 2.0,
                            r.y as f32 + r.height as f32 / 2.0,
                        );
                        let a0 = (t.last.1 - c.1).atan2(t.last.0 - c.0);
                        let a1 = (pos.1 - c.1).atan2(pos.0 - c.0);
                        let mut d = a1 - a0;
                        if d > std::f32::consts::PI {
                            d -= std::f32::consts::TAU;
                        } else if d < -std::f32::consts::PI {
                            d += std::f32::consts::TAU;
                        }
                        // Screen y points down: flip for counter-clockwise.
                        out.push(gesture(&t.view, c, GestureKind::Rotate { radians: -d }));
                    }
                    DragAction::None => {}
                }
                t.last = pos;
            }
            Pointer::Up { id, pos } => {
                let in_pair = self
                    .pair
                    .as_ref()
                    .is_some_and(|p| p.ids.0 == id || p.ids.1 == id);
                let Some(t) = self.tracks.remove(&id) else {
                    return out;
                };
                if in_pair {
                    // Lifting a finger of a pinch ends it; no tap.
                    self.pair = None;
                    // The finger left down continues as a drag, not a tap.
                    for other in self.tracks.values_mut() {
                        other.moved = true;
                    }
                    return out;
                }
                if t.moved {
                    return out;
                }
                if t.button == Button::Right || now - t.pressed >= input.long_press {
                    out.push(gesture(&t.view, pos, GestureKind::LongPress));
                    return out;
                }
                let double = self.last_tap.take().is_some_and(|l| {
                    now - l.at <= input.double_tap
                        && l.view == t.view.0
                        && (l.pos.0 - pos.0).hypot(l.pos.1 - pos.1) <= input.slop * 3.0
                });
                if double {
                    out.push(gesture(&t.view, pos, GestureKind::Tap { count: 2 }));
                } else {
                    out.push(gesture(&t.view, pos, GestureKind::Tap { count: 1 }));
                    self.last_tap = Some(LastTap {
                        at: now,
                        pos,
                        view: t.view.0.clone(),
                    });
                }
            }
            Pointer::Scroll {
                delta,
                precise,
                modifier,
                pos,
            } => {
                let Some(view) = hit(views, pos) else {
                    return out;
                };
                let action = if modifier {
                    input.modifier_scroll
                } else if precise {
                    input.trackpad_scroll
                } else {
                    input.wheel
                };
                match action {
                    ScrollAction::Zoom => {
                        let factor = if precise {
                            (delta.1 * 0.01).exp()
                        } else {
                            input.zoom_step.powf(delta.1)
                        };
                        out.push(gesture(&view, pos, GestureKind::Zoom { factor }));
                    }
                    ScrollAction::Pan => {
                        let k = if precise { 1.0 } else { 40.0 };
                        out.push(gesture(
                            &view,
                            pos,
                            GestureKind::Pan {
                                dx: delta.0 * k,
                                dy: delta.1 * k,
                            },
                        ));
                    }
                    ScrollAction::None => {}
                }
            }
            Pointer::Pinch { factor, pos } => {
                if let Some(view) = hit(views, pos) {
                    out.push(gesture(&view, pos, GestureKind::Zoom { factor }));
                }
            }
            Pointer::Twist { radians, pos } => {
                if let Some(view) = hit(views, pos) {
                    out.push(gesture(&view, pos, GestureKind::Rotate { radians }));
                }
            }
            Pointer::DoubleTap { pos } => {
                if let Some(view) = hit(views, pos) {
                    out.push(gesture(&view, pos, GestureKind::Tap { count: 2 }));
                }
            }
        }
        out
    }

    /// Two fingers down on the same view start a pinch.
    fn start_pair(&mut self) {
        let fingers: Vec<(&u64, &Track)> = self
            .tracks
            .iter()
            .filter(|(_, t)| t.button == Button::Touch)
            .collect();
        if fingers.len() != 2 || fingers[0].1.view.0 != fingers[1].1.view.0 {
            return;
        }
        let (a, b) = (fingers[0].1.last, fingers[1].1.last);
        self.pair = Some(Pair {
            ids: (*fingers[0].0, *fingers[1].0),
            centre: ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0),
            dist: (a.0 - b.0).hypot(a.1 - b.1).max(1.0),
            angle: (b.1 - a.1).atan2(b.0 - a.0),
        });
        for t in self.tracks.values_mut() {
            t.moved = true;
        }
    }

    fn move_pair(&mut self, out: &mut Vec<Gesture>) {
        let Some(pair) = self.pair.as_mut() else {
            return;
        };
        let (Some(ta), Some(tb)) = (self.tracks.get(&pair.ids.0), self.tracks.get(&pair.ids.1))
        else {
            return;
        };
        let (a, b) = (ta.last, tb.last);
        let view = ta.view.clone();
        let centre = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
        let dist = (a.0 - b.0).hypot(a.1 - b.1).max(1.0);
        let angle = (b.1 - a.1).atan2(b.0 - a.0);
        let factor = dist / pair.dist;
        if (factor - 1.0).abs() > 1e-3 {
            out.push(gesture(&view, centre, GestureKind::Zoom { factor }));
        }
        let mut turn = angle - pair.angle;
        if turn > std::f32::consts::PI {
            turn -= std::f32::consts::TAU;
        } else if turn < -std::f32::consts::PI {
            turn += std::f32::consts::TAU;
        }
        if turn.abs() > 1e-3 {
            out.push(gesture(
                &view,
                centre,
                GestureKind::Rotate { radians: -turn },
            ));
        }
        let (dx, dy) = (centre.0 - pair.centre.0, centre.1 - pair.centre.1);
        if dx != 0.0 || dy != 0.0 {
            out.push(gesture(&view, centre, GestureKind::Pan { dx, dy }));
        }
        *pair = Pair {
            ids: pair.ids,
            centre,
            dist,
            angle,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn views() -> Vec<(String, PixelRect)> {
        vec![(
            "globe".into(),
            PixelRect {
                x: 100,
                y: 50,
                width: 400,
                height: 300,
            },
        )]
    }

    fn kinds(g: &[Gesture]) -> Vec<GestureKind> {
        g.iter().map(|g| g.kind).collect()
    }

    #[test]
    fn click_is_a_tap_in_view_pixels_and_outside_is_ignored() {
        let (mut r, input, v, t0) = (
            Recognizer::default(),
            Input::default(),
            views(),
            Instant::now(),
        );
        let down = Pointer::Down {
            id: 1,
            button: Button::Left,
            pos: (150.0, 80.0),
        };
        assert!(r.feed(&input, down, &v, t0).is_empty());
        let g = r.feed(
            &input,
            Pointer::Up {
                id: 1,
                pos: (151.0, 80.0),
            },
            &v,
            t0,
        );
        assert_eq!(kinds(&g), [GestureKind::Tap { count: 1 }]);
        assert_eq!(
            (g[0].pos, g[0].size, g[0].view.as_str()),
            ((51.0, 30.0), (400.0, 300.0), "globe")
        );

        let outside = Pointer::Down {
            id: 2,
            button: Button::Left,
            pos: (10.0, 10.0),
        };
        assert!(r.feed(&input, outside, &v, t0).is_empty());
        assert!(r
            .feed(
                &input,
                Pointer::Up {
                    id: 2,
                    pos: (10.0, 10.0)
                },
                &v,
                t0
            )
            .is_empty());
    }

    #[test]
    fn second_quick_click_is_a_double_tap_and_a_slow_one_is_not() {
        let (mut r, input, v, t0) = (
            Recognizer::default(),
            Input::default(),
            views(),
            Instant::now(),
        );
        let click = |r: &mut Recognizer, at: Instant| {
            r.feed(
                &input,
                Pointer::Down {
                    id: 1,
                    button: Button::Left,
                    pos: (200.0, 100.0),
                },
                &v,
                at,
            );
            r.feed(
                &input,
                Pointer::Up {
                    id: 1,
                    pos: (200.0, 100.0),
                },
                &v,
                at,
            )
        };
        assert_eq!(kinds(&click(&mut r, t0)), [GestureKind::Tap { count: 1 }]);
        let t1 = t0 + Duration::from_millis(200);
        assert_eq!(kinds(&click(&mut r, t1)), [GestureKind::Tap { count: 2 }]);
        let t2 = t1 + Duration::from_secs(2);
        assert_eq!(kinds(&click(&mut r, t2)), [GestureKind::Tap { count: 1 }]);
    }

    #[test]
    fn drag_pans_after_the_slop_and_right_drag_rotates() {
        let (mut r, input, v, t0) = (
            Recognizer::default(),
            Input::default(),
            views(),
            Instant::now(),
        );
        r.feed(
            &input,
            Pointer::Down {
                id: 1,
                button: Button::Left,
                pos: (200.0, 100.0),
            },
            &v,
            t0,
        );
        assert!(r
            .feed(
                &input,
                Pointer::Move {
                    id: 1,
                    pos: (202.0, 100.0)
                },
                &v,
                t0
            )
            .is_empty());
        let g = r.feed(
            &input,
            Pointer::Move {
                id: 1,
                pos: (220.0, 110.0),
            },
            &v,
            t0,
        );
        assert_eq!(kinds(&g), [GestureKind::Pan { dx: 20.0, dy: 10.0 }]);
        // A drag that left the view keeps going (the view captured it).
        let g = r.feed(
            &input,
            Pointer::Move {
                id: 1,
                pos: (620.0, 110.0),
            },
            &v,
            t0,
        );
        assert_eq!(kinds(&g), [GestureKind::Pan { dx: 400.0, dy: 0.0 }]);
        assert!(r
            .feed(
                &input,
                Pointer::Up {
                    id: 1,
                    pos: (620.0, 110.0)
                },
                &v,
                t0
            )
            .is_empty());

        r.feed(
            &input,
            Pointer::Down {
                id: 2,
                button: Button::Right,
                pos: (450.0, 200.0),
            },
            &v,
            t0,
        );
        let g = r.feed(
            &input,
            Pointer::Move {
                id: 2,
                pos: (450.0, 260.0),
            },
            &v,
            t0,
        );
        assert!(matches!(g[0].kind, GestureKind::Rotate { .. }), "{g:?}");
    }

    #[test]
    fn wheel_zooms_trackpad_pans_modifier_zooms() {
        let (mut r, input, v, t0) = (
            Recognizer::default(),
            Input::default(),
            views(),
            Instant::now(),
        );
        let scroll = |r: &mut Recognizer, delta, precise, modifier| {
            r.feed(
                &input,
                Pointer::Scroll {
                    delta,
                    precise,
                    modifier,
                    pos: (300.0, 200.0),
                },
                &v,
                t0,
            )
        };
        assert_eq!(
            kinds(&scroll(&mut r, (0.0, 1.0), false, false)),
            [GestureKind::Zoom { factor: 1.2 }]
        );
        assert_eq!(
            kinds(&scroll(&mut r, (3.0, -4.0), true, false)),
            [GestureKind::Pan { dx: 3.0, dy: -4.0 }]
        );
        assert!(
            matches!(scroll(&mut r, (0.0, 5.0), true, true)[0].kind, GestureKind::Zoom { factor } if factor > 1.0)
        );
    }

    #[test]
    fn two_fingers_pinch_twist_and_pan() {
        let (mut r, input, v, t0) = (
            Recognizer::default(),
            Input::default(),
            views(),
            Instant::now(),
        );
        r.feed(
            &input,
            Pointer::Down {
                id: 1,
                button: Button::Touch,
                pos: (200.0, 200.0),
            },
            &v,
            t0,
        );
        r.feed(
            &input,
            Pointer::Down {
                id: 2,
                button: Button::Touch,
                pos: (300.0, 200.0),
            },
            &v,
            t0,
        );
        // Spread the fingers: zoom in about their centre.
        let g = r.feed(
            &input,
            Pointer::Move {
                id: 2,
                pos: (400.0, 200.0),
            },
            &v,
            t0,
        );
        assert!(
            g.iter().any(
                |g| matches!(g.kind, GestureKind::Zoom { factor } if (factor - 2.0).abs() < 1e-4)
            ),
            "{g:?}"
        );
        // Lifting a finger is not a tap.
        assert!(r
            .feed(
                &input,
                Pointer::Up {
                    id: 1,
                    pos: (200.0, 200.0)
                },
                &v,
                t0
            )
            .is_empty());
        assert!(r
            .feed(
                &input,
                Pointer::Up {
                    id: 2,
                    pos: (400.0, 200.0)
                },
                &v,
                t0
            )
            .is_empty());
    }

    #[test]
    fn right_click_and_long_press_are_long_presses() {
        let (mut r, input, v, t0) = (
            Recognizer::default(),
            Input::default(),
            views(),
            Instant::now(),
        );
        r.feed(
            &input,
            Pointer::Down {
                id: 1,
                button: Button::Right,
                pos: (200.0, 100.0),
            },
            &v,
            t0,
        );
        assert_eq!(
            kinds(&r.feed(
                &input,
                Pointer::Up {
                    id: 1,
                    pos: (200.0, 100.0)
                },
                &v,
                t0
            )),
            [GestureKind::LongPress]
        );
        r.feed(
            &input,
            Pointer::Down {
                id: 2,
                button: Button::Touch,
                pos: (200.0, 100.0),
            },
            &v,
            t0,
        );
        let late = t0 + Duration::from_secs(1);
        assert_eq!(
            kinds(&r.feed(
                &input,
                Pointer::Up {
                    id: 2,
                    pos: (200.0, 100.0)
                },
                &v,
                late
            )),
            [GestureKind::LongPress]
        );
    }

    #[test]
    fn bindings_change_the_gesture_and_can_be_turned_off() {
        let (mut r, v, t0) = (Recognizer::default(), views(), Instant::now());
        let input = Input {
            wheel: ScrollAction::Pan,
            ..Input::default()
        };
        let g = r.feed(
            &input,
            Pointer::Scroll {
                delta: (0.0, 1.0),
                precise: false,
                modifier: false,
                pos: (300.0, 200.0),
            },
            &v,
            t0,
        );
        assert_eq!(kinds(&g), [GestureKind::Pan { dx: 0.0, dy: 40.0 }]);
        let off = Input {
            gestures: false,
            ..Input::default()
        };
        assert!(r
            .feed(
                &off,
                Pointer::Scroll {
                    delta: (0.0, 1.0),
                    precise: false,
                    modifier: false,
                    pos: (300.0, 200.0)
                },
                &v,
                t0
            )
            .is_empty());
        assert!(off.inputs_for(GestureType::Zoom).is_empty());
        assert_eq!(
            Input::default().inputs_for(GestureType::Zoom),
            ["wheel", "Ctrl/Option/Cmd + wheel", "pinch"]
        );
    }
}
