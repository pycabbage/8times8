use std::sync::Arc;

use _8times8::CellIndex;
use bytemuck::{Pod, Zeroable};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    window::{Window, WindowAttributes, WindowId},
};

use self::interaction::Play;

mod audio;
mod board;
mod hud;
mod icon;
mod interaction;
#[cfg(debug_assertions)]
mod legacy;
mod machines;
mod save;
#[cfg(windows)]
mod software;

const NAME: &str = "8times8";
const SIDE: f64 = 8.0;
const WORLD_STEP: f64 = 253.0 / SIDE;
const START_OFFSET: f64 = 245.0 / (2.0 * WORLD_STEP);
const MAX_INSTANCES: usize = 1 << 18;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Theme {
    Light,
    Dark,
}

impl Theme {
    fn system(window: &Window) -> Self {
        match window.theme() {
            Some(winit::window::Theme::Dark) => Self::Dark,
            _ => Self::Light,
        }
    }

    fn from_dark(dark: bool) -> Self {
        if dark { Self::Dark } else { Self::Light }
    }
}

pub fn run() -> Result<(), String> {
    let event_loop = EventLoop::with_user_event()
        .build()
        .map_err(|err| format!("could not start the desktop event loop: {err}"))?;
    let mut app = App {
        proxy: event_loop.create_proxy(),
        state: None,
        error: None,
    };
    event_loop
        .run_app(&mut app)
        .map_err(|err| format!("desktop event loop failed: {err}"))?;
    app.error.map_or(Ok(()), Err)
}

struct GpuError(String);

struct App {
    proxy: EventLoopProxy<GpuError>,
    state: Option<Play>,
    error: Option<String>,
}

impl App {
    fn fail(&mut self, event_loop: &ActiveEventLoop, error: String) {
        if let Some(play) = &mut self.state {
            play.flush();
        }
        self.error = Some(error);
        event_loop.exit();
    }
}

fn window_attributes() -> WindowAttributes {
    let attrs = Window::default_attributes()
        .with_title(NAME)
        .with_inner_size(LogicalSize::new(1280, 820))
        .with_min_inner_size(LogicalSize::new(480, 360));
    #[cfg(target_os = "linux")]
    let attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(attrs, NAME, NAME);
    #[cfg(target_os = "windows")]
    let attrs = winit::platform::windows::WindowAttributesExtWindows::with_class_name(attrs, NAME);
    icon::apply(attrs)
}

impl ApplicationHandler<GpuError> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(play) = &self.state {
            play.window.request_redraw();
            return;
        }
        let result = event_loop
            .create_window(window_attributes())
            .map_err(|err| format!("could not create a window: {err}"))
            .and_then(|window| Play::new(Arc::new(window), self.proxy.clone()));
        match result {
            Ok(play) => self.state = Some(play),
            Err(error) => self.fail(event_loop, error),
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: GpuError) {
        self.fail(event_loop, event.0);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(play) = &mut self.state
            && play.window.id() == id
            && let Err(error) = play.on_event(event_loop, event)
        {
            self.fail(event_loop, error);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(
            self.state
                .as_mut()
                .and_then(Play::idle)
                .map_or(ControlFlow::Wait, ControlFlow::WaitUntil),
        );
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(play) = &mut self.state {
            play.flush();
        }
    }
}

/// Camera coordinates use a signed integer 1A origin and small tile offsets.
/// Projection and hit testing work relative to this origin, including west and north.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Axis {
    block: i64,
    offset: f64,
}

impl Axis {
    fn distance(self, to: Self) -> f64 {
        (i128::from(to.block) - i128::from(self.block)) as f64 * SIDE + to.offset - self.offset
    }

