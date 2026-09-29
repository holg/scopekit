# scopekit

Build [ratatui] viewers with a live [wgpu] view: a medical image, a globe,
a plot. The same app runs in two places:

- **in any terminal:** the view is rendered offscreen on the GPU and shown
  with the kitty, iTerm2 or sixel graphics protocol, or as coloured half
  blocks where the terminal has none;
- **in its own window:** [ratatui-wgpu] draws the text, and the view is
  composited onto the same surface in the same frame, at full resolution.

You write the text UI once with ratatui, handle crossterm events once (the
window translates its input into the same types), and implement one trait
for the GPU view. scopekit does the rest: the terminal graphics protocols,
the window, fonts, colours, the mouse, animation ticks and read-back.

```text
                   ┌──────────── scopekit::App ─────────────┐
                   │ draw(frame, slot)  event(ev)  tick()    │
                   └───────────────┬─────────────────────────┘
          terminal (crossterm)     │     window (winit + ratatui-wgpu)
  ratatui text ──► terminal cells  │  ratatui text ──► glyph texture ─┐
  GpuView ──► offscreen texture    │  GpuView ─────────────────────── ┼─► surface
          ──► kitty/iTerm2/sixel/  │              (same device, same   │
              half blocks          │               frame, no read-back)┘
```

It powers `dicomscope-tui`, the DICOM viewer of
[hl7v2](https://github.com/holg/hl7v2). **Start with [HOWTO.md](HOWTO.md)**; `examples/plasma.rs` is a
complete app in one file:

```sh
cargo run -p scopekit --example plasma             # in this terminal
cargo run -p scopekit --example plasma -- --window # in a window
```

Features: `terminal` and `window`, both on by default. Leave one out to drop
its dependencies. Built on wgpu 30, ratatui 0.30, ratatui-wgpu 0.6 and
ratatui-image 11; scopekit re-exports `wgpu`, `ratatui` and `crossterm` so
apps use the same versions.

[ratatui]: https://ratatui.rs
[wgpu]: https://wgpu.rs
[ratatui-wgpu]: https://docs.rs/ratatui-wgpu
