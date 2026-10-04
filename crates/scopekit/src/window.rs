//! Window mode: the same app in a native window. ratatui-wgpu draws the
//! text into a texture; the `Compositor` puts it on the window surface
//! and then lets each placed GPU view draw into its region, on the same device, in
//! the same frame, at the window's full resolution.

use crate::app::{is_copy_key, is_help_key, is_switch_key, App, End, Flow, Mirror, ViewSlot};
use crate::config::{Config, Palette};
use crate::gesture::{Button, Pointer, Recognizer};
use crate::gpu::{instance, Gpu, PixelRect, Target, Views};
use crate::waker::Waker;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::backend::{Backend as _, WindowSize};
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::Terminal;
use ratatui_wgpu::shaders::DefaultPostProcessor;
use ratatui_wgpu::wgpu::{
    CommandEncoder, Device, Queue, SurfaceConfiguration, TextureFormat, TextureView,
};
use ratatui_wgpu::{Builder, ColorTable, Dimensions, Font, PostProcessor, WgpuBackend};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{
    ElementState, MouseButton as WinitButton, MouseScrollDelta, TouchPhase, WindowEvent,
};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

/// The bundled font: Cascadia Mono, SIL Open Font License 1.1
/// (`fonts/CascadiaOFL.txt`).
pub const FONT: &[u8] = include_bytes!("../fonts/CascadiaMono-Regular.ttf");

/// How soon to redraw after a frame did not reach the surface.
const RETRY: Duration = Duration::from_millis(33);

/// Wheel pixels per step (trackpads send many small deltas).
const WHEEL_STEP: f64 = 24.0;

/// What the event loop tells the compositor.
pub(crate) struct Shared {
    views: Views,
    /// Placed views and their pixel regions, this frame.
    regions: RefCell<Vec<(String, PixelRect)>>,
    /// Text areas drawn again over the views ([`ViewSlot::overlay`]).
    overlays: RefCell<Vec<PixelRect>>,
    dirty: Cell<bool>,
    describe: RefCell<String>,
    /// Set when the compositor ran: the frame reached the surface.
    presented: Cell<bool>,
    /// A view to capture for the clipboard in the next frame, and the result.
    copy_request: RefCell<Option<String>>,
    copied: RefCell<Option<Result<Captured, String>>>,
    /// The app wants pictures of the window ([`App::mirror`]), and the
    /// next one, copied out by the GPU and read after the frame is sent.
    mirror: Cell<bool>,
    mirror_pending: RefCell<Option<MirrorCopy>>,
}

/// A picture of the window on its way from the GPU.
struct MirrorCopy {
    device: Device,
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    padded: u32,
    bgra: bool,
}

impl MirrorCopy {
    /// The picture as RGBA rows (waits for the GPU).
    fn read(self) -> Result<(Vec<u8>, u32, u32), String> {
        let slice = self.buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("mirror poll: {e}"))?;
        rx.recv()
            .map_err(|e| format!("mirror: {e}"))?
            .map_err(|e| format!("mirror map: {e}"))?;
        let mapped = slice
            .get_mapped_range()
            .map_err(|e| format!("mirror range: {e}"))?;
        let row = (self.width * 4) as usize;
        let mut rgba = Vec::with_capacity(row * self.height as usize);
        for chunk in mapped
            .chunks(self.padded as usize)
            .take(self.height as usize)
        {
            rgba.extend_from_slice(&chunk[..row]);
        }
        drop(mapped);
        self.buffer.unmap();
        for px in rgba.chunks_exact_mut(4) {
            if self.bgra {
                px.swap(0, 2);
            }
            px[3] = 255;
        }
        Ok((rgba, self.width, self.height))
    }
}

/// A captured view: name, RGBA pixels, width, height.
type Captured = (String, Vec<u8>, u32, u32);

/// ratatui-wgpu's post-processor: the text, then the GPU views on top.
pub(crate) struct Compositor {
    text: DefaultPostProcessor,
    overlay: Overlay,
    device: Device,
    format: TextureFormat,
    gpu: Option<Gpu>,
    /// Views already given `prepare` on this device.
    prepared: HashSet<String>,
    shared: Rc<Shared>,
}

impl PostProcessor for Compositor {
    type UserData = Rc<Shared>;

    fn compile(
        device: &Device,
        text_view: &TextureView,
        config: &SurfaceConfiguration,
        shared: Rc<Shared>,
    ) -> Self {
        let i = device.adapter_info();
        *shared.describe.borrow_mut() = format!("{:?} · {} → window", i.backend, i.name);
        Compositor {
            text: DefaultPostProcessor::compile(device, text_view, config, ()),
            overlay: Overlay::new(device, text_view, config.format),
            device: device.clone(),
            format: config.format,
            gpu: None,
            prepared: HashSet::new(),
            shared,
        }
    }

    fn resize(&mut self, device: &Device, text_view: &TextureView, config: &SurfaceConfiguration) {
        self.text.resize(device, text_view, config);
        self.overlay.rebind(device, text_view);
        self.shared.dirty.set(true);
    }