    fn shifted(self, tiles: f64) -> Option<Self> {
        let tile = self.offset + tiles;
        let delta = (tile / SIDE).floor();
        if !delta.is_finite() || delta.abs() >= i64::MAX as f64 {
            return None;
        }
        Some(Self {
            block: self.block.checked_add(delta as i64)?,
            // A value immediately west/north of the boundary can round up to 8.
            offset: (tile - delta * SIDE).clamp(0.0, SIDE.next_down()),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Camera {
    center: [Axis; 2],
    scale: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Hit {
    x: i64,
    y: i64,
    cell: CellIndex,
}

impl Camera {
    fn centered(view: View) -> Self {
        Self {
            center: [Axis {
                block: 0,
                offset: START_OFFSET,
            }; 2],
            scale: view.clamp_scale(
                f64::from(view.size.width.min(view.size.height)) * 0.62 / (2.0 * START_OFFSET),
            ),
        }
    }

    fn constrain(&mut self, view: View) {
        self.scale = view.clamp_scale(self.scale);
        for (axis, (low, high)) in self.center.iter_mut().zip(view.extent()) {
            if low.distance(*axis) < 0.0 {
                *axis = low;
            } else if axis.distance(high) < 0.0 {
                *axis = high;
            }
        }
    }

    fn at_minimum(self, view: View) -> bool {
        self.scale <= view.min_scale() * 1.0001
    }

    fn settle(&mut self, view: View) {
        if self.at_minimum(view) {
            self.center = view.extent_center();
        }
    }

    fn pan(&mut self, view: View, before: [f64; 2], after: [f64; 2]) -> bool {
        let prior = *self;
        let delta = std::array::from_fn(|i| (before[i] - after[i]) / self.scale);
        self.translate(delta);
        self.constrain(view);
        *self != prior
    }

    fn translate(&mut self, tiles: [f64; 2]) {
        for (axis, delta) in self.center.iter_mut().zip(tiles) {
            if let Some(next) = axis.shifted(delta) {
                *axis = next;
            }
        }
    }

    fn zoom_at(&mut self, view: View, at: [f64; 2], factor: f64) -> bool {
        let prior = *self;
        if let Some(anchor) = self.world_at(view.size, at) {
            self.anchor(view, anchor, at, self.scale * factor);
        }
        *self != prior
    }

    fn anchor(&mut self, view: View, anchor: [Axis; 2], at: [f64; 2], scale: f64) {
        self.scale = view.clamp_scale(scale);
        self.center = anchor;
        self.translate(pixel_delta(view.size, at).map(|p| -p / self.scale));
        self.constrain(view);
        self.settle(view);
    }

    fn world_at(self, size: PhysicalSize<u32>, at: [f64; 2]) -> Option<[Axis; 2]> {
        let [dx, dy] = pixel_delta(size, at);
        Some([
            self.center[0].shifted(dx / self.scale)?,
            self.center[1].shifted(dy / self.scale)?,
        ])
    }

    fn hit_axes(self, size: PhysicalSize<u32>, at: [f64; 2]) -> Option<[Axis; 2]> {
        if !(0.0..f64::from(size.width)).contains(&at[0])
            || !(0.0..f64::from(size.height)).contains(&at[1])
        {
            return None;
        }
        self.world_at(size, at)
    }

    fn hit(self, size: PhysicalSize<u32>, at: [f64; 2]) -> Option<Hit> {
        let [x, y] = self.hit_axes(size, at)?;
        Hit::from_axes(x, y)
    }

    fn world_center(self) -> [f64; 2] {
        self.center
            .map(|axis| axis.block as f64 * SIDE + axis.offset)
    }

    fn project(self, size: PhysicalSize<u32>, at: [f64; 2]) -> [f64; 2] {
        let origin = screen_center(size);
        let center = self.world_center();
        std::array::from_fn(|i| origin[i] + (at[i] - center[i]) * self.scale)
    }
}

impl Hit {
    fn from_axes(x: Axis, y: Axis) -> Option<Self> {
        Some(Self {
            x: x.block,
            y: y.block,
            cell: CellIndex::from_col_row(
                board::cell_at_offset(x.offset)?,
                board::cell_at_offset(y.offset)?,
            )?,
        })
    }
}

/// Camera limits are recomputed from the opened region, not a fixed world grid.
#[derive(Clone, Copy)]
struct View {
    size: PhysicalSize<u32>,
    dpi: f64,
    bounds: [i64; 4],
}

impl View {
    fn extent(self) -> [(Axis, Axis); 2] {
        std::array::from_fn(|i| {
            (
                Axis {
                    block: self.bounds[i].saturating_sub(1),
                    offset: 0.0,
                },
                Axis {
                    block: self.bounds[i + 2].saturating_add(1),
                    offset: 2.0 * START_OFFSET,
                },
            )
        })
    }

    fn extent_center(self) -> [Axis; 2] {
        self.extent().map(|(low, high)| {
            let sum = i128::from(low.block) + i128::from(high.block);
            let offset = (low.offset + high.offset) * 0.5 + sum.rem_euclid(2) as f64 * SIDE * 0.5;
            Axis {
                block: sum.div_euclid(2) as i64,
                offset,
            }
        })
    }

    fn max_scale(self) -> f64 {
        4.0 * WORLD_STEP * self.dpi
    }

    fn min_scale(self) -> f64 {
        let [(left, right), (top, bottom)] = self.extent();
        (0.9 * (f64::from(self.size.width.max(1)) / left.distance(right))
            .min(f64::from(self.size.height.max(1)) / top.distance(bottom)))
        .min(self.max_scale())
    }

    fn clamp_scale(self, scale: f64) -> f64 {
        scale.clamp(self.min_scale(), self.max_scale())
    }
}

fn screen_center(size: PhysicalSize<u32>) -> [f64; 2] {
    [f64::from(size.width) * 0.5, f64::from(size.height) * 0.5]
}

fn pixel_delta(size: PhysicalSize<u32>, at: [f64; 2]) -> [f64; 2] {
    let center = screen_center(size);
    [at[0] - center[0], at[1] - center[1]]
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct Instance {
    // Independent clip-space left, top, right and bottom. Computing both ends
    // before narrowing to f32 preserves visible edges of large offscreen shapes.
    rect: [f32; 4],
    color: [f32; 4],
    // Physical width, height, radius and stroke. Negative stroke denotes a diamond.
    shape: [f32; 4],
}

struct Painter<'a> {
    output: &'a mut Vec<Instance>,
    size: PhysicalSize<u32>,
    limit: usize,
}

impl Painter<'_> {
    fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    fn rect(&mut self, left: f64, top: f64, width: f64, height: f64, color: [u8; 3]) {
        self.shape([left, top, width, height], 0.0, 0.0, color);
    }

    fn rounded(
        &mut self,
        left: f64,
        top: f64,
        width: f64,
        height: f64,
        radius: f64,
        color: [u8; 3],
    ) {
        self.shape([left, top, width, height], radius, 0.0, color);
    }

    // As with a stroked canvas path, half the stroke grows out from the path and half in.
    #[allow(clippy::too_many_arguments)]
    fn outline(
        &mut self,
        left: f64,
        top: f64,
        width: f64,
        height: f64,
        radius: f64,
        stroke: f64,
        color: [u8; 3],
    ) {
        if width <= 0.0 || height <= 0.0 || stroke <= 0.0 {
            return;
        }
        self.shape(
            [
                left - stroke * 0.5,
                top - stroke * 0.5,
                width + stroke,
                height + stroke,
            ],
            radius.max(0.0).min(width.min(height) * 0.5) + stroke * 0.5,
            stroke,
            color,
        );
    }

    fn diamond(&mut self, center: [f64; 2], half: f64, color: [u8; 3]) {
        self.shape(
            [center[0] - half, center[1] - half, 2.0 * half, 2.0 * half],
            0.0,
            -1.0,
            color,
        );
    }

    fn shape(&mut self, area: [f64; 4], radius: f64, stroke: f64, [r, g, b]: [u8; 3]) {
        let [left, top, width, height] = area;
        let [right, bottom] = [left + width, top + height];
        let [w, h] = [f64::from(self.size.width), f64::from(self.size.height)];
        if self.output.len() >= self.limit
            || w == 0.0
            || h == 0.0
            || width <= 0.0
            || height <= 0.0
            || left >= w
            || top >= h
            || right <= 0.0
            || bottom <= 0.0
            || !area
                .into_iter()
                .chain([right, bottom, radius, stroke])
                .all(|n| n.is_finite() && n.abs() < f64::from(f32::MAX))
        {
            return;
        }
        self.output.push(Instance {
            rect: [
                (left * 2.0 / w - 1.0) as f32,
                (1.0 - top * 2.0 / h) as f32,
                (right * 2.0 / w - 1.0) as f32,
                (1.0 - bottom * 2.0 / h) as f32,
            ],
            color: [r, g, b, 255].map(|channel| f32::from(channel) / 255.0),
            shape: [
                width as f32,
                height as f32,
                radius.clamp(0.0, width.min(height) * 0.5) as f32,
                stroke as f32,
            ],
        });
    }
}

fn clear_color(rgb: [u8; 3], srgb: bool) -> wgpu::Color {
    let [r, g, b] = rgb.map(|n| {
        let channel = f64::from(n) / 255.0;
        if !srgb {
            channel
        } else if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    });
    wgpu::Color { r, g, b, a: 1.0 }
}

enum Frame {
    Presented,
    Hidden,
    Retry(&'static str),
}

enum Renderer {
    Gpu(Box<Graphics>),
    #[cfg(windows)]
    Software(Box<software::Software>),
}

impl Renderer {
    fn new(window: Arc<Window>, proxy: EventLoopProxy<GpuError>) -> Result<Self, String> {
        #[cfg(windows)]
        if software::is_wine() {
            // Some Wine displays acquire and present GPU textures without showing them.
            // Use the window's paint surface directly there, before attempting a swapchain.
            return Self::open_software(window);
        }
        match pollster::block_on(Graphics::new(window.clone(), proxy)) {
            Ok(gpu) => Ok(Self::Gpu(Box::new(gpu))),
            #[cfg(windows)]
            Err(gpu_error) => {
                Self::open_software(window).map_err(|error| format!("{gpu_error}; {error}"))
            }
            #[cfg(not(windows))]
            Err(error) => Err(error),
        }
    }

    #[cfg(windows)]
    fn open_software(window: Arc<Window>) -> Result<Self, String> {
        software::Software::new(window).map(|renderer| Self::Software(Box::new(renderer)))
    }

    fn draw(
        &mut self,
        window: &Arc<Window>,
        size: PhysicalSize<u32>,
        background: [u8; 3],
        instances: &[Instance],
    ) -> Result<Frame, String> {
        match self {
            Self::Gpu(gpu) => gpu.draw(window, size, background, instances),
            #[cfg(windows)]
            Self::Software(renderer) => renderer.draw(window, size, background, instances),
        }
    }
}

async fn check_gpu<T>(device: &wgpu::Device, work: impl FnOnce() -> T) -> Result<T, wgpu::Error> {
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let result = work();
    let error = validation
        .pop()
        .await
        .or(oom.pop().await)
        .or(internal.pop().await);
    error.map_or(Ok(result), Err)
}

struct Graphics {
    surface: wgpu::Surface<'static>,
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    configured: bool,
    pipeline: wgpu::RenderPipeline,
    viewport: wgpu::Buffer,
    viewport_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    warned_large: bool,
}

impl Graphics {
    async fn new(window: Arc<Window>, proxy: EventLoopProxy<GpuError>) -> Result<Self, String> {
        let backends = [
            #[cfg(target_os = "windows")]
            wgpu::Backends::DX12,
            wgpu::Backends::VULKAN,
        ];
        let allowed = wgpu::Backends::from_env().unwrap_or(wgpu::Backends::all());
        let mut errors = Vec::new();
        for backend in backends.into_iter().filter(|b| allowed.contains(*b)) {
            match Self::open_backend(window.clone(), backend).await {
                Ok(gpu) => {
                    let errors = proxy.clone();
                    gpu.device.on_uncaptured_error(Arc::new(move |err| {
                        let _ = errors.send_event(GpuError(format!("GPU rendering failed: {err}")));
                    }));
                    gpu.device.set_device_lost_callback(move |reason, message| {
                        if reason != wgpu::DeviceLostReason::Destroyed {
                            let _ =
                                proxy.send_event(GpuError(format!("GPU device lost: {message}")));
                        }
                    });
                    return Ok(gpu);
                }
                Err(error) => errors.push(format!("{backend:?}: {error}")),
            }
        }
        if errors.is_empty() {
            return Err("WGPU_BACKEND selects no graphics backend supported by this build".into());
        }
        Err(format!(
            "could not initialize graphics ({})",
            errors.join("; ")
        ))
    }

    async fn open_backend(window: Arc<Window>, backend: wgpu::Backends) -> Result<Self, String> {
        let size = window.inner_size();
        let mut options = wgpu::InstanceDescriptor::new_without_display_handle();
        options.backends = backend;
        // FXC is supplied by Windows; the GNU build does not ship or link DXC.
        options.backend_options.dx12.shader_compiler = wgpu::Dx12Compiler::Fxc;
        let instance = wgpu::Instance::new(options);
        let surface = instance
            .create_surface(window)
            .map_err(|err| format!("could not create a graphics surface: {err}"))?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(|err| format!("no compatible GPU or driver: {err}"))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some(NAME),
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(|err| format!("could not open the GPU device: {err}"))?;
        let mut config = surface
            .get_default_config(&adapter, 1, 1)
            .ok_or_else(|| "graphics adapter cannot present to the window".to_owned())?;
        if let Some(format) = surface
            .get_capabilities(&adapter)
            .formats
            .into_iter()
            .find(wgpu::TextureFormat::is_srgb)
        {
            config.format = format;
        }
        config.present_mode = wgpu::PresentMode::Fifo;
        let limit = device.limits().max_texture_dimension_2d;
        let configured = (1..=limit).contains(&size.width) && (1..=limit).contains(&size.height);
        let (pipeline, viewport, viewport_group, buffer) = check_gpu(&device, || {
            if configured {
                config.width = size.width;
                config.height = size.height;
                surface.configure(&device, &config);
            }
            let pipeline = pipeline(&device, config.format);
            let viewport = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("8times8 clip units per physical pixel"),
                size: size_of::<[f32; 2]>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            if configured {
                set_viewport(&queue, &viewport, size);
            }
            let viewport_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("8times8 viewport"),
                layout: &pipeline.get_bind_group_layout(0),
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: viewport.as_entire_binding(),
                }],
            });
            (
                pipeline,
                viewport,
                viewport_group,
                instance_buffer(&device, 128),
            )
        })
        .await
        .map_err(|err| format!("could not set up the renderer: {err}"))?;
        Ok(Self {
            surface,
            instance,
            adapter,
            device,
            queue,
            config,
            configured,
            pipeline,
            viewport,
            viewport_group,
            buffer,
            warned_large: false,
        })
    }

    fn draw(
        &mut self,
        window: &Arc<Window>,
        size: PhysicalSize<u32>,
        background: [u8; 3],
        instances: &[Instance],
    ) -> Result<Frame, String> {
        let limit = self.device.limits().max_texture_dimension_2d;
        if size.width > limit || size.height > limit {
            if !self.warned_large {
                eprintln!(
                    "{NAME}: window exceeds the GPU's {limit}px texture limit; resize to resume drawing"
                );
                self.warned_large = true;
            }
            return Ok(Frame::Hidden);
        }
        self.warned_large = false;
        for attempt in 0..2 {
            if !self.configured
                || self.config.width != size.width
                || self.config.height != size.height
            {
                self.configured = false;
                self.config.width = size.width;
                self.config.height = size.height;
                pollster::block_on(check_gpu(&self.device, || {
                    self.surface.configure(&self.device, &self.config);
                }))
                .map_err(|err| format!("could not reconfigure the graphics surface: {err}"))?;
                set_viewport(&self.queue, &self.viewport, size);
                self.configured = true;
            }
            use wgpu::CurrentSurfaceTexture::*;
            let frame = match self.surface.get_current_texture() {
                Success(frame) => frame,
                Suboptimal(frame) => {
                    self.configured = false;
                    if attempt == 0 {
                        drop(frame);
                        continue;
                    }
                    frame
                }
                Occluded => return Ok(Frame::Hidden),
                Timeout => return Ok(Frame::Retry("surface acquisition timed out")),
                Outdated => {
                    self.configured = false;
                    continue;
                }
                Lost => {
                    self.surface = self
                        .instance
                        .create_surface(window.clone())
                        .map_err(|err| format!("could not recreate the graphics surface: {err}"))?;
                    if !self
                        .surface
                        .get_capabilities(&self.adapter)
                        .formats
                        .contains(&self.config.format)
                    {
                        return Err("recreated graphics surface no longer supports the current render format".into());
                    }
                    self.configured = false;
                    continue;
                }
                Validation => return Err("GPU rejected the current graphics surface".into()),
            };
            self.present(window, frame, background, instances);
            return Ok(Frame::Presented);
        }
        Ok(Frame::Retry("graphics surface is changing"))
    }

    fn present(
        &mut self,
        window: &Window,
        frame: wgpu::SurfaceTexture,
        background: [u8; 3],
        instances: &[Instance],
    ) {
        let bytes = bytemuck::cast_slice(instances);
        if bytes.len() as u64 > self.buffer.size() {
            self.buffer = instance_buffer(&self.device, instances.len().next_power_of_two());
        }
        if !bytes.is_empty() {
            self.queue.write_buffer(&self.buffer, 0, bytes);
        }
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("8times8 frame"),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("8times8 board"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear_color(
                            background,
                            self.config.format.is_srgb(),
                        )),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if !bytes.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.viewport_group, &[]);
                pass.set_vertex_buffer(0, self.buffer.slice(..));
                pass.draw(0..6, 0..instances.len() as u32);
            }
        }
        self.queue.submit([encoder.finish()]);
        window.pre_present_notify();
        self.queue.present(frame);
    }
}

