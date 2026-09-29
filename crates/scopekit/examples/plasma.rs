//! A complete scopekit app in one file: an animated GPU view (a plasma
//! shader) beside a text panel, in the terminal or in a window.
//!
//! ```text
//! cargo run -p scopekit --example plasma             # terminal
//! cargo run -p scopekit --example plasma -- --window # native window
//! cargo run -p scopekit --example plasma -- --protocol halfblocks
//! ```
//!
//! Keys: `+`/`-` zoom, arrows pan, space pauses, `q` quits. Mouse: wheel
//! zooms, left-drag pans.

use scopekit::crossterm::event::{Event, KeyCode, MouseButton, MouseEventKind};
use scopekit::ratatui::layout::{Constraint, Layout};
use scopekit::ratatui::widgets::{Block, Paragraph};
use scopekit::ratatui::Frame;
use scopekit::{wgpu, App, Config, Flow, Gpu, GpuView, Target, ViewSlot};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

const SHADER: &str = r#"
struct U { time: f32, zoom: f32, pan: vec2<f32>, origin: vec2<f32>, size: vec2<f32> };
@group(0) @binding(0) var<uniform> u: U;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    // Framebuffer pixels -> view coordinates, independent of where the
    // view sits in the target.
    let q = ((pos.xy - u.origin) / u.size.y - vec2<f32>(u.size.x / u.size.y * 0.5, 0.5)) / u.zoom + u.pan;
    let v = sin(q.x * 10.0 + u.time) + sin(q.y * 10.0 + u.time * 1.3)
          + sin((q.x + q.y) * 7.0 + u.time * 0.7) + sin(length(q) * 12.0 - u.time * 2.0);
    let c = 0.5 + 0.5 * cos(vec3<f32>(0.0, 2.1, 4.2) + v * 1.2);
    return vec4<f32>(c, 1.0);
}
"#;

/// The GPU view: owns its pipeline, knows nothing about terminals.
struct Plasma {
    time: f32,
    zoom: f32,
    pan: (f32, f32),
    changed: bool,
    pipe: Option<(wgpu::RenderPipeline, wgpu::Buffer, wgpu::BindGroup)>,
}

