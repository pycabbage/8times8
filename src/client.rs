use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use _8times8::{CellIndex, Game, Level};
use bytemuck::{Pod, Zeroable};
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalSize, PhysicalSize},
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{KeyCode, PhysicalKey},
    window::{Window, WindowAttributes, WindowId},
};

#[cfg(windows)]
mod software;

const NAME: &str = "8times8";
const SIDE: f64 = 8.0;
const MAX_VISIBLE_TILES_PER_AXIS: f64 = 512.0;
const MIN_DETAIL_PX: f64 = 6.0;

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
        self.error = Some(error);
        event_loop.exit();
    }
}

fn window_attributes() -> WindowAttributes {
    let attrs = Window::default_attributes()
        .with_title(NAME)
        .with_inner_size(LogicalSize::new(840, 640))
        .with_min_inner_size(LogicalSize::new(320, 240));
    #[cfg(target_os = "linux")]
    let attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(attrs, NAME, NAME);
    #[cfg(target_os = "windows")]
    let attrs = winit::platform::windows::WindowAttributesExtWindows::with_class_name(attrs, NAME);
    attrs
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
        let mut flow = ControlFlow::Wait;
        if let Some(play) = &mut self.state
            && let Some(deadline) = play.retry_at
        {
            if Instant::now() < deadline {
                flow = ControlFlow::WaitUntil(deadline);
            } else {
                play.retry_at = None;
                play.window.request_redraw();
            }
        }
        event_loop.set_control_flow(flow);
    }
}

struct Play {
    window: Arc<Window>,
    renderer: Renderer,
    game: Game,
    camera: Camera,
    cursor: Option<[f64; 2]>,
    pan_buttons: u8,
    upgrade_feedback: &'static str,
    retry_at: Option<Instant>,
    retries: u8,
    occluded: bool,
}

impl Play {
    fn new(window: Arc<Window>, proxy: EventLoopProxy<GpuError>) -> Result<Self, String> {
        let renderer = Renderer::new(window.clone(), proxy)?;
        let camera = Camera::centered(window.inner_size());
        let play = Self {
            game: Game::new(),
            window,
            renderer,
            camera,
            cursor: None,
            pan_buttons: 0,
            upgrade_feedback: "",
            retry_at: None,
            retries: 0,
            occluded: false,
        };
        play.update_title();
        play.window.request_redraw();
        Ok(play)
    }

    fn update_title(&self) {
        let upgrade = match self.game.next_upgrade() {
            Some((level, price)) => {
                format!("{}/{price} 1{level:?}", self.game.available(level))
            }
            None => "max".into(),
        };
        self.window.set_title(&format!(
            "{NAME} | 1A cleared: {} | Power: {} | U: {}{upgrade} | Click: flip · Right/mid drag: pan · Wheel: zoom · Home: center",
            self.game.cleared(Level::A),
            self.game.manual_power(),
            self.upgrade_feedback,
        ));
    }

