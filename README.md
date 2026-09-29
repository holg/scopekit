# scopekit

Build ratatui viewers with live **wgpu** views (a medical image, a globe,
an emulated device screen) that run

- **in any terminal:** views are rendered on the GPU and shown with the
  kitty, iTerm2 or sixel graphics protocol, or as coloured half blocks
  where the terminal has none;
- **in their own window:** the same text UI drawn by ratatui-wgpu, with the
  views composited onto the same surface at full resolution.

You write the UI once with ratatui, handle input once (the window speaks
crossterm events too), and wrap your wgpu renderer in one trait.

```text
 your App ──draw()──► ratatui text ─┬─ terminal: cells + kitty/iTerm2/sixel/half-block images
          ──event()◄─ keys, mouse   └─ window:   glyph texture + views on one wgpu surface
 your GpuView ──render(encoder, target)──► the placed rectangle, in either mode
```

| | |
|---|---|
| **Guide** | [HOWTO.md](HOWTO.md): from a first app to porting an existing renderer |
| **Example** | [`plasma.rs`](crates/scopekit/examples/plasma.rs): an animated view, a side panel and mouse control in one file |
| **Plans** | [ROADMAP.md](ROADMAP.md): console panel, layout shell, bricks (emulated IoT and healthcare devices from IFC building models) |
| **Used by** | dicomscope-tui in [hl7v2](https://github.com/holg/hl7v2), its reference app; geodb-globe next |

## Integrate in five minutes

**1. Depend on it.** The repository is private for now: use SSH and let
Cargo use your git (details, CI and local overrides: [HOWTO
§0](HOWTO.md#0-add-scopekit-to-your-project)).

```toml
# Cargo.toml
[dependencies]
scopekit = { git = "ssh://git@github.com/holg/scopekit.git", tag = "v0.1.0" }
```

```toml
# .cargo/config.toml
[net]
git-fetch-with-cli = true
```

**2. Write the app.** Text UI, input, and where the view goes:

```rust
use scopekit::crossterm::event::{Event, KeyCode};
use scopekit::ratatui::{layout::{Constraint, Layout}, widgets::Paragraph, Frame};
use scopekit::{App, Config, Flow, ViewSlot, Views};

struct Viewer;

impl App for Viewer {
    fn draw(&mut self, f: &mut Frame, slot: &mut ViewSlot) {
        let [side, main] =
            Layout::horizontal([Constraint::Length(30), Constraint::Min(1)]).areas(f.area());
        f.render_widget(Paragraph::new(format!("q quits\n{}", slot.describe())), side);
        slot.place("scene", main); // your GpuView, registered as "scene"
    }

    fn event(&mut self, ev: Event) -> Flow {
        match ev {
            Event::Key(k) if k.code == KeyCode::Char('q') => Flow::Quit,
            _ => Flow::Continue,
        }
    }
}

fn main() -> Result<(), String> {
    let (_scene, view) = scopekit::share(my_renderer());       // step 3
    // Your defaults, then scopekit's flags: --window, --backend, --protocol …
    let defaults = Config { title: "my viewer".into(), ..Config::default() };
    let (config, _your_args) = defaults.with_args(std::env::args())?;
    scopekit::run(&mut Viewer, Views::new().with("scene", view), &config)
}
```

**3. Wrap your renderer** as a `GpuView`: `prepare` builds pipelines for
the given format, `render` draws into `target.region` with `LoadOp::Load`
and a viewport. If your renderer opens its own device today, give it a
constructor that takes one ([HOWTO §9](HOWTO.md#9-moving-an-existing-wgpu-renderer-onto-scopekit)).
[`plasma.rs`](crates/scopekit/examples/plasma.rs) shows a complete one.

**4. Run it:** `cargo run` in a terminal, `cargo run -- --window` for the
window (the `--` hands the flag to your program rather than to Cargo).
Every scopekit app understands the same flags: `--window`, `--backend`,
`--protocol`, `--font`, `--title`, `--config`. The status text from `slot.describe()` tells you the GPU and how
the view is shown, e.g. `Metal · Apple M2 Max → kitty`.

## What you get

- **Terminal mode:** the view is rendered offscreen at exactly the pixel
  size of its cells and re-sent only when it changes. Graphics support is
  detected automatically: kitty, Ghostty, WezTerm, iTerm2, foot, and half
  blocks everywhere else.
- **Window mode:** native window (macOS, Windows, Linux X11 and Wayland)
  with the bundled Cascadia Mono or any TTF/OTF, light and dark palettes,
  Retina scaling. Views draw on the window surface, in the same frame,
  with no read-back.
- **One input model:** keys, mouse clicks and drags, wheel and trackpad
  (pinch and Cmd/Option-wheel for zoom on a Mac) arrive as crossterm events
  in both modes.
- **Several views**, named and placed independently.
- **A `Waker`** for background threads (serial consoles, builds).
- **Configuration** in code or TOML.
- **Offscreen export** (`render_to_rgba`) and **inline printing**
  (`terminal::print_image`) for command-line tools.
- **Testable:** `terminal::Driver` renders frames on ratatui's
  `TestBackend`, GPU included.

Built on wgpu 30, ratatui 0.30, ratatui-wgpu 0.6, ratatui-image 11 and
winit 0.30. An app that depends on wgpu itself must use 30.x; see [HOWTO
§0](HOWTO.md#use-scopekits-wgpu-ratatui-and-crossterm).

## Repository

| Path | |
|---|---|
| `crates/scopekit` | the library (its README is the docs.rs front page) |
| `HOWTO.md` | the guide; a copy of `crates/scopekit/HOWTO.md`, kept identical by CI |
| `ROADMAP.md` | what comes next |

Licensed under MIT or Apache-2.0, at your option. The bundled Cascadia
Mono font is under the SIL Open Font License 1.1
(`crates/scopekit/fonts/CascadiaOFL.txt`).