    fn process(
        &mut self,
        encoder: &mut CommandEncoder,
        queue: &Queue,
        text_view: &TextureView,
        config: &SurfaceConfiguration,
        surface_view: &TextureView,
    ) {
        self.paint(encoder, queue, text_view, config, surface_view);
        self.shared.dirty.set(false);
        self.shared.presented.set(true);
        let Some(gpu) = self.gpu.as_ref() else {
            return;
        };
        // A copy request: render the view once more, into its own texture.
        if let Some(name) = self.shared.copy_request.borrow_mut().take() {
            let region = self
                .shared
                .regions
                .borrow()
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, r)| *r);
            let result = match (region, self.shared.views.get(&name)) {
                (Some(r), Some(view)) => crate::gpu::capture(
                    gpu,
                    &mut *view.borrow_mut(),
                    self.format,
                    r.width,
                    r.height,
                )
                .map(|rgba| (name.clone(), rgba, r.width, r.height)),
                _ => Err(format!("{name} is not on screen")),
            };
            *self.shared.copied.borrow_mut() = Some(result);
        }
        if self.shared.mirror.get() {
            let copy = self.mirror_copy(encoder, queue, text_view, config);
            *self.shared.mirror_pending.borrow_mut() = Some(copy);
        }
    }

    fn needs_update(&self) -> bool {
        self.shared.dirty.get()
            || self.shared.regions.borrow().iter().any(|(n, _)| {
                self.shared
                    .views
                    .get(n)
                    .is_some_and(|v| v.borrow().changed())
            })
    }
}

impl Compositor {
    /// The frame into `target`: the text, the views, the text over them.
    fn paint(
        &mut self,
        encoder: &mut CommandEncoder,
        queue: &Queue,
        text_view: &TextureView,
        config: &SurfaceConfiguration,
        target: &TextureView,
    ) {
        self.text.process(encoder, queue, text_view, config, target);
        // The queue arrives only here: views are prepared on the first
        // frame that shows them.
        let gpu = self.gpu.get_or_insert_with(|| Gpu {
            device: self.device.clone(),
            queue: queue.clone(),
        });
        for (name, region) in self.shared.regions.borrow().iter() {
            let Some(view) = self.shared.views.get(name) else {
                continue;
            };
            let mut view = view.borrow_mut();
            if self.prepared.insert(name.clone()) {
                view.prepare(gpu, self.format);
            }
            let target = Target {
                view: target,
                format: self.format,
                size: (config.width, config.height),
                region: *region,
            };
            view.render(gpu, encoder, &target);
        }
        let overlays = self.shared.overlays.borrow();
        if !overlays.is_empty() {
            self.overlay.draw(encoder, queue, config, target, &overlays);
        }
    }

    /// The frame once more, into a texture that is copied out for the app.
    fn mirror_copy(
        &mut self,
        encoder: &mut CommandEncoder,
        queue: &Queue,
        text_view: &TextureView,
        config: &SurfaceConfiguration,
    ) -> MirrorCopy {
        let (width, height) = (config.width.max(1), config.height.max(1));
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scopekit mirror"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.paint(encoder, queue, text_view, config, &view);
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = (width * 4).div_ceil(align) * align;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scopekit mirror read-back"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            size,
        );
        MirrorCopy {
            device: self.device.clone(),
            buffer,
            width,
            height,
            padded,
            bgra: matches!(
                self.format,
                TextureFormat::Bgra8Unorm | TextureFormat::Bgra8UnormSrgb
            ),
        }
    }
}

/// Draws the text texture again, clipped to overlay rectangles, after the
/// GPU views: text the app wants on top of them.
struct Overlay {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

impl Overlay {
    fn new(device: &Device, text_view: &TextureView, format: TextureFormat) -> Overlay {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scopekit overlay"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("overlay.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scopekit overlay"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scopekit overlay"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("scopekit overlay"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scopekit overlay"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind = Self::bind_group(device, &layout, text_view, &sampler, &uniforms);
        Overlay {
            pipeline,
            layout,
            sampler,
            uniforms,
            bind,
        }
    }

    fn bind_group(
        device: &Device,
        layout: &wgpu::BindGroupLayout,
        text_view: &TextureView,
        sampler: &wgpu::Sampler,
        uniforms: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scopekit overlay"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(text_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniforms.as_entire_binding(),
                },
            ],
        })
    }

    /// The text texture is recreated on resize.
    fn rebind(&mut self, device: &Device, text_view: &TextureView) {
        self.bind = Self::bind_group(
            device,
            &self.layout,
            text_view,
            &self.sampler,
            &self.uniforms,
        );
    }

    fn draw(
        &self,
        encoder: &mut CommandEncoder,
        queue: &Queue,
        config: &SurfaceConfiguration,
        surface_view: &TextureView,
        rects: &[PixelRect],
    ) {
        let mut u = [0u8; 16];
        u[0..4].copy_from_slice(&(config.width as f32).to_ne_bytes());
        u[4..8].copy_from_slice(&(config.height as f32).to_ne_bytes());
        u[8..12].copy_from_slice(&u32::from(config.format.is_srgb()).to_ne_bytes());
        queue.write_buffer(&self.uniforms, 0, &u);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scopekit overlay"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: surface_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind, &[]);
        for r in rects {
            let x = r.x.min(config.width);
            let y = r.y.min(config.height);
            let w = r.width.min(config.width - x);
            let h = r.height.min(config.height - y);
            if w > 0 && h > 0 {
                pass.set_scissor_rect(x, y, w, h);
                pass.draw(0..4, 0..1);
            }
        }
    }
}

type WindowTerminal = Terminal<WgpuBackend<'static, 'static, Compositor>>;