    fn on_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) -> Result<(), String> {
        let size = self.window.inner_size();
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(size)?,
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.retries = 0;
                self.retry_at = None;
                self.camera.fit_size(size);
                self.window.request_redraw();
            }
            WindowEvent::Occluded(hidden) => {
                self.occluded = hidden;
                self.retry_at = None;
                if !hidden {
                    self.retries = 0;
                    self.window.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let before = self.hover(size);
                let cursor = [position.x, position.y];
                let moved = self.pan_buttons != 0
                    && self.cursor.is_some_and(|old| self.camera.pan(old, cursor));
                self.cursor = Some(cursor);
                if moved || before != self.hover(size) {
                    self.window.request_redraw();
                }
            }
            WindowEvent::CursorLeft { .. } | WindowEvent::Focused(false) => {
                self.pan_buttons = 0;
                if self.cursor.take().is_some() {
                    self.window.request_redraw();
                }
            }
            WindowEvent::MouseInput { button, state, .. } => {
                let mask = match button {
                    MouseButton::Right => 1,
                    MouseButton::Middle => 2,
                    _ => 0,
                };
                let before = self.hover(size);
                if state == ElementState::Pressed {
                    self.pan_buttons |= mask;
                    if button == MouseButton::Left
                        && let Some(hit) = before
                    {
                        let cleared = self.game.cleared(Level::A).clone();
                        if self.game.activate(hit.x, hit.y, hit.cell) {
                            if !self.upgrade_feedback.is_empty()
                                || self.game.cleared(Level::A) != &cleared
                            {
                                self.upgrade_feedback = "";
                                self.update_title();
                            }
                            self.window.request_redraw();
                        }
                    }
                } else {
                    self.pan_buttons &= !mask;
                }
                if before != self.hover(size) {
                    self.window.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let steps = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y),
                    MouseScrollDelta::PixelDelta(p) => p.y / 60.0,
                };
                let center = [f64::from(size.width) / 2.0, f64::from(size.height) / 2.0];
                if self.camera.zoom(size, self.cursor.unwrap_or(center), steps) {
                    self.window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::Escape) => event_loop.exit(),
                    PhysicalKey::Code(KeyCode::Home) => {
                        self.camera = Camera::centered(size);
                        self.window.request_redraw();
                    }
                    PhysicalKey::Code(KeyCode::KeyU) => {
                        self.upgrade_feedback = if self.game.try_upgrade() {
                            "bought; next "
                        } else if self.game.next_upgrade().is_some() {
                            "unavailable; "
                        } else {
                            ""
                        };
                        self.update_title();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn hover(&self, size: PhysicalSize<u32>) -> Option<Hit> {
        if self.pan_buttons != 0 {
            return None;
        }
        self.camera.hit(size, self.cursor?)
    }

    fn redraw(&mut self, size: PhysicalSize<u32>) -> Result<(), String> {
        self.retry_at = None;
        if self.occluded || size.width == 0 || size.height == 0 {
            return Ok(());
        }
        self.camera.fit_size(size);
        let hover = self.hover(size);
        match self
            .renderer
            .draw(&self.window, size, self.camera, &self.game, hover)?
        {
            Frame::Presented => self.retries = 0,
            Frame::Hidden => {}
            Frame::Retry(reason) => match self.retries {
                0..=2 => {
                    self.retries += 1;
                    self.retry_at = Some(Instant::now() + Duration::from_millis(80));
                }
                3 => {
                    self.retries += 1;
                    eprintln!(
                        "{NAME}: display temporarily unavailable ({reason}); will retry on input or resize"
                    );
                }
                _ => {}
            },
        }
        Ok(())
    }
}

/// Camera coordinates use a signed integer 1A origin and small tile offsets.
/// Projection and hit testing work relative to this origin, including west and north.
#[derive(Clone, Copy)]
struct Axis {
    block: i64,
    offset: f64,
}

impl Axis {
    fn shifted(self, tiles: f64) -> Option<Self> {
        let tile = self.offset + tiles;
        let delta = (tile / SIDE).floor();
        if !delta.is_finite() || delta.abs() >= i64::MAX as f64 {
            return None;
        }
        Some(Self {
            block: self.block.checked_add(delta as i64)?,
            // A value immediately west/north of the boundary can round up to 8.
            offset: (tile - delta * SIDE).min(SIDE.next_down()),
        })
    }
}

#[derive(Clone, Copy)]
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
    fn centered(size: PhysicalSize<u32>) -> Self {
        Self {
            center: [Axis {
                block: 0,
                offset: 4.0,
            }; 2],
            scale: (f64::from(size.width.min(size.height)) * 0.36 / SIDE)
                .clamp(Self::min_scale(size), Self::max_scale(size)),
        }
    }

    fn min_scale(size: PhysicalSize<u32>) -> f64 {
        (f64::from(size.width.max(size.height)) / MAX_VISIBLE_TILES_PER_AXIS).max(1.5)
    }

    fn max_scale(size: PhysicalSize<u32>) -> f64 {
        Self::min_scale(size).max(384.0)
    }

    fn fit_size(&mut self, size: PhysicalSize<u32>) {
        self.scale = self
            .scale
            .clamp(Self::min_scale(size), Self::max_scale(size));
    }

    fn pan(&mut self, before: [f64; 2], after: [f64; 2]) -> bool {
        let delta = std::array::from_fn(|i| (before[i] - after[i]) / self.scale);
        if delta == [0.0, 0.0] {
            return false;
        }
        self.translate(delta);
        true
    }

    fn translate(&mut self, tiles: [f64; 2]) {
        for (axis, delta) in self.center.iter_mut().zip(tiles) {
            if let Some(next) = axis.shifted(delta) {
                *axis = next;
            }
        }
    }

    fn zoom(&mut self, size: PhysicalSize<u32>, at: [f64; 2], steps: f64) -> bool {
        if !steps.is_finite() || !at.iter().all(|v| v.is_finite()) {
            return false;
        }
        let new = (self.scale * 1.2f64.powf(steps.clamp(-8.0, 8.0)))
            .clamp(Self::min_scale(size), Self::max_scale(size));
        if new == self.scale {
            return false;
        }
        let delta = pixel_delta(size, at);
        self.translate(delta.map(|px| px / self.scale - px / new));
        self.scale = new;
        true
    }

    fn hit(self, size: PhysicalSize<u32>, at: [f64; 2]) -> Option<Hit> {
        if !(0.0..f64::from(size.width)).contains(&at[0])
            || !(0.0..f64::from(size.height)).contains(&at[1])
        {
            return None;
        }
        let [dx, dy] = pixel_delta(size, at);
        let x = self.center[0].shifted(dx / self.scale)?;
        let y = self.center[1].shifted(dy / self.scale)?;
        Some(Hit {
            x: x.block,
            y: y.block,
            cell: CellIndex::from_col_row(x.offset.floor() as u8, y.offset.floor() as u8)?,
        })
    }

    fn visible_offsets(self, i: usize, pixels: f64) -> std::ops::RangeInclusive<i64> {
        let half = pixels / (2.0 * self.scale);
        let offset = self.center[i].offset;
        (((offset - half) / SIDE).floor() as i64)..=(((offset + half) / SIDE).ceil() as i64 - 1)
    }
}

