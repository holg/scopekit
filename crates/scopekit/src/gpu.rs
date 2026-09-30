//! The GPU side: the [`GpuView`] an app implements, the device it gets,
//! and offscreen rendering with read-back for terminals and exports.

use crate::config::Backend;
use std::cell::RefCell;
use std::rc::Rc;

/// The device and queue a view renders with. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Gpu {
    /// The device.
    pub device: wgpu::Device,
    /// Its queue.
    pub queue: wgpu::Queue,
}

impl Gpu {
    /// `"Metal · Apple M2 Max"`: backend and adapter, for status lines.
    pub fn describe(&self) -> String {
        let i = self.device.adapter_info();
        format!("{:?} · {}", i.backend, i.name)
    }
}

/// A rectangle in pixels of the render target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PixelRect {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width.
    pub width: u32,
    /// Height.
    pub height: u32,
}

impl PixelRect {
    /// `true` when it covers no pixels.
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Where a view draws this frame.
pub struct Target<'a> {
    /// The texture view to draw into.
    pub view: &'a wgpu::TextureView,
    /// Its format, as given to [`GpuView::prepare`].
    pub format: wgpu::TextureFormat,
    /// The whole target's size in pixels.
    pub size: (u32, u32),
    /// The part that belongs to this view. In a window the rest of the
    /// target is the text UI: draw with `LoadOp::Load`, and set the
    /// viewport and scissor to this rectangle.
    pub region: PixelRect,
}

/// Something that draws with wgpu: a globe, a DICOM slice, a plot.
///
/// scopekit calls [`prepare`](GpuView::prepare) once per device and target
/// format, then [`render`](GpuView::render) whenever the view is on screen
/// and [`changed`](GpuView::changed) says so, or its region moved. In a
/// window the target is the window's surface, shared with the text; in a
/// terminal it is an offscreen texture exactly the size of the region.
pub trait GpuView {
    /// Create pipelines and buffers for `format` on `gpu`'s device.
    fn prepare(&mut self, gpu: &Gpu, format: wgpu::TextureFormat);

    /// Record the drawing into `encoder`. Stay inside `target.region`.
    fn render(&mut self, gpu: &Gpu, encoder: &mut wgpu::CommandEncoder, target: &Target<'_>);

    /// Whether the picture differs from the last render: new data, camera
    /// moved, animation running. Terminal mode re-sends the image only
    /// then, so returning `true` needlessly costs bandwidth.
    fn changed(&self) -> bool {
        true
    }
}

/// A view shared between the app, which updates its state, and scopekit,
/// which renders it.
pub type SharedView = Rc<RefCell<dyn GpuView>>;

/// Wrap a view for sharing, and keep a typed handle for the app:
///
/// ```ignore
/// let (globe, view) = scopekit::share(Globe::new());
/// globe.borrow_mut().camera.rotate(0.1);   // the app's handle
/// scopekit::run(app, Some(view), config);  // scopekit's handle
/// ```
pub fn share<V: GpuView + 'static>(view: V) -> (Rc<RefCell<V>>, SharedView) {
    let typed = Rc::new(RefCell::new(view));
    let shared: SharedView = typed.clone();
    (typed, shared)
}

/// A wgpu instance limited to `backend`. `WGPU_BACKEND` narrows `Auto`.
pub fn instance(backend: Backend, desc: wgpu::InstanceDescriptor) -> wgpu::Instance {
    let backends = match backend {
        Backend::Auto if std::env::var_os("WGPU_BACKEND").is_some() => desc.backends,
        other => other.backends(),
    };
    wgpu::Instance::new(wgpu::InstanceDescriptor { backends, ..desc })
}

/// A device without a window, for terminals and exports.
pub fn headless(backend: Backend) -> Result<Gpu, String> {
    let instance = instance(
        backend,
        wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
    );
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))
    .map_err(|e| format!("no GPU adapter: {e}"))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("scopekit"),
        required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
        ..Default::default()
    }))
    .map_err(|e| format!("no GPU device: {e}"))?;
    Ok(Gpu { device, queue })
}

/// The format offscreen targets use: sRGB, like window surfaces, so a
/// picture looks the same in both.
pub const OFFSCREEN_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// An offscreen target the size of one view, with read-back.
pub struct Offscreen {
    gpu: Gpu,
    texture: Option<wgpu::Texture>,
    prepared: bool,
}

impl Offscreen {
    /// A target on `gpu`; textures are made on first use.
    pub fn new(gpu: Gpu) -> Offscreen {
        Offscreen {
            gpu,
            texture: None,
            prepared: false,
        }
    }