fn color_table(p: &Palette) -> ColorTable {
    ColorTable {
        BLACK: p.black,
        RED: p.red,
        GREEN: p.green,
        YELLOW: p.yellow,
        BLUE: p.blue,
        MAGENTA: p.magenta,
        CYAN: p.cyan,
        GRAY: p.gray,
        DARKGRAY: p.dark_gray,
        LIGHTRED: p.light_red,
        LIGHTGREEN: p.light_green,
        LIGHTYELLOW: p.light_yellow,
        LIGHTBLUE: p.light_blue,
        LIGHTMAGENTA: p.light_magenta,
        LIGHTCYAN: p.light_cyan,
        WHITE: p.white,
    }
}

/// The cell under a surface pixel. The grid is stretched over the whole
/// surface, so the mapping is a plain proportion.
fn cell_at(size: &WindowSize, (x, y): (f64, f64)) -> (u16, u16) {
    let cell = |p: f64, px: u16, cells: u16| {
        let v = p * f64::from(cells) / f64::from(px.max(1));
        (v.max(0.0) as u16).min(cells.saturating_sub(1))
    };
    (
        cell(x, size.pixels.width, size.columns_rows.width),
        cell(y, size.pixels.height, size.columns_rows.height),
    )
}

/// Pixels covered by `area`, by the same proportion.
fn region_of(size: &WindowSize, area: Rect) -> PixelRect {
    let (cols, rows) = (
        u32::from(size.columns_rows.width.max(1)),
        u32::from(size.columns_rows.height.max(1)),
    );
    let (pw, ph) = (u32::from(size.pixels.width), u32::from(size.pixels.height));
    let x0 = u32::from(area.x) * pw / cols;
    let y0 = u32::from(area.y) * ph / rows;
    let x1 = u32::from(area.x + area.width) * pw / cols;
    let y1 = u32::from(area.y + area.height) * ph / rows;
    PixelRect {
        x: x0,
        y: y0,
        width: x1.saturating_sub(x0),
        height: y1.saturating_sub(y0),
    }
}

struct Handler<'a> {
    app: &'a mut dyn App,
    config: &'a Config,
    font: &'static [u8],
    shared: Rc<Shared>,
    window: Option<Arc<Window>>,
    terminal: Option<WindowTerminal>,
    modifiers: ModifiersState,
    cursor: (f64, f64),
    held: Option<MouseButton>,
    wheel: f64,
    last_tick: Instant,
    error: Option<String>,
    /// Why the loop ended.
    end: End,
    /// What closing the window means: quit, or back to the terminal.
    closes_to: End,
    /// The last frame's cells, to tell whether a draw had text to render.
    last_frame: Option<ratatui::buffer::Buffer>,
    /// Every cell must be rendered again (after a resize, or lost text).
    full_redraw: bool,
    /// When to try again after a frame did not reach the surface.
    retry_at: Option<Instant>,
    /// Mouse, trackpad and touch input into gestures.
    recognizer: Recognizer,
    /// The built-in help box is open.
    help_open: bool,
    /// The finger that also drives the mouse events, for apps without gestures.
    touch_mouse: Option<u64>,
    /// Text selection in cells (anchor, end), made with Shift + drag.
    selection: Option<((u16, u16), (u16, u16))>,
    selecting: bool,
    /// The window size the app asked for last ([`App::window_size`]).
    asked_size: Option<((u32, u32), Instant)>,
}