fn pixel_delta(size: PhysicalSize<u32>, at: [f64; 2]) -> [f64; 2] {
    [
        at[0] - f64::from(size.width) / 2.0,
        at[1] - f64::from(size.height) / 2.0,
    ]
}

#[derive(Clone, Copy, Pod, Zeroable)]
#[repr(C)]
struct Instance {
    rect: [f32; 4],
    color: [f32; 4],
}

struct Palette {
    background: wgpu::Color,
    frame: [f32; 4],
    finished: [f32; 4],
    white: [f32; 4],
    ink: [f32; 4],
    hover: [f32; 4],
}

impl Palette {
    fn new(format: wgpu::TextureFormat) -> Self {
        let color = |rgb: [u8; 3]| {
            let [r, g, b] = rgb.map(|n| {
                let s = f32::from(n) / 255.0;
                if !format.is_srgb() {
                    s
                } else if s <= 0.04045 {
                    s / 12.92
                } else {
                    ((s + 0.055) / 1.055).powf(2.4)
                }
            });
            [r, g, b, 1.0]
        };
        let [r, g, b, a] = color([18, 25, 38]).map(f64::from);
        Self {
            background: wgpu::Color { r, g, b, a },
            frame: color([70, 88, 111]),
            finished: color([79, 226, 175]),
            white: color([245, 249, 255]),
            ink: color([29, 41, 68]),
            hover: color([162, 246, 220]),
        }
    }
}

