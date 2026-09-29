# scopekit how-to

How to build a ratatui app with a live GPU view that runs in the terminal
and in its own window, and how to move an existing wgpu renderer onto it.
Every Rust example on this page is compiled as a test.

- [0. Add scopekit to your project](#0-add-scopekit-to-your-project)
- [1. The model](#1-the-model)
- [2. A text-only app](#2-a-text-only-app)
- [3. Adding a GPU view](#3-adding-a-gpu-view)
- [4. Configuration](#4-configuration)
- [5. Input: keys, mouse, wheel](#5-input-keys-mouse-wheel)
- [6. Animation and redraws](#6-animation-and-redraws)
  - [Waking the app from other threads](#waking-the-app-from-other-threads)
- [7. Exports and command-line output](#7-exports-and-command-line-output)
- [8. Testing](#8-testing)
- [9. Moving an existing wgpu renderer onto scopekit](#9-moving-an-existing-wgpu-renderer-onto-scopekit)
- [10. Recipe: geodb-globe](#10-recipe-geodb-globe)
- [11. Troubleshooting](#11-troubleshooting)

## 0. Add scopekit to your project

### The dependency

scopekit is a private repository on GitHub for now, so depend on it by git
over SSH and pin a tag:

```toml
[dependencies]
scopekit = { git = "ssh://git@github.com/holg/scopekit.git", tag = "v0.1.0" }
```

Cargo's built-in git client does not use your SSH agent reliably. Let it
use your own `git`, which already can fetch the repository, in the
project's `.cargo/config.toml`:

```toml
[net]
git-fetch-with-cli = true
```

`rev = "<commit>"` pins a commit; `branch = "main"` follows the tip. Take a
tag for anything others build.

### Features

| Features | Gets you | Leaves out |
|---|---|---|
| default (`terminal`, `window`) | both modes | |
| `default-features = false, features = ["terminal"]` | terminal only: tools used over SSH | winit, ratatui-wgpu, the bundled font |
| `default-features = false, features = ["window"]` | window only | ratatui-image, image |
| `+ "toml"` | `Config::load` / `Config::from_toml` | |

### Use scopekit's wgpu, ratatui and crossterm

A GPU view renders with scopekit's device, so its `wgpu` must be the same
crate. scopekit re-exports `wgpu`, `ratatui` and `crossterm`; import them
from there (`use scopekit::wgpu;`). If your crate also depends on wgpu
directly, it must be **30.x**. Otherwise Cargo builds two wgpu versions,
and the compiler rejects your view with "expected `wgpu::Device`, found
`wgpu::Device`".

### Developing scopekit and an app side by side

Keep the git dependency, and override it locally with a `[patch]` in the
app's workspace `Cargo.toml` (or in `.cargo/config.toml`, which you can
leave out of the repository):

```toml
[patch."ssh://git@github.com/holg/scopekit.git"]
scopekit = { path = "../scopekit/crates/scopekit" }
```

Edits in `../scopekit` then show up in the app at once, and the
dependency line stays what CI and others use.

### CI (GitHub Actions)

A workflow cannot read the private repository on its own. Give it a
read-only deploy key:

1. `ssh-keygen -t ed25519 -N "" -f scopekit_deploy -C "ci read scopekit"`
2. In `holg/scopekit` → Settings → Deploy keys: add `scopekit_deploy.pub`,
   read-only.
3. In the app's repository → Settings → Secrets → Actions: add
   `SCOPEKIT_DEPLOY_KEY` with the content of `scopekit_deploy`.
4. In the workflow, before any `cargo` step:

```yaml
env:
  CARGO_NET_GIT_FETCH_WITH_CLI: "true"
steps:
  - uses: actions/checkout@v4
  - uses: webfactory/ssh-agent@v0.9.0
    with:
      ssh-private-key: ${{ secrets.SCOPEKIT_DEPLOY_KEY }}
```

A public repository cannot build against a private scopekit for people
outside: its CI can use the key, but a stranger cloning it cannot.
Keep such crates out of public CI, or make scopekit public first.

### Integration checklist

1. Add the dependency (above).
2. Implement [`App`](crate::App): `draw` and `event` (section 2).
3. Wrap your renderer as a [`GpuView`](crate::GpuView) (sections 3
   and 9).
4. Share it and register it: `scopekit::share`, then
   `Views::new().with("name", view)`.
5. Build a [`Config`](crate::Config) from your command line (`--window`) or
   a TOML file (section 4).
6. Call `scopekit::run(&mut app, views, &config)`.
7. Test with the [`terminal::Driver`](crate::terminal::Driver) on a
   `TestBackend` (section 8).

$1
An app implements [`App`](crate::App):

| Method | Called | Does |
|---|---|---|
| `start(waker)` | once, before the first frame | keeps the [`Waker`](crate::Waker) for background threads (optional) |
| `draw(frame, slot)` | every frame | draws the text UI with ratatui, and calls `slot.place(name, rect)` to put each GPU view in a rectangle of cells |
| `event(ev)` | every key, mouse, paste or resize event | returns `Flow::Continue` or `Flow::Quit` |
| `tick()` | every `tick_interval()` | advances animations; returns `true` when something changed |

Each GPU view is a separate object implementing [`GpuView`](crate::GpuView):

| Method | Called | Does |
|---|---|---|
| `prepare(gpu, format)` | once per device, before the first render | creates pipelines and buffers |
| `render(gpu, encoder, target)` | whenever the view is on screen and has changed or moved | draws into `target.region` |
| `changed()` | before each frame | says whether the picture differs from the last render |

The app and scopekit share each view through `Rc<RefCell<…>>`, made by
[`scopekit::share`](crate::share): the app keeps a typed handle to update
the view's state (camera, data), and scopekit keeps one to render it.
Views are registered by name in [`Views`](crate::Views) and placed by the
same name, so an app can show any number: a globe next to a device
screen, four emulated boards side by side.

Where the view renders depends on the mode:

- **Terminal:** into an offscreen texture exactly the size of the placed
  rectangle, read back, and sent with the terminal's graphics protocol.
- **Window:** straight onto the window surface, inside the rectangle, after
  the text has been drawn.

## 2. A text-only app

```rust,no_run
use scopekit::crossterm::event::{Event, KeyCode};
use scopekit::ratatui::widgets::{Block, Paragraph};
use scopekit::ratatui::Frame;
use scopekit::{App, Config, Flow, ViewSlot};

struct Hello {
    presses: u32,
}

impl App for Hello {
    fn draw(&mut self, f: &mut Frame, _view: &mut ViewSlot) {
        let text = format!("{} keys pressed; q quits", self.presses);
        f.render_widget(Paragraph::new(text).block(Block::bordered()), f.area());
    }

    fn event(&mut self, ev: Event) -> Flow {
        match ev {
            Event::Key(k) if k.code == KeyCode::Char('q') => Flow::Quit,
            Event::Key(_) => {
                self.presses += 1;
                Flow::Continue
            }
            _ => Flow::Continue,
        }
    }
}

fn main() -> Result<(), String> {
    let window = std::env::args().any(|a| a == "--window");
    let config = Config {
        mode: if window { scopekit::Mode::Window } else { scopekit::Mode::Terminal },
        ..Config::default()
    };
    scopekit::run(&mut Hello { presses: 0 }, scopekit::Views::new(), &config)
}
```

This already runs in both modes. `--window` opens a native window with
the same UI in the bundled Cascadia Mono.

## 3. Adding a GPU view

A view draws with wgpu into the region it is given. There are three rules,
all because in a window the rest of the target is the text UI:

1. **Load, do not clear:** use `LoadOp::Load` in the render pass.
2. **Stay inside the region:** set the viewport and scissor to
   `target.region`.
3. **Mind the origin:** a shader that reads `@builtin(position)` gets
   whole-target pixels. Subtract `target.region.x` and `.y` to get
   view-local ones (or pass the origin as a uniform, as below).

In a terminal the region is the whole offscreen texture, cleared to black
first, so a view written to these rules works in both modes unchanged.

```rust,no_run
use scopekit::{wgpu, Gpu, GpuView, Target};

/// Fills its region with one colour that the app can change.
struct Swatch {
    rgb: [f64; 3],
    changed: bool,
}

impl GpuView for Swatch {
    fn prepare(&mut self, _gpu: &Gpu, _format: wgpu::TextureFormat) {
        // Create pipelines here. They must target `format`: the window's
        // surface format, or scopekit::gpu::OFFSCREEN_FORMAT in a terminal.
    }

    fn render(&mut self, _gpu: &Gpu, encoder: &mut wgpu::CommandEncoder, t: &Target<'_>) {
        let r = t.region;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("swatch"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: t.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            ..Default::default()
        });
        pass.set_viewport(r.x as f32, r.y as f32, r.width as f32, r.height as f32, 0.0, 1.0);
        pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        // A real view draws here; see examples/plasma.rs for a full pipeline.
        let _ = self.rgb;
        self.changed = false;
    }

    fn changed(&self) -> bool {
        self.changed
    }
}
```

The app places it and keeps a handle to change it:

```rust,no_run
# use scopekit::{wgpu, Gpu, GpuView, Target};
# struct Swatch { rgb: [f64; 3], changed: bool }
# impl GpuView for Swatch {
#     fn prepare(&mut self, _: &Gpu, _: wgpu::TextureFormat) {}
#     fn render(&mut self, _: &Gpu, _: &mut wgpu::CommandEncoder, _: &Target<'_>) {}
# }
use scopekit::crossterm::event::{Event, KeyCode};
use scopekit::ratatui::layout::{Constraint, Layout};
use scopekit::ratatui::widgets::Paragraph;
use scopekit::ratatui::Frame;
use scopekit::{App, Config, Flow, ViewSlot};
use std::cell::RefCell;
use std::rc::Rc;

struct Viewer {
    swatch: Rc<RefCell<Swatch>>,
    popup: bool,
}

impl App for Viewer {
    fn draw(&mut self, f: &mut Frame, view: &mut ViewSlot) {
        let [side, main] =
            Layout::horizontal([Constraint::Length(24), Constraint::Min(1)]).areas(f.area());
        f.render_widget(Paragraph::new(view.describe().to_string()), side);
        // Hide the view while a popup covers it: in a window the view is
        // drawn on top of the text, so it would hide the popup.
        if !self.popup {
            view.place("swatch", main);
        }
    }

    fn event(&mut self, ev: Event) -> Flow {
        if let Event::Key(k) = ev {
            match k.code {
                KeyCode::Char('q') => return Flow::Quit,
                KeyCode::Char('r') => {
                    let mut s = self.swatch.borrow_mut();
                    s.rgb = [1.0, 0.0, 0.0];
                    s.changed = true; // re-render; in a terminal, re-send
                }
                KeyCode::Char('?') => self.popup = !self.popup,
                _ => {}
            }
        }
        Flow::Continue
    }
}

fn main() -> Result<(), String> {
    let (swatch, view) = scopekit::share(Swatch { rgb: [0.2, 0.4, 0.8], changed: true });
    let mut app = Viewer { swatch, popup: false };
    let views = scopekit::Views::new().with("swatch", view);
    scopekit::run(&mut app, views, &Config::default())
}
```

`view.describe()` gives a status string such as `Metal · Apple M2 Max →
kitty` or `… → window`, or `no GPU (…)` if no adapter could be opened.

## 4. Configuration

Everything is in [`Config`](crate::Config); set what you need and take the
rest from `Config::default()`:

| Field | Default | For |
|---|---|---|
| `mode` | `Mode::Terminal` | `Terminal` or `Window` |
| `backend` | `Backend::Auto` | GPU backend: `Metal`, `Vulkan`, `Dx12`, `Gl`; `Auto` honours `WGPU_BACKEND` |
| `protocol` | `Protocol::Auto` | terminal graphics: `Kitty`, `Iterm2`, `Sixel`, `Halfblocks`; `Auto` asks the terminal |
| `mouse` | `true` | mouse reporting in the terminal (users hold Shift to select text) |
| `title` | `"scopekit"` | window title |
| `window_size` | `(1280.0, 800.0)` | initial window size, logical pixels |
| `font_size` | `15.0` | window text size, logical pixels (scaled for Retina) |
| `font` | `None` | a `.ttf`, `.otf` or `.ttc`; `None` uses the bundled Cascadia Mono |
| `palette` | `Palette::DARK` | window colours: the 16 named colours, foreground and background; `Palette::LIGHT` is included |
| `idle_poll` | 250 ms | how often to wake without input when the app sets no tick interval |

Every scopekit app gets the same command-line flags from
[`Config::with_args`](crate::Config::with_args): set your defaults, then
let the flags override them. Arguments scopekit does not know come back
for your own parsing:

```rust
use scopekit::{Config, Mode};

let defaults = Config { title: "my viewer".into(), ..Config::default() };
let args = ["my-viewer", "--window", "--backend", "metal", "data.bin"].map(String::from);
let (config, rest) = defaults.with_args(args).unwrap(); // in main: .with_args(std::env::args())
assert_eq!(config.mode, Mode::Window);
assert_eq!(rest, ["data.bin"]);
```

| Flag | Sets |
|---|---|
| `--window`, `-w` / `--terminal` | `mode` (also `SCOPEKIT_MODE=window`) |
| `--backend auto\|metal\|vulkan\|dx12\|gl` | `backend` |
| `--protocol auto\|kitty\|iterm2\|sixel\|halfblocks` | `protocol` |
| `--font FILE`, `--font-size N`, `--title TEXT` | window text and title |
| `--no-mouse` | `mouse` off |
| `--config FILE` | a TOML file first, flags on top (`toml` feature) |

With `cargo run`, put them after `--`: `cargo run -- --window`.

Or set the fields yourself; a typical command line maps straight onto them:

```rust
use scopekit::{Backend, Config, Mode, Palette, Protocol};

let window = true; // from --window
let config = Config {
    mode: if window { Mode::Window } else { Mode::Terminal },
    backend: Backend::Auto,
    protocol: Protocol::Auto,
    title: "my viewer".into(),
    font_size: 14.0,
    palette: Palette { background: [0, 0, 0], ..Palette::DARK },
    ..Config::default()
};
assert_eq!(config.window_size, (1280.0, 800.0));
```

With the `toml` feature a project can keep all of this in a file:
`Config::load(path)` or `Config::from_toml(text)`. Every key is optional;
see the `config_file` example in the API docs, including per-colour
overrides such as `[colors] background = "#101418"`.

The palette matters only in a window, where there is no terminal theme.
The defaults fix one trap: in CSS colour names, which ratatui-wgpu uses by
default, "DarkGray" is *lighter* than "Gray".

## 5. Input: keys, mouse, wheel

Both modes deliver `crossterm::event::Event`s, so one handler serves both.
The window translates winit's input into them:

| Window input | Arrives as |
|---|---|
| keys, F1–F12, arrows, Tab / Shift-Tab | `Event::Key`, as in a terminal |
| Ctrl, Alt | `KeyModifiers::CONTROL`, `ALT` |
| `Cmd-Q`, `Cmd-W` | closes the window (the app is not asked) |
| mouse buttons, drags, moves | `Event::Mouse` with the *cell* under the pointer |
| wheel, trackpad scroll | `ScrollUp`/`ScrollDown`, one per 24 pixels of scrolling |
| Cmd or Option + wheel | wheel with `KeyModifiers::ALT` |
| trackpad pinch | wheel with `KeyModifiers::CONTROL` |
| resize | `Event::Resize` |

For zoom, accept Ctrl *or* Alt with the wheel. On a Mac, Ctrl + scroll is
the system's screen zoom and never reaches any app, in a terminal or not.

To turn a cell into a position in the view, use the slot's metrics, saved
during `draw`:

```rust
use scopekit::ratatui::layout::Rect;
use scopekit::ViewSlot;

/// The view pixel at the centre of cell (col, row), for zooming about the
/// pointer. `area` is the rectangle the view was placed in.
fn view_pixel(slot: &ViewSlot, area: Rect, col: u16, row: u16) -> (f32, f32) {
    let (cw, ch) = slot.cell_px();
    ((f32::from(col - area.x) + 0.5) * cw, (f32::from(row - area.y) + 0.5) * ch)
}

let slot = ViewSlot::new((10.0, 20.0), String::new());
assert_eq!(view_pixel(&slot, Rect::new(5, 2, 40, 20), 6, 2), (15.0, 10.0));
```

`slot.px_size(area)` is the view's pixel size for an area. In a terminal
the cell size comes from the terminal's font. In a window it is the
surface divided by the text grid, so it includes Retina scaling.

## 6. Animation and redraws

The screen is redrawn after every event. For motion without input
(playback, a spinning globe), return an interval from `tick_interval()`
and `true` from `tick()` when something moved:

```rust
use std::time::Duration;

struct Spinner {
    angle: f32,
    running: bool,
}

impl Spinner {
    // In `impl App`: fn tick(&mut self) -> bool, fn tick_interval(&self)
    fn tick(&mut self) -> bool {
        if self.running {
            self.angle += 0.05;
        }
        self.running
    }

    fn tick_interval(&self) -> Option<Duration> {
        self.running.then_some(Duration::from_millis(33))
    }
}

let mut s = Spinner { angle: 0.0, running: true };
assert!(s.tick() && s.tick_interval().is_some());
```

Return `None` when idle: the event loop then sleeps until input arrives,
using no CPU.

`GpuView::changed` decides what gets re-rendered. Set it when the view's
inputs change and clear it in `render`. In a terminal every render means
an image transfer (kitty, iTerm2 and sixel images can be hundreds of KB),
so views that don't change shouldn't say they did. In a window a render
costs only GPU time.

### Waking the app from other threads

Output from a background thread (an emulator's serial port, a build, a
network client) should appear at once, not at the next key press. Keep
the [`Waker`](crate::Waker) that `start` receives and call `wake()` after
new data arrives. The loop then redraws immediately, in both modes, and
wake-ups faster than frames coalesce:

```rust
use scopekit::Waker;
use std::sync::{Arc, Mutex};

/// Lines a background thread appends; the app draws them.
#[derive(Default)]
struct Console {
    lines: Arc<Mutex<Vec<String>>>,
}

impl Console {
    // In `impl App`: fn start(&mut self, waker: Waker)
    fn start(&mut self, waker: Waker) {
        let lines = self.lines.clone();
        std::thread::spawn(move || {
            for i in 0..3 {
                lines.lock().unwrap().push(format!("line {i}"));
                waker.wake(); // redraw now
            }
        })
        .join()
        .unwrap();
    }
}

let mut c = Console::default();
c.start(Waker::noop());
assert_eq!(c.lines.lock().unwrap().len(), 3);
```

$1

The same view renders without any UI, for screenshots, PNG exports and
tests:

```rust,no_run
# use scopekit::{wgpu, Gpu, GpuView, Target};
# struct Swatch;
# impl GpuView for Swatch {
#     fn prepare(&mut self, _: &Gpu, _: wgpu::TextureFormat) {}
#     fn render(&mut self, _: &Gpu, _: &mut wgpu::CommandEncoder, _: &Target<'_>) {}
# }
// Tightly packed RGBA8, sRGB: straight into the png or image crates.
let rgba = scopekit::render_to_rgba(&mut Swatch, 1024, 768, scopekit::Backend::Auto)?;
assert_eq!(rgba.len(), 1024 * 768 * 4);

// Or print it into the terminal's scrollback, like `cat` for pictures.
scopekit::terminal::print_image(rgba, 1024, 768, scopekit::Protocol::Auto)?;
# Ok::<(), String>(())
```

For many renders, keep a `scopekit::gpu::Offscreen` instead: it creates the
device and pipelines once.

## 8. Testing

`terminal::Driver` is the terminal mode without the terminal. It draws
frames on any ratatui backend, including `TestBackend`, and with half
blocks the view's pixels land in the cell buffer where a test can read
them:

```rust,no_run
# use scopekit::crossterm::event::Event;
# use scopekit::ratatui::Frame;
# use scopekit::{wgpu, App, Flow, Gpu, GpuView, Target, ViewSlot};
# struct Solid;
# impl GpuView for Solid {
#     fn prepare(&mut self, _: &Gpu, _: wgpu::TextureFormat) {}
#     fn render(&mut self, _: &Gpu, _: &mut wgpu::CommandEncoder, _: &Target<'_>) {}
# }
# struct MyApp;
# impl App for MyApp {
#     fn draw(&mut self, f: &mut Frame, v: &mut ViewSlot) { v.place("solid", f.area()) }
#     fn event(&mut self, _: Event) -> Flow { Flow::Continue }
# }
use scopekit::ratatui::{backend::TestBackend, Terminal};
use scopekit::terminal::{picker, Driver};
use scopekit::{Config, Protocol};

let config = Config { protocol: Protocol::Halfblocks, ..Config::default() };
let (_handle, view) = scopekit::share(Solid);
let views = scopekit::Views::new().with("solid", view);
let mut driver = Driver::new(picker(Protocol::Halfblocks), views, &config);
let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
driver.draw(&mut term, &mut MyApp).unwrap();
let cell = &term.backend().buffer()[(10, 5)];
// cell.fg / cell.bg hold the view's colours; cell.symbol() is a half block.
```

Send events with `app.event(..)` between draws. `tests/terminal.rs` in
this crate and `dicomscope-tui`'s viewer test do exactly this. GPU tests
should skip, not fail, where `scopekit::gpu::headless` finds no adapter
(some CI machines).

## 9. Moving an existing wgpu renderer onto scopekit

Most renderers open their own device and surface. A GPU view instead
receives a device and draws into someone else's texture. The change is
the same in every codebase:

1. **Add a constructor that takes a device.**
   `Renderer::on_device(device, queue, format)` builds pipelines for
   `format` and stores the device and queue. It has no surface of its own.
   Keep the old constructor for the app's standalone window if it has one.
2. **Split "draw" from "present".** The drawing has to go into a passed-in
   `CommandEncoder` and `TextureView`, with no `get_current_texture`, no
   `submit` and no `present`: scopekit does those.
3. **Draw with Load and a viewport**, as in section 3.
4. **Offset the framebuffer position** if the shader uses
   `@builtin(position)`.
5. **Wrap it** in a struct implementing `GpuView`. It holds the renderer as
   an `Option`, because it is created in `prepare`, plus the state that
   `render` needs (camera, scene, pending uploads) and a `changed` flag.

`dicomscope-core`'s renderer went through exactly these steps, and its
wrapper, struct included, is about 55 lines (`demo/dicomscope-tui/src/gpu.rs`):

```rust,ignore
impl GpuView for SliceView {
    fn prepare(&mut self, gpu: &Gpu, format: wgpu::TextureFormat) {
        self.renderer = Some(Renderer::on_device(gpu.device.clone(), gpu.queue.clone(), format, 1, 1));
    }
    fn render(&mut self, _: &Gpu, encoder: &mut wgpu::CommandEncoder, t: &Target<'_>) {
        let Some(r) = self.renderer.as_mut() else { return };
        r.resize(t.size.0, t.size.1);
        if let Some(frame) = self.pending.take() { r.upload(&frame); }
        if let Some(mut u) = self.uniforms {
            u.tx += t.region.x as f32;              // step 4
            u.ty += t.region.y as f32;
            r.set_uniforms(u);
        }
        r.draw_image_over(encoder, t.view, (t.region.x, t.region.y, t.region.width, t.region.height));
        self.changed = false;
    }
    fn changed(&self) -> bool { self.changed }
}
```

That move took dicomscope-tui's app, GPU, terminal and window code from
1,422 lines to 694: its own window loop, compositor, font handling,
graphics-protocol code and offscreen renderer went.

## 10. Recipe: geodb-globe

`geodb-rs/crates/geodb-globe` (the wgpu 3D globe for geodb-core) already
has everything scopekit needs, in its own form:

- `Renderer::render(target, camera, scene)` draws into a texture view;
- `render_to_rgba` does the offscreen read-back;
- `src/bin/geodb-globe/tui.rs` is a ratatui UI with its own half-block
  `GlobeWidget`;
- `window.rs` is a winit window.

Moving it onto scopekit gives kitty, iTerm2 and sixel images in terminals
that have them (today it is half blocks only), and the window mode with
the text panels next to the globe, in one code path.

1. **Upgrade wgpu 28 → 30.** scopekit, ratatui-wgpu 0.6 and the view
   share one device, so they must be on one wgpu version. What changed
   between them (all hit by dicomscope):
   - `SurfaceConfiguration` gained `color_space`;
   - `Device::poll` takes `PollType::wait_indefinitely()`;
   - `BufferSlice::get_mapped_range()` returns a `Result`;
   - `PipelineLayoutDescriptor` has `bind_group_layouts: &[Option<&_>]` and
     `immediate_size`;
   - render pipelines take `multiview_mask`;
   - color attachments take `depth_slice`.

   The browser build (`webgpu`/`webgl` features) upgrades with it.
2. **Add `Renderer::on_device(device, queue, format)`** next to the existing
   constructor (section 9, step 1). Keep `Presenter` for the web canvas.
3. **Write `GlobeView: GpuView`.** It holds `Option<Renderer>`, the
   `OrbitCamera`, the current `Scene` and `changed`. `render` sets the
   viewport and scissor to `target.region` and calls
   `renderer.render(target.view, &camera, &scene)`. The existing render
   pass must switch from `Clear` to `Load`: clear the region by drawing the
   background (space) inside the viewport, not with the attachment's
   `LoadOp`.
4. **Make `Tui` implement `scopekit::App`.**
   - `draw` keeps the tabs, search and panel, and replaces the
     `GlobeWidget` with `slot.place("globe", globe_area)`.
   - `event` keeps the existing key handling: it already matches on
     crossterm `KeyCode`s, and returns `Flow::Quit` where it returned
     `Action::Quit`.
   - The spin and query debounce move to `tick` / `tick_interval`.
5. **Labels.** Place names drawn *over* the globe with `draw_labels` cannot
   stay ratatui text, because in a window the view is drawn on top of the
   text. Either render the labels in the GPU view (for example as marker
   sprites with a glyph atlas), or draw them in a side list, as dicomscope
   does.
6. **`main.rs`** maps `--window` onto `Config::mode`, and keeps
   `--screenshot` via `render_to_rgba`. `window.rs` and the half-block
   widget can then go.

In a terminal, pointer picking on the globe works as before: mouse events
arrive as cells, and `slot.cell_px()` converts them to view pixels for the
camera's ray cast.

## 11. Troubleshooting

| Symptom | Cause, fix |
|---|---|
| The view is missing in the terminal and the status says `half blocks` | The terminal did not answer the graphics query (Terminal.app, Alacritty, tmux without passthrough). Use iTerm2, kitty, WezTerm or Ghostty, or `Mode::Window`. |
| The view shows as garbage characters | The terminal claims a protocol it does not support; force `Protocol::Halfblocks` or the right one. |
| A popup is hidden behind the view | Don't `place` the view while the popup is open (section 3). |
| Window text missing after resizing | Fixed in scopekit (it forces a full redraw after a resize); if you drive ratatui-wgpu yourself, call `terminal.clear()` after `resize`. |
| Selection bars and borders too bright in the window | You passed a palette based on CSS names; use `Palette::DARK` or define `dark_gray` darker than `gray`. |
| `no GPU (…)` over SSH or in CI | No adapter there. Try `Backend::Gl`, or render elsewhere; tests should skip. |
| Slow in the terminal with a large view | Every changed frame is an image transfer. Clear `changed` when nothing moved, lower the tick rate, or use the window. |
| Benchmarks slower than expected | The workspace `release` profile optimises for size; use a speed profile (`native` in this repository). |