impl Handler<'_> {
    /// Leave the loop. The window and its surface go first: the event loop
    /// is reused for the next window after a mode switch.
    fn finish(&mut self, event_loop: &ActiveEventLoop, end: End) {
        self.end = end;
        self.terminal = None;
        self.window = None;
        event_loop.exit();
    }

    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let (w, h) = self.config.window_size;
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title(self.config.title.clone())
                        .with_inner_size(winit::dpi::LogicalSize::new(w, h)),
                )
                .map_err(|e| format!("cannot open a window: {e}"))?,
        );
        let size = window.inner_size();
        let font = Font::new(self.font).ok_or("the font is not a TrueType or OpenType font")?;
        let px = (self.config.font_size * window.scale_factor())
            .round()
            .max(6.0) as u32;
        let p = &self.config.palette;
        let rgb = |c: [u8; 3]| Color::Rgb(c[0], c[1], c[2]);
        let backend = pollster::block_on(
            Builder::<Compositor>::from_font_and_user_data(font, self.shared.clone())
                // With the display handle: GL on Wayland (EGL) needs it.
                .with_instance(instance(
                    self.config.backend,
                    wgpu::InstanceDescriptor::new_with_display_handle_from_env(Box::new(
                        event_loop.owned_display_handle(),
                    )),
                ))
                .with_font_size_px(px)
                .with_color_table(color_table(p))
                .with_fg_color(rgb(p.foreground))
                .with_bg_color(rgb(p.background))
                .with_width_and_height(Dimensions {
                    width: NonZeroU32::new(size.width.max(1)).unwrap_or(NonZeroU32::MIN),
                    height: NonZeroU32::new(size.height.max(1)).unwrap_or(NonZeroU32::MIN),
                })
                .build_with_target(window.clone()),
        )
        .map_err(|e| format!("no GPU for the window: {e}"))?;
        self.terminal = Some(Terminal::new(backend).map_err(|e| e.to_string())?);
        window.request_redraw();
        self.window = Some(window);
        Ok(())
    }

    fn window_size(&mut self) -> Option<WindowSize> {
        self.terminal.as_mut()?.backend_mut().window_size().ok()
    }

    fn redraw(&mut self) -> Result<(), String> {
        let Some(size) = self.window_size() else {
            return Ok(());
        };
        let cell_px = (
            f32::from(size.pixels.width) / f32::from(size.columns_rows.width.max(1)),
            f32::from(size.pixels.height) / f32::from(size.columns_rows.height.max(1)),
        );
        // The size the app wants; asked again while the window has another
        // (a window that is still opening can ignore the request)
        if let (Some((w, h)), Some(window)) = (self.app.window_size(), &self.window) {
            let now: winit::dpi::LogicalSize<f64> =
                window.inner_size().to_logical(window.scale_factor());
            let differs =
                (now.width - f64::from(w)).abs() > 1.0 || (now.height - f64::from(h)).abs() > 1.0;
            let due = self
                .asked_size
                .is_none_or(|(size, at)| size != (w, h) || at.elapsed() > Duration::from_secs(1));
            if differs && due {
                let _ = window.request_inner_size(winit::dpi::LogicalSize::new(w, h));
                self.asked_size = Some(((w, h), Instant::now()));
            }
        }
        let mirror = self.app.mirror();
        self.shared.mirror.set(mirror != Mirror::Off);
        if mirror == Mirror::Now {
            self.shared.dirty.set(true);
        }
        let mut slot = ViewSlot::new(cell_px, self.shared.describe.borrow().clone());
        let (app, shared, config) = (&mut *self.app, &self.shared, self.config);
        let mut help_open = self.help_open;
        let selection = self.selection;
        let Some(terminal) = self.terminal.as_mut() else {
            return Ok(());
        };
        if self.full_redraw {
            terminal.clear().map_err(|e| e.to_string())?;
        }
        shared.presented.set(false);
        let frame = terminal
            .draw(|f| {
                app.draw(f, &mut slot);
                // The help box hides the views: they would be drawn over it.
                if help_open {
                    match app.help() {
                        Some(h) => {
                            slot.clear_views();
                            crate::help::draw(f, f.area(), &h, config);
                        }
                        None => help_open = false,
                    }
                }
                if let Some((a, b)) = selection {
                    let buf = f.buffer_mut();
                    for (x, y) in selected_cells(a, b, buf.area) {
                        if let Some(c) = buf.cell_mut((x, y)) {
                            c.modifier.insert(ratatui::style::Modifier::REVERSED);
                        }
                    }
                }
                // Before the frame is flushed: the compositor reads this.
                let regions: Vec<(String, PixelRect)> = slot
                    .placed()
                    .iter()
                    .map(|(n, r)| (n.clone(), region_of(&size, *r)))
                    .filter(|(_, r)| !r.is_empty())
                    .collect();
                if regions != *shared.regions.borrow() {
                    *shared.regions.borrow_mut() = regions;
                    shared.dirty.set(true);
                }
                let overlays: Vec<PixelRect> = slot
                    .overlays()
                    .iter()
                    .map(|r| region_of(&size, *r))
                    .filter(|r| !r.is_empty())
                    .collect();
                if overlays != *shared.overlays.borrow() {
                    *shared.overlays.borrow_mut() = overlays;
                    shared.dirty.set(true);
                }
            })
            .map_err(|e| e.to_string())?;
        self.help_open = help_open;
        let mirrored = self.shared.mirror_pending.borrow_mut().take();
        if let Some(copy) = mirrored {
            match copy.read() {
                Ok((rgba, w, h)) => self.app.mirrored(rgba, w, h),
                Err(e) => self.app.message(&format!("mirror failed: {e}")),
            }
        }
        let copied = self.shared.copied.borrow_mut().take();
        if let Some(result) = copied {
            let text = match result.and_then(|(name, rgba, w, h)| {
                copy_image(&rgba, w, h)
                    .map(|()| format!("copied {name} ({w} × {h}) to the clipboard"))
            }) {
                Ok(t) => t,
                Err(e) => format!("copy failed: {e}"),
            };
            self.app.message(&text);
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
        // ratatui-wgpu 0.6 records the changed cells into its text texture,
        // but drops that work when the surface has no texture to give
        // (common while a macOS window is being created). ratatui then
        // believes those cells are on screen and never sends them again.
        // So when a frame with text to render did not reach the surface,
        // render every cell again on the next frame.
        let had_text = self.full_redraw || self.last_frame.as_ref() != Some(frame.buffer);
        let lost = had_text && !shared.presented.get();
        self.last_frame = Some(frame.buffer.clone());
        self.full_redraw = lost;
        // Paced, not at once: a hidden window may refuse frames for a while.
        self.retry_at = lost.then(|| Instant::now() + RETRY);
        Ok(())
    }

    fn send(&mut self, event_loop: &ActiveEventLoop, ev: Event) {
        if self.help_open {
            // Any key or click only closes the help.
            if matches!(
                ev,
                Event::Key(_)
                    | Event::Mouse(MouseEvent {
                        kind: MouseEventKind::Down(_),
                        ..
                    })
            ) {
                self.help_open = false;
                self.full_redraw = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            return;
        }
        if let Event::Key(k) = &ev {
            if is_help_key(self.config, &*self.app, k) {
                self.help_open = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
                return;
            }
            if is_switch_key(self.config, &*self.app, k) {
                self.finish(event_loop, End::Switch);
                return;
            }
            if is_copy_key(self.config, &*self.app, k) {
                self.request_copy();
                return;
            }
        }
        if self.app.event(ev) == Flow::Quit {
            self.finish(event_loop, End::Quit);
        } else if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// Ask the compositor to capture the view under the pointer (or the
    /// first one) in the next frame.
    fn request_copy(&mut self) {
        let (x, y) = self.cursor_px();
        let regions = self.shared.regions.borrow();
        let under = regions.iter().rev().find(|(_, r)| {
            x >= r.x as f32
                && y >= r.y as f32
                && x < (r.x + r.width) as f32
                && y < (r.y + r.height) as f32
        });
        let name = under.or(regions.first()).map(|(n, _)| n.clone());
        drop(regions);
        match name {
            Some(n) => {
                *self.shared.copy_request.borrow_mut() = Some(n);
                self.shared.dirty.set(true);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            None => self.app.message("no view to copy"),
        }
    }

    /// Copy the selected cells' text, row by row, trailing spaces trimmed.
    fn copy_selection(&mut self) {
        let (Some((a, b)), Some(buf)) = (self.selection, self.last_frame.as_ref()) else {
            return;
        };
        let text = selection_text(buf, a, b);
        let chars = text.chars().count();
        let msg = match copy_text(&text) {
            Ok(()) => format!("copied {chars} characters to the clipboard"),
            Err(e) => format!("copy failed: {e}"),
        };
        self.app.message(&msg);
    }

    /// Pointer input for the gesture recognizer, in surface pixels.
    fn pointer(&mut self, event_loop: &ActiveEventLoop, p: Pointer) {
        if self.help_open {
            return;
        }
        let views = self.shared.regions.borrow().clone();
        let gestures = self
            .recognizer
            .feed(&self.config.input, p, &views, Instant::now());
        for g in gestures {
            if self.app.gesture(g) == Flow::Quit {
                self.finish(event_loop, End::Quit);
                return;
            }
        }
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn cursor_px(&self) -> (f32, f32) {
        (self.cursor.0 as f32, self.cursor.1 as f32)
    }

    fn mouse(&mut self, event_loop: &ActiveEventLoop, kind: MouseEventKind) {
        let Some(size) = self.window_size() else {
            return;
        };
        let (column, row) = cell_at(&size, self.cursor);
        let modifiers = mouse_modifiers(self.modifiers);
        self.send(
            event_loop,
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers,
            }),
        );
    }
}

/// A wake-up from another thread.
#[derive(Debug, Clone, Copy)]
struct Wake;

impl ApplicationHandler<Wake> for Handler<'_> {
    fn user_event(&mut self, _: &ActiveEventLoop, _: Wake) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(e) = self.start(event_loop) {
            self.error = Some(e);
            self.finish(event_loop, End::Quit);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.finish(event_loop, self.closes_to),
            WindowEvent::Resized(size) => {
                if let Some(t) = self.terminal.as_mut() {
                    t.backend_mut().resize(size.width, size.height);
                    // The backend starts a blank text texture on resize,
                    // but ratatui only sends changed cells: redraw all.
                    let _ = t.clear();
                    self.full_redraw = true;
                }
                self.shared.dirty.set(true);
                let (w, h) = (
                    size.width.min(u32::from(u16::MAX)) as u16,
                    size.height.min(u32::from(u16::MAX)) as u16,
                );
                self.send(event_loop, Event::Resize(w, h));
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                // Cmd-Q quits and Cmd-W closes the window, as in any Mac app.
                // Cmd-C (Ctrl-Shift-C elsewhere) copies the text selection.
                let m = self.modifiers;
                let copy_combo = (m.super_key() || (m.control_key() && m.shift_key()))
                    && matches!(&event.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("c"));
                if copy_combo && self.selection.is_some() {
                    self.copy_selection();
                    return;
                }
                // Cmd-V (Ctrl-Shift-V elsewhere) pastes the clipboard's
                // text, as a terminal with bracketed paste does
                let paste_combo = (m.super_key() || (m.control_key() && m.shift_key()))
                    && matches!(&event.logical_key, Key::Character(c) if c.eq_ignore_ascii_case("v"));
                if paste_combo {
                    match paste_text() {
                        Ok(text) => self.send(event_loop, Event::Paste(text)),
                        Err(e) => self.app.message(&format!("paste failed: {e}")),
                    }
                    return;
                }
                if self.modifiers.super_key() {
                    match &event.logical_key {
                        Key::Character(c) if c == "q" => return self.finish(event_loop, End::Quit),
                        Key::Character(c) if c == "w" => {
                            return self.finish(event_loop, self.closes_to)
                        }
                        _ => {}
                    }
                }
                if let Some(k) = key_event(&event.logical_key, self.modifiers) {
                    self.send(event_loop, Event::Key(k));
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (d, precise) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => ((x, y), false),
                    MouseScrollDelta::PixelDelta(p) => ((p.x as f32, p.y as f32), true),
                };
                let m = self.modifiers;
                let pos = self.cursor_px();
                self.pointer(
                    event_loop,
                    Pointer::Scroll {
                        delta: d,
                        precise,
                        modifier: m.control_key() || m.alt_key() || m.super_key(),
                        pos,
                    },
                );
                self.wheel += match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y) * WHEEL_STEP,
                    MouseScrollDelta::PixelDelta(p) => p.y,
                };
                let steps = (self.wheel / WHEEL_STEP).trunc();
                self.wheel -= steps * WHEEL_STEP;
                let kind = if steps > 0.0 {
                    MouseEventKind::ScrollUp
                } else {
                    MouseEventKind::ScrollDown
                };
                for _ in 0..steps.abs() as u32 {
                    self.mouse(event_loop, kind);
                }
            }
            WindowEvent::PinchGesture { delta, .. } if delta.is_finite() => {
                let pos = self.cursor_px();
                self.pointer(
                    event_loop,
                    Pointer::Pinch {
                        factor: 1.0 + delta as f32,
                        pos,
                    },
                );
                // No crossterm equivalent: a pinch arrives as Ctrl+wheel,
                // the usual zoom gesture.
                let kind = if delta > 0.0 {
                    MouseEventKind::ScrollUp
                } else {
                    MouseEventKind::ScrollDown
                };
                if delta.abs() > 0.01 {
                    let Some(size) = self.window_size() else {
                        return;
                    };
                    let (column, row) = cell_at(&size, self.cursor);
                    self.send(
                        event_loop,
                        Event::Mouse(MouseEvent {
                            kind,
                            column,
                            row,
                            modifiers: KeyModifiers::CONTROL,
                        }),
                    );
                }
            }
            WindowEvent::RotationGesture { delta, .. } if delta.is_finite() => {
                let pos = self.cursor_px();
                self.pointer(
                    event_loop,
                    Pointer::Twist {
                        radians: delta.to_radians(),
                        pos,
                    },
                );
            }
            WindowEvent::DoubleTapGesture { .. } => {
                let pos = self.cursor_px();
                self.pointer(event_loop, Pointer::DoubleTap { pos });
            }
            WindowEvent::Touch(t) => {
                let pos = (t.location.x as f32, t.location.y as f32);
                let id = 1000 + t.id;
                // The first finger also drives the mouse, for apps without gestures.
                let drives_mouse = match t.phase {
                    TouchPhase::Started if self.touch_mouse.is_none() => {
                        self.touch_mouse = Some(t.id);
                        true
                    }
                    _ => self.touch_mouse == Some(t.id),
                };
                if drives_mouse {
                    self.cursor = (t.location.x, t.location.y);
                    let kind = match t.phase {
                        TouchPhase::Started => MouseEventKind::Down(MouseButton::Left),
                        TouchPhase::Moved => MouseEventKind::Drag(MouseButton::Left),
                        TouchPhase::Ended | TouchPhase::Cancelled => {
                            self.touch_mouse = None;
                            MouseEventKind::Up(MouseButton::Left)
                        }
                    };
                    self.mouse(event_loop, kind);
                }
                let p = match t.phase {
                    TouchPhase::Started => Pointer::Down {
                        id,
                        button: Button::Touch,
                        pos,
                    },
                    TouchPhase::Moved => Pointer::Move { id, pos },
                    TouchPhase::Ended | TouchPhase::Cancelled => Pointer::Up { id, pos },
                };
                self.pointer(event_loop, p);
            }
            WindowEvent::CursorMoved { position, .. } => {
                let before = self.window_size().map(|s| cell_at(&s, self.cursor));
                self.cursor = (position.x, position.y);
                if self.selecting {
                    if let (Some(size), Some((a, _))) = (self.window_size(), self.selection) {
                        self.selection = Some((a, cell_at(&size, self.cursor)));
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                    return;
                }
                if let Some(b) = self.held {
                    let pos = self.cursor_px();
                    let id = mouse_id(b);
                    self.pointer(event_loop, Pointer::Move { id, pos });
                }
                let now = self.window_size().map(|s| cell_at(&s, self.cursor));
                if before != now {
                    let kind = match self.held {
                        Some(b) => MouseEventKind::Drag(b),
                        None => MouseEventKind::Moved,
                    };
                    self.mouse(event_loop, kind);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    WinitButton::Left => MouseButton::Left,
                    WinitButton::Right => MouseButton::Right,
                    WinitButton::Middle => MouseButton::Middle,
                    _ => return,
                };
                let pressed = state == ElementState::Pressed;
                // Shift + left drag selects text; the app does not see it.
                if button == MouseButton::Left && !self.help_open {
                    if pressed && self.modifiers.shift_key() {
                        if let Some(size) = self.window_size() {
                            let c = cell_at(&size, self.cursor);
                            self.selection = Some((c, c));
                            self.selecting = true;
                        }
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                        return;
                    }
                    if !pressed && self.selecting {
                        self.selecting = false;
                        return;
                    }
                    if pressed && self.selection.take().is_some() {
                        if let Some(w) = &self.window {
                            w.request_redraw();
                        }
                    }
                }
                let kind = if pressed {
                    self.held = Some(button);
                    MouseEventKind::Down(button)
                } else {
                    self.held = None;
                    MouseEventKind::Up(button)
                };
                // A click that only closes the help box is not a gesture.
                let was_help = self.help_open;
                self.mouse(event_loop, kind);
                if !was_help {
                    let (id, pos) = (mouse_id(button), self.cursor_px());
                    let p = if pressed {
                        let b = match button {
                            MouseButton::Left => Button::Left,
                            MouseButton::Right => Button::Right,
                            MouseButton::Middle => Button::Middle,
                        };
                        Pointer::Down { id, button: b, pos }
                    } else {
                        Pointer::Up { id, pos }
                    };
                    self.pointer(event_loop, p);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.redraw() {
                    self.error = Some(e);
                    self.finish(event_loop, End::Quit);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(at) = self.retry_at {
            if Instant::now() >= at {
                self.retry_at = None;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
        }
        let tick = self.app.tick_interval().map(|interval| {
            if self.last_tick.elapsed() >= interval {
                self.last_tick = Instant::now();
                if self.app.tick() {
                    if let Some(w) = &self.window {
                        w.request_redraw();
                    }
                }
            }
            self.last_tick + interval.max(Duration::from_millis(1))
        });
        let wake = match (tick, self.retry_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        event_loop.set_control_flow(match wake {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });
    }
}

/// The text of the cells between `a` and `b`, one line per row, trailing
/// spaces trimmed.
fn selection_text(buf: &ratatui::buffer::Buffer, a: (u16, u16), b: (u16, u16)) -> String {
    let mut rows: Vec<String> = Vec::new();
    let mut row_y = None;
    for (x, y) in selected_cells(a, b, buf.area) {
        if row_y != Some(y) {
            rows.push(String::new());
            row_y = Some(y);
        }
        if let (Some(line), Some(cell)) = (rows.last_mut(), buf.cell((x, y))) {
            line.push_str(cell.symbol());
        }
    }
    rows.iter()
        .map(|r| r.trim_end())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The cells between two corners of a selection, in reading order.
fn selected_cells(a: (u16, u16), b: (u16, u16), area: Rect) -> Vec<(u16, u16)> {
    let (start, end) = if (a.1, a.0) <= (b.1, b.0) {
        (a, b)
    } else {
        (b, a)
    };
    let mut v = Vec::new();
    for y in start.1..=end.1.min(area.bottom().saturating_sub(1)) {
        let x0 = if y == start.1 { start.0 } else { area.x };
        let x1 = if y == end.1 {
            end.0
        } else {
            area.right().saturating_sub(1)
        };
        for x in x0..=x1 {
            v.push((x, y));
        }
    }
    v
}

#[cfg(feature = "clipboard")]
fn copy_text(t: &str) -> Result<(), String> {
    crate::clipboard::copy_text(t)
}

#[cfg(feature = "clipboard")]
fn copy_image(rgba: &[u8], w: u32, h: u32) -> Result<(), String> {
    crate::clipboard::copy_image(rgba, w, h)
}

#[cfg(feature = "clipboard")]
fn paste_text() -> Result<String, String> {
    crate::clipboard::paste_text()
}

#[cfg(not(feature = "clipboard"))]
fn paste_text() -> Result<String, String> {
    Err("scopekit was built without the clipboard feature".into())
}

#[cfg(not(feature = "clipboard"))]
fn copy_text(_: &str) -> Result<(), String> {
    Err("scopekit was built without the clipboard feature".into())
}

#[cfg(not(feature = "clipboard"))]
fn copy_image(_: &[u8], _: u32, _: u32) -> Result<(), String> {
    Err("scopekit was built without the clipboard feature".into())
}

/// Pointer id of a mouse button (touches start at 1000).
fn mouse_id(b: MouseButton) -> u64 {
    match b {
        MouseButton::Left => 1,
        MouseButton::Right => 2,
        MouseButton::Middle => 3,
    }
}

/// A winit key as the crossterm key apps handle.
fn key_event(key: &Key, mods: ModifiersState) -> Option<KeyEvent> {
    let code = match key {
        Key::Named(n) => match n {
            NamedKey::ArrowUp => KeyCode::Up,
            NamedKey::ArrowDown => KeyCode::Down,
            NamedKey::ArrowLeft => KeyCode::Left,
            NamedKey::ArrowRight => KeyCode::Right,
            NamedKey::PageUp => KeyCode::PageUp,
            NamedKey::PageDown => KeyCode::PageDown,
            NamedKey::Home => KeyCode::Home,
            NamedKey::End => KeyCode::End,
            NamedKey::Enter => KeyCode::Enter,
            NamedKey::Escape => KeyCode::Esc,
            NamedKey::Backspace => KeyCode::Backspace,
            NamedKey::Delete => KeyCode::Delete,
            NamedKey::Insert => KeyCode::Insert,
            NamedKey::Space => KeyCode::Char(' '),
            NamedKey::Tab if mods.shift_key() => KeyCode::BackTab,
            NamedKey::Tab => KeyCode::Tab,
            NamedKey::F1 => KeyCode::F(1),
            NamedKey::F2 => KeyCode::F(2),
            NamedKey::F3 => KeyCode::F(3),
            NamedKey::F4 => KeyCode::F(4),
            NamedKey::F5 => KeyCode::F(5),
            NamedKey::F6 => KeyCode::F(6),
            NamedKey::F7 => KeyCode::F(7),
            NamedKey::F8 => KeyCode::F(8),
            NamedKey::F9 => KeyCode::F(9),
            NamedKey::F10 => KeyCode::F(10),
            NamedKey::F11 => KeyCode::F(11),
            NamedKey::F12 => KeyCode::F(12),
            _ => return None,
        },
        Key::Character(s) => KeyCode::Char(s.chars().next()?),
        _ => return None,
    };
    let mut m = KeyModifiers::NONE;
    if mods.control_key() {
        m |= KeyModifiers::CONTROL;
    }
    if mods.alt_key() {
        m |= KeyModifiers::ALT;
    }
    if mods.shift_key() && !matches!(code, KeyCode::Char(_) | KeyCode::BackTab) {
        m |= KeyModifiers::SHIFT;
    }
    Some(KeyEvent::new(code, m))
}

/// Modifiers on a mouse event. Cmd counts as Alt: on a Mac, Ctrl + scroll
/// is the system's screen zoom, so apps can take Alt (Option or Cmd) too.
fn mouse_modifiers(m: ModifiersState) -> KeyModifiers {
    let mut k = KeyModifiers::NONE;
    if m.control_key() {
        k |= KeyModifiers::CONTROL;
    }
    if m.alt_key() || m.super_key() {
        k |= KeyModifiers::ALT;
    }
    if m.shift_key() {
        k |= KeyModifiers::SHIFT;
    }
    k
}

// winit allows one event loop per process. Switching modes opens a window
// again, so the loop is kept here and run on demand where the platform can.
thread_local! {
    static EVENT_LOOP: RefCell<Option<EventLoop<Wake>>> = const { RefCell::new(None) };
}

/// Run `app` in a native window until it returns [`Flow::Quit`] or the
/// window is closed.
pub fn run(app: &mut dyn App, views: Views, config: &Config) -> Result<(), String> {
    session(app, views, config, false).map(|_| ())
}

/// One stay in a window: until the app quits, the switch key is pressed or
/// the window is closed (which ends in `End::Switch` when `closes_to_terminal`).
pub(crate) fn session(
    app: &mut dyn App,
    views: Views,
    config: &Config,
    closes_to_terminal: bool,
) -> Result<End, String> {
    let font: &'static [u8] = match &config.font {
        None => FONT,
        Some(p) => Box::leak(
            std::fs::read(p)
                .map_err(|e| format!("{}: {e}", p.display()))?
                .into_boxed_slice(),
        ),
    };
    let event_loop = match EVENT_LOOP.with(|l| l.borrow_mut().take()) {
        Some(l) => l,
        None => EventLoop::<Wake>::with_user_event()
            .build()
            .map_err(|e| format!("cannot open another window here: {e}"))?,
    };
    // A wake-up from any thread becomes a user event, then a redraw.
    let proxy = Mutex::new(event_loop.create_proxy());
    app.start(Waker::new(move || {
        if let Ok(p) = proxy.lock() {
            let _ = p.send_event(Wake);
        }
    }));
    let mut handler = Handler {
        app,
        config,
        font,
        shared: Rc::new(Shared {
            views,
            regions: RefCell::new(Vec::new()),
            overlays: RefCell::new(Vec::new()),
            dirty: Cell::new(true),
            describe: RefCell::new(String::new()),
            presented: Cell::new(false),
            copy_request: RefCell::new(None),
            copied: RefCell::new(None),
            mirror: Cell::new(false),
            mirror_pending: RefCell::new(None),
        }),
        window: None,
        terminal: None,
        modifiers: ModifiersState::empty(),
        cursor: (0.0, 0.0),
        held: None,
        wheel: 0.0,
        last_tick: Instant::now(),
        error: None,
        end: End::Quit,
        closes_to: if closes_to_terminal {
            End::Switch
        } else {
            End::Quit
        },
        last_frame: None,
        full_redraw: true,
        retry_at: None,
        recognizer: Recognizer::default(),
        help_open: false,
        touch_mouse: None,
        selection: None,
        selecting: false,
        asked_size: None,
    };
    let (event_loop, result) = run_loop(event_loop, &mut handler);
    let (end, error) = (handler.end, handler.error.take());
    drop(handler);
    EVENT_LOOP.with(|l| *l.borrow_mut() = event_loop);
    result?;
    match error {
        Some(e) => Err(e),
        None => Ok(end),
    }
}

/// Runs the loop and hands it back for the next window, where winit
/// allows running it again.
#[cfg(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
fn run_loop(
    mut event_loop: EventLoop<Wake>,
    handler: &mut Handler<'_>,
) -> (Option<EventLoop<Wake>>, Result<(), String>) {
    use winit::platform::run_on_demand::EventLoopExtRunOnDemand;
    let result = event_loop
        .run_app_on_demand(handler)
        .map_err(|e| e.to_string());
    (Some(event_loop), result)
}

/// Elsewhere the loop runs once, so a window cannot be opened again after
/// switching back to the terminal.
#[cfg(not(any(
    target_os = "macos",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
)))]
fn run_loop(
    event_loop: EventLoop<Wake>,
    handler: &mut Handler<'_>,
) -> (Option<EventLoop<Wake>>, Result<(), String>) {
    (None, event_loop.run_app(handler).map_err(|e| e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    #[test]
    fn selection_reads_like_text_in_either_direction() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 3));
        buf.set_string(0, 0, "Tokyo  6.7", ratatui::style::Style::new());
        buf.set_string(0, 1, "Osaka", ratatui::style::Style::new());
        buf.set_string(0, 2, "Kyoto  371", ratatui::style::Style::new());
        // From the middle of row 0 to the start of row 2 …
        assert_eq!(selection_text(&buf, (7, 0), (4, 2)), "6.7\nOsaka\nKyoto");
        // … and dragged backwards, the same text.
        assert_eq!(selection_text(&buf, (4, 2), (7, 0)), "6.7\nOsaka\nKyoto");
        // One cell.
        assert_eq!(selection_text(&buf, (0, 1), (0, 1)), "O");
    }
}