fn board_instances(
    output: &mut Vec<Instance>,
    palette: &Palette,
    size: PhysicalSize<u32>,
    camera: Camera,
    game: &Game,
    hover: Option<Hit>,
) {
    output.clear();
    let [width, height] = [f64::from(size.width), f64::from(size.height)];
    let tile_px = camera.scale;
    let detail = tile_px >= MIN_DETAIL_PX;
    let gap = (tile_px * 0.09).clamp(1.0, 4.0);
    let board_px = SIDE * tile_px;
    let mut rect = |left: f64, top: f64, side: f64, color| {
        if left < width && top < height && left + side > 0.0 && top + side > 0.0 {
            output.push(Instance {
                rect: [
                    (left * 2.0 / width - 1.0) as f32,
                    (1.0 - top * 2.0 / height) as f32,
                    (side * 2.0 / width) as f32,
                    (-side * 2.0 / height) as f32,
                ],
                color,
            });
        }
    };
    for by in camera.visible_offsets(1, height) {
        let Some(y) = camera.center[1].block.checked_add(by) else {
            continue;
        };
        let top = height * 0.5 + (by as f64 * SIDE - camera.center[1].offset) * tile_px;
        for bx in camera.visible_offsets(0, width) {
            let Some(x) = camera.center[0].block.checked_add(bx) else {
                continue;
            };
            let Some(block) = game.block_at(x, y) else {
                continue;
            };
            let left = width * 0.5 + (bx as f64 * SIDE - camera.center[0].offset) * tile_px;
            let selected = hover.filter(|hit| hit.x == x && hit.y == y);
            let flipped = block.mask();
            let complete = flipped == u64::MAX;
            let edge = if complete {
                palette.finished
            } else if !detail && selected.is_some() {
                palette.hover
            } else {
                palette.frame
            };
            rect(left + gap * 0.5, top + gap * 0.5, board_px - gap, edge);
            if detail && !complete {
                for index in 0..64u8 {
                    let col = f64::from(index % 8);
                    let row = f64::from(index / 8);
                    let color = if flipped & (1 << index) != 0 {
                        palette.ink
                    } else if selected.is_some_and(|h| h.cell.index() == index) {
                        palette.hover
                    } else {
                        palette.white
                    };
                    rect(
                        left + col * tile_px + gap,
                        top + row * tile_px + gap,
                        tile_px - 2.0 * gap,
                        color,
                    );
                }
            } else {
                // Keep completed and zoomed-out blocks in a single progress swatch.
                let ratio = block.flipped_count() as f32 / 64.0;
                let color = std::array::from_fn(|i| {
                    palette.white[i] * (1.0 - ratio) + palette.ink[i] * ratio
                });
                rect(
                    left + gap * 1.5,
                    top + gap * 1.5,
                    board_px - 3.0 * gap,
                    color,
                );
            }
        }
    }
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
        camera: Camera,
        game: &Game,
        hover: Option<Hit>,
    ) -> Result<Frame, String> {
        match self {
            Self::Gpu(gpu) => gpu.draw(window, size, camera, game, hover),
            #[cfg(windows)]
            Self::Software(renderer) => renderer.draw(window, size, camera, game, hover),
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
    buffer: wgpu::Buffer,
    palette: Palette,
    instances: Vec<Instance>,
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
        let (pipeline, buffer) = check_gpu(&device, || {
            if configured {
                config.width = size.width;
                config.height = size.height;
                surface.configure(&device, &config);
            }
            (
                pipeline(&device, config.format),
                instance_buffer(&device, 128),
            )
        })
        .await
        .map_err(|err| format!("could not set up the renderer: {err}"))?;
        let palette = Palette::new(config.format);
        Ok(Self {
            surface,
            instance,
            adapter,
            device,
            queue,
            config,
            configured,
            pipeline,
            buffer,
            palette,
            instances: Vec::with_capacity(128),
            warned_large: false,
        })
    }

    fn draw(
        &mut self,
        window: &Arc<Window>,
        size: PhysicalSize<u32>,
        camera: Camera,
        game: &Game,
        hover: Option<Hit>,
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
            board_instances(
                &mut self.instances,
                &self.palette,
                size,
                camera,
                game,
                hover,
            );
            self.present(window, frame);
            return Ok(Frame::Presented);
        }
        Ok(Frame::Retry("graphics surface is changing"))
    }

    fn present(&mut self, window: &Window, frame: wgpu::SurfaceTexture) {
        let bytes = bytemuck::cast_slice(&self.instances);
        if bytes.len() as u64 > self.buffer.size() {
            self.buffer = instance_buffer(&self.device, self.instances.len().next_power_of_two());
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
                        load: wgpu::LoadOp::Clear(self.palette.background),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if !bytes.is_empty() {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.buffer.slice(..));
                pass.draw(0..6, 0..self.instances.len() as u32);
            }
        }
        self.queue.submit([encoder.finish()]);
        window.pre_present_notify();
        self.queue.present(frame);
    }
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
    @location(0) color: vec4<f32>,
}