impl GpuView for Plasma {
    fn prepare(&mut self, gpu: &Gpu, format: wgpu::TextureFormat) {
        let d = &gpu.device;
        let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("plasma"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let buffer = d.create_buffer(&wgpu::BufferDescriptor {
            label: Some("plasma uniforms"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let layout = d.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind = d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        let pipeline_layout = d.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = d.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("plasma"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(format.into())],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        self.pipe = Some((pipeline, buffer, bind));
    }

    fn render(&mut self, gpu: &Gpu, encoder: &mut wgpu::CommandEncoder, target: &Target<'_>) {
        let Some((pipeline, buffer, bind)) = &self.pipe else {
            return;
        };
        let r = target.region;
        let u: [f32; 8] = [
            self.time,
            self.zoom,
            self.pan.0,
            self.pan.1,
            r.x as f32,
            r.y as f32,
            r.width as f32,
            r.height as f32,
        ];
        let bytes: Vec<u8> = u.iter().flat_map(|f| f.to_le_bytes()).collect();
        gpu.queue.write_buffer(buffer, 0, &bytes);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("plasma"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target.view,
                depth_slice: None,
                resolve_target: None,
                // Load: in a window the rest of the target is the text UI.
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_viewport(
            r.x as f32,
            r.y as f32,
            r.width as f32,
            r.height as f32,
            0.0,
            1.0,
        );
        pass.set_scissor_rect(r.x, r.y, r.width, r.height);
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.draw(0..3, 0..1);
        self.changed = false;
    }

    fn changed(&self) -> bool {
        self.changed
    }
}

/// The app: text UI and input; drives the view through its handle.
struct Demo {
    plasma: Rc<RefCell<Plasma>>,
    paused: bool,
    started: Instant,
    drag: Option<(u16, u16)>,
    cell_px: (f32, f32),
    /// The view's height in pixels: one view unit at zoom 1.
    view_h: f32,
}

impl App for Demo {
    fn draw(&mut self, f: &mut Frame, view: &mut ViewSlot) {
        let [side, main] =
            Layout::horizontal([Constraint::Length(30), Constraint::Min(10)]).areas(f.area());
        let p = self.plasma.borrow();
        let text = format!(
            "zoom  {:.2}\npan   {:.2}, {:.2}\n{}\n\n+ -  zoom\narrows  pan\nspace  pause\nq  quit\n\nwheel zooms,\ndrag pans\n\n{}",
            p.zoom, p.pan.0, p.pan.1, if self.paused { "paused" } else { "running" }, view.describe()
        );
        f.render_widget(
            Paragraph::new(text).block(Block::bordered().title(" plasma ")),
            side,
        );
        let block = Block::bordered().title(" GPU view ");
        let inner = block.inner(main);
        f.render_widget(block, main);
        view.place("plasma", inner);
        self.cell_px = view.cell_px();
        self.view_h = view.px_size(inner).1.max(1) as f32;
    }

    fn event(&mut self, event: Event) -> Flow {
        let mut p = self.plasma.borrow_mut();
        let step = 0.1 / p.zoom;
        match event {
            Event::Key(k) => match k.code {
                KeyCode::Char('q') | KeyCode::Esc => return Flow::Quit,
                KeyCode::Char('+') => p.zoom *= 1.25,
                KeyCode::Char('-') => p.zoom /= 1.25,
                KeyCode::Left => p.pan.0 -= step,
                KeyCode::Right => p.pan.0 += step,
                KeyCode::Up => p.pan.1 -= step,
                KeyCode::Down => p.pan.1 += step,
                KeyCode::Char(' ') => self.paused = !self.paused,
                _ => return Flow::Continue,
            },
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => p.zoom *= 1.1,
                MouseEventKind::ScrollDown => p.zoom /= 1.1,
                MouseEventKind::Down(MouseButton::Left) => self.drag = Some((m.column, m.row)),
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some((x, y)) = self.drag.replace((m.column, m.row)) {
                        // Cells -> pixels -> view units (a view height is 1.0).
                        let h = self.view_h * p.zoom;
                        p.pan.0 -= (f32::from(m.column) - f32::from(x)) * self.cell_px.0 / h;
                        p.pan.1 -= (f32::from(m.row) - f32::from(y)) * self.cell_px.1 / h;
                    }
                }
                MouseEventKind::Up(_) => self.drag = None,
                _ => return Flow::Continue,
            },
            _ => {}
        }
        p.changed = true;
        Flow::Continue
    }

    fn tick(&mut self) -> bool {
        if self.paused {
            return false;
        }
        let mut p = self.plasma.borrow_mut();
        p.time = self.started.elapsed().as_secs_f32();
        p.changed = true;
        true
    }

    fn tick_interval(&self) -> Option<Duration> {
        // 30 frames a second; terminals get fewer when the image is large.
        (!self.paused).then_some(Duration::from_millis(33))
    }
}

fn main() {
    // The app's defaults first, then scopekit's common flags on top:
    // --window, --backend, --protocol, --font, --title …
    let defaults = Config {
        title: "scopekit plasma".into(),
        ..Config::default()
    };
    let (config, _rest) = match defaults.with_args(std::env::args()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("plasma: {e}");
            std::process::exit(2);
        }
    };
    let (plasma, view) = scopekit::share(Plasma {
        time: 0.0,
        zoom: 1.0,
        pan: (0.0, 0.0),
        changed: true,
        pipe: None,
    });
    let mut app = Demo {
        plasma,
        paused: false,
        started: Instant::now(),
        drag: None,
        cell_px: (8.0, 16.0),
        view_h: 1.0,
    };
    let views = scopekit::Views::new().with("plasma", view);
    if let Err(e) = scopekit::run(&mut app, views, &config) {
        eprintln!("plasma: {e}");
        std::process::exit(1);
    }
}