    /// The device, for status lines.
    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// Render `view` at `width` x `height` and return tightly packed RGBA8
    /// (sRGB) rows.
    pub fn render(
        &mut self,
        view: &mut dyn GpuView,
        width: u32,
        height: u32,
    ) -> Result<Vec<u8>, String> {
        let (w, h) = (width.max(1), height.max(1));
        if !self.prepared {
            view.prepare(&self.gpu, OFFSCREEN_FORMAT);
            self.prepared = true;
        }
        let fits = self
            .texture
            .as_ref()
            .is_some_and(|t| (t.width(), t.height()) == (w, h));
        if !fits {
            self.texture = Some(self.gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("scopekit offscreen"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: OFFSCREEN_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            }));
        }
        let Some(texture) = &self.texture else {
            return Err("no offscreen texture".into());
        };
        let tv = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("scopekit offscreen"),
            });
        // Start from black, so a view that draws nothing is not garbage.
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scopekit clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &tv,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        let target = Target {
            view: &tv,
            format: OFFSCREEN_FORMAT,
            size: (w, h),
            region: PixelRect {
                x: 0,
                y: 0,
                width: w,
                height: h,
            },
        };
        view.render(&self.gpu, &mut encoder, &target);
        read_back(&self.gpu, encoder, texture, w, h)
    }
}

/// Render a view that is already prepared for `format` on `gpu` into a
/// `width` x `height` texture of that format, and return RGBA8 rows (BGRA
/// targets are swizzled). Used to copy a window's view without preparing
/// it again on another device.
#[cfg(feature = "window")]
pub(crate) fn capture(
    gpu: &Gpu,
    view: &mut dyn GpuView,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let (w, h) = (width.max(1), height.max(1));
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("scopekit capture"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let tv = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("scopekit capture"),
        });
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("scopekit capture clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &tv,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    let target = Target {
        view: &tv,
        format,
        size: (w, h),
        region: PixelRect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        },
    };
    view.render(gpu, &mut encoder, &target);
    let mut rgba = read_back(gpu, encoder, &texture, w, h)?;
    if matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    ) {
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }
    for px in rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }
    Ok(rgba)
}

/// Copy `texture` into a buffer after `encoder`'s work and return its rows.
fn read_back(
    gpu: &Gpu,
    mut encoder: wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    w: u32,
    h: u32,
) -> Result<Vec<u8>, String> {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = (w * 4).div_ceil(align) * align;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("scopekit read-back"),
        size: u64::from(padded) * u64::from(h),
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
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(std::iter::once(encoder.finish()));
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| format!("read-back poll: {e}"))?;
    rx.recv()
        .map_err(|e| format!("read-back: {e}"))?
        .map_err(|e| format!("read-back map: {e}"))?;
    let mapped = slice
        .get_mapped_range()
        .map_err(|e| format!("read-back range: {e}"))?;
    let row = (w * 4) as usize;
    let mut out = Vec::with_capacity(row * h as usize);
    for chunk in mapped.chunks(padded as usize).take(h as usize) {
        out.extend_from_slice(&chunk[..row]);
    }
    drop(mapped);
    buffer.unmap();
    Ok(out)
}

/// Render a view once into RGBA8 pixels without any UI: screenshots, PNG
/// exports, tests.
pub fn render_to_rgba(
    view: &mut dyn GpuView,
    width: u32,
    height: u32,
    backend: Backend,
) -> Result<Vec<u8>, String> {
    Offscreen::new(headless(backend)?).render(view, width, height)
}

/// The GPU views an app shows, by name. The app places them with
/// [`ViewSlot::place`](crate::ViewSlot::place) using the same names.
///
/// ```ignore
/// let (globe, globe_view) = scopekit::share(Globe::new());
/// let (board, board_view) = scopekit::share(Framebuffer::new());
/// let views = Views::new().with("globe", globe_view).with("board", board_view);
/// scopekit::run(&mut app, views, &config)?;
/// ```
#[derive(Clone, Default)]
pub struct Views {
    list: Vec<(String, SharedView)>,
}

impl Views {
    /// No views: a text-only app.
    pub fn new() -> Views {
        Views::default()
    }

    /// Add `view` as `name`, replacing an earlier view of that name.
    pub fn with(mut self, name: &str, view: SharedView) -> Views {
        self.add(name, view);
        self
    }

    /// Add `view` as `name`, replacing an earlier view of that name.
    pub fn add(&mut self, name: &str, view: SharedView) {
        self.list.retain(|(n, _)| n != name);
        self.list.push((name.to_string(), view));
    }

    /// The view called `name`.
    pub fn get(&self, name: &str) -> Option<&SharedView> {
        self.list.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    /// Every view with its name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &SharedView)> {
        self.list.iter().map(|(n, v)| (n.as_str(), v))
    }

    /// `true` when there are none.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}