@vertex
fn vertex_main(
    @builtin(vertex_index) index: u32,
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
) -> Output {
    let corners = array<vec2<f32>, 6>(
        vec2<f32>(0., 0.), vec2<f32>(1., 0.), vec2<f32>(0., 1.),
        vec2<f32>(0., 1.), vec2<f32>(1., 0.), vec2<f32>(1., 1.),
    );
    return Output(vec4<f32>(rect.xy + corners[index] * rect.zw, 0., 1.), color);
}

@fragment
fn fragment_main(input: Output) -> @location(0) vec4<f32> {
    return input.color;
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
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: size_of::<Instance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4],
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
                blend: None,
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

    #[test]
    fn hit_tests_cross_zero_with_euclidean_blocks() {
        let size = PhysicalSize::new(240, 240);
        let mut camera = Camera::centered(size);
        camera.scale = 10.0;
        let west = camera.hit(size, [79.0, 81.0]).unwrap();
        assert_eq!((west.x, west.y, west.cell.index()), (-1, 0, 7));
        let north = camera.hit(size, [81.0, 79.0]).unwrap();
        assert_eq!((north.x, north.y, north.cell.index()), (0, -1, 56));
        let origin = camera.hit(size, [80.0, 80.0]).unwrap();
        assert_eq!((origin.x, origin.y, origin.cell.index()), (0, 0, 0));
    }

    #[test]
    fn wheel_zoom_holds_the_tile_under_the_mouse() {
        let size = PhysicalSize::new(840, 640);
        let mut camera = Camera::centered(size);
        let point = [128.0, 256.0];
        let hit = camera.hit(size, point);
        assert!(camera.zoom(size, point, 3.0));
        assert_eq!(camera.hit(size, point), hit);
        assert!(camera.zoom(size, point, -6.0));
        assert_eq!(camera.hit(size, point), hit);
    }

    #[test]
    fn zoom_limits_visible_work_and_hit_testing_avoids_integer_overflow() {
        let size = PhysicalSize::new(u32::MAX, u32::MAX);
        let mut camera = Camera::centered(size);
        let point = [f64::from(size.width) / 2.0; 2];
        assert!(camera.visible_offsets(0, f64::from(size.width)).count() <= 65);
        camera.center[0].block = i64::MAX;
        assert_eq!(camera.hit(size, point).unwrap().x, i64::MAX);
        assert_eq!(
            camera.hit(size, [point[0] + camera.scale * 8.0, point[1]]),
            None
        );
        assert_eq!(camera.hit(size, [f64::NAN, point[1]]), None);
    }

    #[test]
    fn scene_shows_a_flip_then_the_four_opened_neighbors() {
        let size = PhysicalSize::new(640, 480);
        let mut camera = Camera::centered(size);
        camera.scale = 20.0;
        let palette = Palette::new(wgpu::TextureFormat::Bgra8UnormSrgb);
        let mut game = Game::new();
        let mut instances = Vec::new();
        let count_color = |instances: &[Instance], color| {
            instances.iter().filter(|tile| tile.color == color).count()
        };

        board_instances(&mut instances, &palette, size, camera, &game, None);
        assert_eq!(count_color(&instances, palette.white), 64);
        assert!(game.activate(0, 0, CellIndex::new(0).unwrap()));
        board_instances(&mut instances, &palette, size, camera, &game, None);
        assert_eq!(count_color(&instances, palette.white), 63);
        assert_eq!(count_color(&instances, palette.ink), 1);

        for i in 1..64 {
            assert!(game.activate(0, 0, CellIndex::new(i).unwrap()));
        }
        board_instances(&mut instances, &palette, size, camera, &game, None);
        assert_eq!(count_color(&instances, palette.white), 4 * 64);
        assert_eq!(count_color(&instances, palette.finished), 1);
        camera.scale = 4.0;
        board_instances(&mut instances, &palette, size, camera, &game, None);
        assert_eq!(instances.len(), 5 * 2);
    }
}