fn set_viewport(queue: &wgpu::Queue, buffer: &wgpu::Buffer, size: PhysicalSize<u32>) {
    let pixel = [size.width, size.height].map(|n| 2.0 / n as f32);
    queue.write_buffer(buffer, 0, bytemuck::bytes_of(&pixel));
}

fn instance_buffer(device: &wgpu::Device, capacity: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("8times8 instances"),
        size: (capacity * size_of::<Instance>()) as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn pipeline(device: &wgpu::Device, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
    // A single instanced vertex stream renders both backing plates and tile faces.
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("8times8 rectangles"),
        source: wgpu::ShaderSource::Wgsl(
            r#"
struct Output {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) color: vec4<f32>,
    @location(1) @interpolate(flat) shape: vec4<f32>,
    @location(2) edges: vec4<f32>,
}

@group(0) @binding(0) var<uniform> pixel: vec2<f32>;
override target_srgb: bool = false;

@vertex
fn vertex_main(
    @builtin(vertex_index) index: u32,
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
    @location(2) shape: vec4<f32>,
) -> Output {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0., 0.), vec2<f32>(1., 0.), vec2<f32>(0., 1.),
        vec2<f32>(0., 1.), vec2<f32>(1., 0.), vec2<f32>(1., 1.),
    );
    // Rasterize half a physical pixel beyond each edge, regardless of DPI.
    let pad = vec2(pixel.x, -pixel.y) * 0.5;
    let start = clamp(rect.xy - pad, vec2(-1.), vec2(1.));
    let end = clamp(rect.zw + pad, vec2(-1.), vec2(1.));
    let p = select(start, end, corners[index] == vec2(1.));
    let edges = vec4(p.x - rect.x, rect.y - p.y, rect.z - p.x, p.y - rect.w) / pixel.xyxy;
    var rgb = color.rgb;
    if target_srgb {
        rgb = select(pow((rgb + 0.055) / 1.055, vec3(2.4)), rgb / 12.92, rgb <= vec3(0.04045));
    }
    return Output(vec4(p, 0., 1.), vec4(rgb, color.a), shape, edges);
}

fn box_coverage(edges: vec4<f32>, radius: f32) -> f32 {
    let near = min(edges.xy, edges.zw);
    let far = max(edges.xy, edges.zw);
    // Exact pixel overlap along straight edges, including subpixel-wide shapes.
    let overlap = max(vec2(0.), min(near + 0.5, vec2(1.)) - max(0.5 - far, vec2(0.)));
    let area = overlap.x * overlap.y;
    if area <= 0. || radius <= 0. || any(near >= vec2(radius)) {
        return area;
    }
    let distance = length(vec2(radius) - near) - radius;
    return min(area, clamp(0.5 - distance, 0., 1.));
}

@fragment
fn fragment_main(input: Output) -> @location(0) vec4<f32> {
    let radius = input.shape.z;
    let stroke = input.shape.w;
    var coverage = box_coverage(input.edges, radius);
    if stroke < 0. {
        let near = min(input.edges.xy, input.edges.zw);
        let distance = (input.shape.x * 0.5 - near.x - near.y) * 0.70710678118;
        coverage = min(coverage, clamp(0.5 - distance, 0., 1.));
    } else if stroke > 0. && coverage > 0. {
        coverage -= box_coverage(input.edges - stroke, max(0., radius - stroke));
    }
    // Completely empty pixels (e.g. the middle of a giant frame) need not blend.
    if coverage <= 0. { discard; }
    return vec4(input.color.rgb, input.color.a * coverage);
}
"#
            .into(),
        ),
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("8times8 rectangle pipeline"),
        layout: None,
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vertex_main"),
            compilation_options: wgpu::PipelineCompilationOptions {
                constants: &[("target_srgb", if format.is_srgb() { 1.0 } else { 0.0 })],
                ..Default::default()
            },
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<Instance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4],
            })],
        },
        primitive: Default::default(),
        depth_stencil: None,
        multisample: Default::default(),
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fragment_main"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(width: u32, height: u32) -> View {
        View {
            size: PhysicalSize::new(width, height),
            dpi: 1.0,
            bounds: [-10, -8, 12, 9],
        }
    }

    #[test]
    fn hit_tests_cross_zero_with_euclidean_blocks() {
        let view = view(240, 240);
        let size = view.size;
        let mut camera = Camera::centered(view);
        camera.center[0].offset = 4.0;
        camera.center[1].offset = 4.0;
        camera.scale = 10.0;
        let west = camera.hit(size, [77.0, 81.0]).unwrap();
        assert_eq!((west.x, west.y, west.cell.index()), (-1, 0, 7));
        let north = camera.hit(size, [81.0, 77.0]).unwrap();
        assert_eq!((north.x, north.y, north.cell.index()), (0, -1, 56));
        let origin = camera.hit(size, [80.0, 80.0]).unwrap();
        assert_eq!((origin.x, origin.y, origin.cell.index()), (0, 0, 0));
        assert_eq!(camera.hit(size, [79.0, 81.0]), None);
    }

    #[test]
    fn wheel_zoom_holds_the_tile_under_the_mouse() {
        let view = view(840, 640);
        let mut camera = Camera::centered(view);
        let point = [128.0, 256.0];
        let world = camera.world_at(view.size, point).unwrap();
        assert!(camera.zoom_at(view, point, 1.4));
        assert!(camera.zoom_at(view, point, 0.7));
        for (before, after) in world
            .into_iter()
            .zip(camera.world_at(view.size, point).unwrap())
        {
            assert!(before.distance(after).abs() < 1e-10);
        }
        camera.zoom_at(view, point, 0.001);
        assert_eq!(camera.center, view.extent_center());
        assert_eq!(camera.scale, view.min_scale());
    }

    #[test]
    fn hit_testing_avoids_integer_overflow() {
        let view = view(840, 640);
        let size = view.size;
        let mut camera = Camera::centered(view);
        let point = screen_center(size);
        camera.center[0].block = i64::MAX;
        assert_eq!(camera.hit(size, point).unwrap().x, i64::MAX);
        assert_eq!(
            camera.hit(size, [point[0] + camera.scale * 8.0, point[1]]),
            None
        );
        assert_eq!(camera.hit(size, [f64::NAN, point[1]]), None);
    }

    #[test]
    fn painter_rejects_invisible_geometry_and_respects_budget() {
        let mut instances = Vec::new();
        let mut p = Painter {
            output: &mut instances,
            size: PhysicalSize::new(200, 200),
            limit: 2,
        };
        let rgb = [100, 150, 200];
        p.rect(-30.0, -30.0, 20.0, 20.0, rgb);
        p.rect(f64::NAN, 5.0, 20.0, 20.0, rgb);
        p.rounded(5.0, 5.0, 20.0, 10.0, 10.0, rgb);
        p.outline(5.0, 5.0, 10.0, 10.0, 4.0, 2.0, rgb);
        for _ in 0..4 {
            p.rect(5.0, 5.0, 20.0, 20.0, rgb);
        }
        assert_eq!(instances.len(), 2);
        assert_eq!(instances[0].shape, [20.0, 10.0, 5.0, 0.0]);
        assert_eq!(instances[1].shape, [12.0, 12.0, 5.0, 2.0]);
    }

    #[test]
    fn painter_keeps_visible_edges_of_far_offscreen_shapes() {
        let mut instances = Vec::new();
        let mut p = Painter {
            output: &mut instances,
            size: PhysicalSize::new(8192, 2048),
            limit: 1,
        };
        let offset = (1_u64 << 34) as f64;
        p.rect(
            -offset,
            -offset,
            offset + 3.25,
            offset + 6.5,
            [251, 251, 249],
        );
        let [_, _, right, bottom] = instances[0].rect;
        assert_eq!((right + 1.0) * 4096.0, 3.25);
        assert_eq!((1.0 - bottom) * 1024.0, 6.5);
    }
}
