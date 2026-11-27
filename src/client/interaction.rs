use std::{
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use _8times8::{Amount, BoardEvent, Coord, Game, Level, Target};
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, Touch, TouchPhase, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoopProxy},
    keyboard::{KeyCode, ModifiersState, PhysicalKey},
    window::{CursorIcon, Window},
};

use super::{
    Axis, Camera, Frame, GpuError, Hit, Instance, MAX_INSTANCES, NAME, Painter, Renderer, Theme,
    View, WORLD_STEP,
    audio::Audio,
    board::{self, Effects},
    hud::{self, Action, Hud, MachineRow, MachineTier},
    machines::{MACHINE_PRICE, MACHINES_PER_TIER, Machines},
    save::{self, Saved},
    screen_center,
};
#[cfg(debug_assertions)]
use super::{hud::DebugAction, legacy};

const FRAME_TIME: Duration = Duration::from_nanos(16_666_667);
const SAVE_DELAY: Duration = Duration::from_millis(400);
const DRAG_START: f64 = 5.0;
const LONG_PRESS: Duration = Duration::from_millis(400);
const PAINT_GAP: Duration = Duration::from_millis(70);
const TOOLTIP_DELAY: Duration = Duration::from_millis(700);
const HINT_DELAY: Duration = Duration::from_millis(700);
const HINT_DURATION: Duration = Duration::from_secs(12);
const ZOOM_FACTOR: f64 = 1.4;
const WHEEL_ZOOM_FACTOR: f64 = 1.20;
const WHEEL_LINE_PIXELS: f64 = 16.0;
const WHEEL_PIXEL_CAP: f64 = 200.0;
const PIXEL_ZOOM_COEFFICIENT: f64 = 0.0018;
const FAST_PIXEL_ZOOM_COEFFICIENT: f64 = 0.01;
const HUD_INSTANCE_RESERVE: usize = 32 * 1024;
const ANIMATION_BUDGET: usize = 1500;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Gesture {
    Idle,
    Hud {
        action: Action,
        start: [f64; 2],
    },
    Press {
        start: [f64; 2],
        at: Instant,
    },
    Pan {
        last: [f64; 2],
    },
    Paint {
        pos: [f64; 2],
        last: Option<Target>,
        next: Instant,
    },
    Void,
}

#[derive(Clone, Copy)]
struct Pinch {
    anchor: [Axis; 2],
    distance: f64,
    scale: f64,
}

struct Tween {
    from: Camera,
    to: Camera,
    start: Instant,
    duration: Duration,
}

impl Tween {
    fn sample(&self, now: Instant) -> (Camera, bool) {
        let t = (now.saturating_duration_since(self.start).as_secs_f64()
            / self.duration.as_secs_f64())
        .min(1.0);
        if t == 1.0 {
            return (self.to, true);
        }
        let e = if t < 0.5 {
            2.0 * t * t
        } else {
            1.0 - 2.0 * (1.0 - t).powi(2)
        };
        let mut camera = self.from;
        camera.scale *= (self.to.scale / self.from.scale).powf(e);
        camera.translate(std::array::from_fn(|i| {
            self.from.center[i].distance(self.to.center[i]) * e
        }));
        (camera, false)
    }
}

pub(super) struct Play {
    pub(super) window: Arc<Window>,
    renderer: Renderer,
    instances: Vec<Instance>,
    game: Game,
    machines: Machines,
    hud: Hud,
    audio: Audio,
    sound: bool,
    theme: Theme,
    theme_chosen: bool,
    camera: Camera,
    camera_dpi: f64,
    camera_touched: bool,
    tween: Option<Tween>,
    effects: Effects,
    cursor: Option<[f64; 2]>,
    tooltip: Option<(Action, Option<Instant>)>,
    gesture: Gesture,
    mouse_button: Option<MouseButton>,
    touches: Vec<(u64, [f64; 2])>,
    pinch: Option<Pinch>,
    space: bool,
    modifiers: ModifiersState,
    hint_seen: bool,
    hint_shown: bool,
    hint_flips: u64,
    hint_at: Option<Instant>,
    saves: Option<SaveWorker>,
    _save_session: save::SaveSession,
    #[cfg(debug_assertions)]
    clipboard: Option<arboard::Clipboard>,
    #[cfg(debug_assertions)]
    debug_pending: Option<DebugAction>,
    #[cfg(debug_assertions)]
    machine_paused: bool,
    #[cfg(debug_assertions)]
    machine_speed: u32,
    save_at: Option<Instant>,
    retry_at: Option<Instant>,
    frame_at: Option<Instant>,
    retries: u8,
    occluded: bool,
}

impl Play {
    pub(super) fn new(
        window: Arc<Window>,
        proxy: EventLoopProxy<GpuError>,
    ) -> Result<Self, String> {
        let save_session = save::lock_session()?;
        let renderer = Renderer::new(window.clone(), proxy)?;
        let now = Instant::now();
        let loaded = save::load_validated(|saved| decode_components(saved, now));
        let mut save_enabled = true;
        let (game, machines, saved) = match loaded {
            Ok(Some((saved, (game, machines)))) => (game, machines, Some(saved)),
            Ok(None) => (Game::new(), Machines::default(), None),
            Err(error) => {
                save_enabled = false;
                eprintln!("{NAME}: {error}; autosave disabled to preserve the existing save");
                (Game::new(), Machines::default(), None)
            }
        };
        let view = View {
            size: window.inner_size(),
            dpi: window.scale_factor(),
            bounds: game.bounds(),
        };
        let saved_camera = saved.as_ref().and_then(|s| s.camera);
        let mut camera = Camera::centered(view);
        if let Some((blocks, offsets, scale)) = saved_camera {
            camera.center = std::array::from_fn(|i| Axis {
                block: blocks[i],
                offset: offsets[i],
            });
            camera.scale = scale * view.dpi;
            camera.constrain(view);
        }
        let theme_pref = saved.as_ref().and_then(|s| s.dark);
        let theme = theme_pref.map_or_else(|| Theme::system(&window), Theme::from_dark);
        let hint_seen = saved.as_ref().is_some_and(|s| s.hint_seen);
        let sound = saved.as_ref().is_none_or(|s| s.sound);
        let mut audio = Audio::default();
        audio.set_enabled(sound);
        let saves = save_enabled.then(SaveWorker::start).flatten();
        let mut hud = Hud::default();
        hud.set_sound(Some(sound));
        hud.set_hint(false);
        hud.set_save_blocked(saves.is_none());
        let mut play = Self {
            window,
            renderer,
            instances: Vec::with_capacity(256),
            game,
            machines,
            hud,
            audio,
            sound,
            theme,
            theme_chosen: theme_pref.is_some(),
            camera,
            camera_dpi: view.dpi,
            camera_touched: saved_camera.is_some(),
            tween: None,
            effects: Effects::default(),
            cursor: None,
            tooltip: None,
            gesture: Gesture::Idle,
            mouse_button: None,
            touches: Vec::with_capacity(2),
            pinch: None,
            space: false,
            modifiers: ModifiersState::empty(),
            hint_seen,
            hint_shown: false,
            hint_flips: 0,
            hint_at: (!hint_seen).then_some(now + HINT_DELAY),
            saves,
            _save_session: save_session,
            #[cfg(debug_assertions)]
            clipboard: None,
            #[cfg(debug_assertions)]
            debug_pending: None,
            #[cfg(debug_assertions)]
            machine_paused: false,
            #[cfg(debug_assertions)]
            machine_speed: 1,
            save_at: None,
            retry_at: None,
            frame_at: None,
            retries: 0,
            occluded: false,
        };
        play.update_hud();
        play.window.request_redraw();
        Ok(play)
    }

    fn view(&self) -> View {
        View {
            size: self.window.inner_size(),
            dpi: self.window.scale_factor(),
            bounds: self.game.bounds(),
        }
    }

    pub(super) fn on_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        event: WindowEvent,
    ) -> Result<(), String> {
        let now = Instant::now();
        let view = self.view();
        if matches!(
            event,
            WindowEvent::CursorLeft { .. }
                | WindowEvent::Focused(false)
                | WindowEvent::Occluded(true)
                | WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. }
                | WindowEvent::Touch(_)
        ) {
            self.clear_tooltip();
        }
        match event {
            WindowEvent::CloseRequested | WindowEvent::Destroyed => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(view, now)?,
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.retries = 0;
                self.retry_at = None;
                if view.size.width > 0 && view.size.height > 0 {
                    self.advance_camera(now);
                    self.tween = None;
                    if self.camera_touched {
                        self.camera.scale *= view.dpi / self.camera_dpi;
                        let before = self.camera.scale;
                        self.camera.constrain(view);
                        if self.camera.scale > before {
                            self.camera.settle(view);
                        }
                        self.schedule_save(now);
                    } else {
                        self.camera = Camera::centered(view);
                    }
                    self.camera_dpi = view.dpi;
                    self.window.request_redraw();
                }
            }
            WindowEvent::Occluded(hidden) => {
                self.occluded = hidden;
                self.retry_at = None;
                self.frame_at = None;
                if hidden {
                    self.submit_save();
                } else {
                    self.retries = 0;
                    self.window.request_redraw();
                }
            }
            WindowEvent::ThemeChanged(theme) if !self.theme_chosen => {
                self.theme = Theme::from_dark(theme == winit::window::Theme::Dark);
                self.window.request_redraw();
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::CursorMoved { position, .. } if self.touches.is_empty() => {
                let before = self.hover_state(view);
                let cursor = [position.x, position.y];
                self.cursor = Some(cursor);
                if self.mouse_button.is_some() {
                    self.move_pointer(view, cursor, now);
                }
                self.update_tooltip(view, now);
                self.update_cursor(view);
                if before != self.hover_state(view) {
                    self.window.request_redraw();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.cursor = None;
                if self.touches.is_empty() {
                    self.gesture = Gesture::Idle;
                    self.mouse_button = None;
                }
                self.window.request_redraw();
            }
            WindowEvent::Focused(false) => {
                self.space = false;
                self.modifiers = ModifiersState::empty();
                self.gesture = Gesture::Idle;
                self.mouse_button = None;
                self.touches.clear();
                self.pinch = None;
                self.cursor = None;
                self.hud.set_focus(None);
                self.update_cursor(view);
                self.window.request_redraw();
            }
            WindowEvent::MouseInput { button, state, .. } if self.touches.is_empty() => {
                if matches!(
                    button,
                    MouseButton::Left | MouseButton::Middle | MouseButton::Right
                ) {
                    match state {
                        ElementState::Pressed if self.mouse_button.is_none() => {
                            if let Some(pos) = self.cursor {
                                self.mouse_button = Some(button);
                                self.press_pointer(view, pos, button, now);
                            }
                        }
                        ElementState::Released if self.mouse_button == Some(button) => {
                            self.mouse_button = None;
                            self.release_pointer(view, self.cursor, now);
                        }
                        _ => {}
                    }
                    self.update_cursor(view);
                    self.window.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => self.wheel(view, delta, now),
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    if code == KeyCode::Space {
                        self.space = event.state == ElementState::Pressed;
                        self.update_cursor(view);
                        self.window.request_redraw();
                    } else if event.state == ElementState::Pressed {
                        match code {
                            KeyCode::Equal | KeyCode::NumpadAdd => {
                                self.action(Action::ZoomIn, view, now)
                            }
                            KeyCode::Minus | KeyCode::NumpadSubtract => {
                                self.action(Action::ZoomOut, view, now)
                            }
                            KeyCode::Tab => {
                                self.hud
                                    .tab(view.size, view.dpi, self.modifiers.shift_key());
                                self.window.request_redraw();
                            }
                            KeyCode::Escape if !event.repeat => {
                                #[cfg(debug_assertions)]
                                if self.debug_pending.is_some() {
                                    self.action(Action::DebugCancel, view, now);
                                    return Ok(());
                                }
                                event_loop.exit();
                            }
                            KeyCode::Home if !event.repeat => self.action(Action::Home, view, now),
                            KeyCode::KeyU if !event.repeat => {
                                self.action(Action::Purchase, view, now)
                            }
                            KeyCode::Enter | KeyCode::NumpadEnter if !event.repeat => {
                                if let Some(action) = self.hud.focused_action() {
                                    self.action(action, view, now);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            WindowEvent::Touch(touch) => self.touch(view, touch, now),
            _ => {}
        }
        Ok(())
    }

    fn hud_hit(&self, view: View, pos: [f64; 2]) -> Option<Action> {
        self.hud.hit(view.size, view.dpi, pos)
    }

    fn tooltip_target(&self, view: View) -> Option<Action> {
        #[cfg(debug_assertions)]
        if self.debug_pending.is_some() {
            return None;
        }
        if self.space || self.mouse_button.is_some() || self.gesture != Gesture::Idle {
            return None;
        }
        self.hud.tooltip_action(view.size, view.dpi, self.cursor?)
    }

    fn clear_tooltip(&mut self) {
        if self.tooltip.take().is_some_and(|(_, at)| at.is_none()) {
            self.hud.set_tooltip(None);
            self.window.request_redraw();
        }
    }

    fn update_tooltip(&mut self, view: View, now: Instant) {
        let action = self.tooltip_target(view);
        if action != self.tooltip.map(|(action, _)| action) {
            self.clear_tooltip();
            self.tooltip = action.map(|action| (action, Some(now + TOOLTIP_DELAY)));
        }
    }

    fn target(&self, view: View, pos: [f64; 2]) -> Option<Target> {
        if self.hud_hit(view, pos).is_some() {
            return None;
        }
        let [x, y] = self.camera.hit_axes(view.size, pos)?;
        if let Some((coord, _)) = self.game.large_at(x.block, y.block) {
            return Some(Target::Large(coord));
        }
        let hit = Hit::from_axes(x, y)?;
        self.game.target_at(hit.x, hit.y, hit.cell)
    }

    fn tile_area(&self, view: View, pos: [f64; 2]) -> bool {
        self.camera.hit_axes(view.size, pos).is_some_and(|[x, y]| {
            self.game.large_at(x.block, y.block).is_some()
                || Hit::from_axes(x, y)
                    .is_some_and(|hit| self.game.unit_present(Coord::new(Level::A, hit.x, hit.y)))
        })
    }

    fn hover_state(&self, view: View) -> (Option<Action>, Option<Hit>) {
        let Some(pos) = self.cursor else {
            return (None, None);
        };
        let hud = self.hud_hit(view, pos);
        if hud.is_some()
            || self.space
            || matches!(self.gesture, Gesture::Pan { .. } | Gesture::Void)
        {
            return (hud, None);
        }
        let hit = self.camera.hit(view.size, pos).or_else(|| {
            let [x, y] = self.camera.hit_axes(view.size, pos)?;
            self.game.large_at(x.block, y.block)?;
            Some(Hit {
                x: x.block,
                y: y.block,
                cell: _8times8::CellIndex::new(0).unwrap(),
            })
        });
        (None, hit)
    }

    fn update_cursor(&self, view: View) {
        let icon = if matches!(self.gesture, Gesture::Pan { .. }) {
            CursorIcon::Grabbing
        } else if let Some(pos) = self.cursor {
            match self.hud_hit(view, pos) {
                Some(Action::Block) => CursorIcon::Default,
                Some(_) => CursorIcon::Pointer,
                None if self.space || !self.tile_area(view, pos) => CursorIcon::Grab,
                None => CursorIcon::Default,
            }
        } else {
            CursorIcon::Default
        };
        self.window.set_cursor(icon);
    }

    fn press_pointer(&mut self, view: View, pos: [f64; 2], button: MouseButton, now: Instant) {
        self.hud.set_focus(None);
        if let Some(action) = self.hud_hit(view, pos) {
            self.gesture = if button == MouseButton::Left {
                Gesture::Hud { action, start: pos }
            } else {
                Gesture::Void
            };
            return;
        }
        self.advance_camera(now);
        self.tween = None;
        self.gesture = if button != MouseButton::Left || self.space || !self.tile_area(view, pos) {
            Gesture::Pan { last: pos }
        } else {
            Gesture::Press {
                start: pos,
                at: now,
            }
        };
    }

    fn move_pointer(&mut self, view: View, pos: [f64; 2], now: Instant) {
        match self.gesture {
            Gesture::Hud { start, .. } if moved(start, pos, view.dpi) => {
                self.gesture = Gesture::Void
            }
            Gesture::Press { start, .. } if moved(start, pos, view.dpi) => {
                self.gesture = Gesture::Void;
                if self.game.has_drag_item() {
                    self.gesture = Gesture::Paint {
                        pos: start,
                        last: None,
                        next: now,
                    };
                    self.try_paint(view, now);
                    self.move_pointer(view, pos, now);
                }
            }
            Gesture::Paint {
                pos: ref mut previous,
                ..
            } => {
                *previous = pos;
                self.try_paint(view, now);
            }
            Gesture::Pan { last } => {
                self.gesture = Gesture::Pan { last: pos };
                if self.camera.pan(view, last, pos) {
                    self.camera_changed(now);
                }
            }
            _ => {}
        }
    }

    fn release_pointer(&mut self, view: View, pos: Option<[f64; 2]>, now: Instant) {
        let old = std::mem::replace(&mut self.gesture, Gesture::Idle);
        let Some(pos) = pos else { return };
        match old {
            Gesture::Hud { action, start }
                if !moved(start, pos, view.dpi) && self.hud_hit(view, pos) == Some(action) =>
            {
                self.action(action, view, now);
            }
            Gesture::Press { start, at }
                if now.saturating_duration_since(at) < LONG_PRESS
                    && !moved(start, pos, view.dpi)
                    && self.hud_hit(view, pos).is_none() =>
            {
                if let Some(target) = self.target(view, start) {
                    self.work(target, false, now);
                }
            }
            _ => {}
        }
    }

    fn try_paint(&mut self, view: View, now: Instant) {
        if let Gesture::Paint { pos, last, next } = self.gesture
            && now >= next
            && let Some(target) = self.target(view, pos)
            && Some(target) != last
        {
            self.gesture = Gesture::Paint {
                pos,
                last: Some(target),
                next: now + PAINT_GAP,
            };
            self.work(target, true, now);
        }
    }

    fn touch(&mut self, view: View, touch: Touch, now: Instant) {
        let pos = [touch.location.x, touch.location.y];
        self.cursor = None;
        match touch.phase {
            TouchPhase::Started if self.touches.len() < 8 => {
                self.touches.push((touch.id, pos));
                if self.touches.len() == 1 {
                    self.mouse_button = None;
                    self.press_pointer(view, pos, MouseButton::Left, now);
                } else if self.touches.len() == 2 {
                    let first_blocked = matches!(self.gesture, Gesture::Hud { .. });
                    self.gesture = Gesture::Void;
                    if !first_blocked && self.hud_hit(view, pos).is_none() {
                        let (center, distance) = self.two_fingers();
                        self.advance_camera(now);
                        self.tween = None;
                        self.pinch = self.camera.world_at(view.size, center).map(|anchor| Pinch {
                            anchor,
                            distance,
                            scale: self.camera.scale,
                        });
                    }
                }
            }
            TouchPhase::Moved => {
                let Some((_, location)) = self.touches.iter_mut().find(|(id, _)| *id == touch.id)
                else {
                    return;
                };
                *location = pos;
                if let Some(pinch) = self.pinch
                    && self.touches.len() >= 2
                {
                    let (center, distance) = self.two_fingers();
                    let before = self.camera;
                    self.camera.anchor(
                        view,
                        pinch.anchor,
                        center,
                        pinch.scale * distance / pinch.distance,
                    );
                    if self.camera != before {
                        self.camera_changed(now);
                    }
                } else if self.touches.len() == 1 {
                    self.move_pointer(view, pos, now);
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let Some(i) = self.touches.iter().position(|(id, _)| *id == touch.id) else {
                    return;
                };
                self.touches.remove(i);
                if self.pinch.is_some() {
                    if self.touches.len() >= 2 {
                        let (center, distance) = self.two_fingers();
                        self.pinch = self.camera.world_at(view.size, center).map(|anchor| Pinch {
                            anchor,
                            distance,
                            scale: self.camera.scale,
                        });
                    } else {
                        self.pinch = None;
                    }
                }
                if self.touches.is_empty() {
                    self.release_pointer(
                        view,
                        (touch.phase == TouchPhase::Ended).then_some(pos),
                        now,
                    );
                }
            }
            _ => {}
        }
        self.window.request_redraw();
    }

    fn two_fingers(&self) -> ([f64; 2], f64) {
        let [x, y] = self.touches[0].1;
        let [u, v] = self.touches[1].1;
        (
            [(x + u) * 0.5, (y + v) * 0.5],
            (x - u).hypot(y - v).max(1.0),
        )
    }

    fn wheel(&mut self, view: View, delta: MouseScrollDelta, now: Instant) {
        let y = logical_wheel_y(delta, view.dpi);
        let pos = self.cursor.unwrap_or_else(|| screen_center(view.size));
        if self.hud.scroll(view.size, view.dpi, pos, y) {
            self.window.request_redraw();
            return;
        }
        if !y.is_finite() || y == 0.0 || !matches!(self.gesture, Gesture::Idle) {
            return;
        }
        self.advance_camera(now);
        let goal_scale = self
            .tween
            .take()
            .map_or(self.camera.scale, |tween| tween.to.scale);
        let factor = wheel_zoom_factor(delta, view.dpi, self.modifiers.control_key()) * goal_scale
            / self.camera.scale;
        if self.camera.zoom_at(view, pos, factor) {
            self.camera_changed(now);
        }
        self.update_cursor(view);
    }

    fn camera_changed(&mut self, now: Instant) {
        self.camera_touched = true;
        self.schedule_save(now);
        self.window.request_redraw();
    }

    fn advance_camera(&mut self, now: Instant) {
        if let Some(tween) = &self.tween {
            let (camera, done) = tween.sample(now);
            self.camera = camera;
            if done {
                self.tween = None;
                self.schedule_save(now);
            }
        }
    }

    fn zoom_button(&mut self, view: View, factor: f64, now: Instant) {
        self.advance_camera(now);
        let goal_scale = self
            .tween
            .as_ref()
            .map_or(self.camera.scale, |t| t.to.scale);
        let mut to = self.camera;
        to.scale = view.clamp_scale(goal_scale * factor);
        to.settle(view);
        self.tween_to(to, if to.at_minimum(view) { 320 } else { 220 }, now);
    }

    fn tween_to(&mut self, to: Camera, milliseconds: u64, now: Instant) {
        if to == self.camera {
            return;
        }
        self.camera_touched = true;
        self.tween = Some(Tween {
            from: self.camera,
            to,
            start: now,
            duration: Duration::from_millis(milliseconds),
        });
        self.schedule_save(now);
        self.window.request_redraw();
    }

    fn action(&mut self, action: Action, view: View, now: Instant) {
        #[cfg(debug_assertions)]
        if self.debug_pending.is_some()
            && !matches!(action, Action::DebugConfirm | Action::DebugCancel)
        {
            return;
        }
        let changed = match action {
            Action::Shop => {
                self.hud.toggle_shop();
                false
            }
            #[cfg(debug_assertions)]
            Action::Debug => {
                self.hud.toggle_debug();
                false
            }
            #[cfg(debug_assertions)]
            Action::DebugCommand(command) => {
                if matches!(
                    command,
                    DebugAction::Reset
                        | DebugAction::JumpItem
                        | DebugAction::JumpBig
                        | DebugAction::PasteSave
                ) {
                    self.debug_pending = Some(command);
                    self.hud.set_debug_pending(Some(command));
                } else {
                    self.run_debug(command, view, now);
                }
                false
            }
            #[cfg(debug_assertions)]
            Action::DebugConfirm => {
                let pending = self.debug_pending.take();
                self.hud.set_debug_pending(None);
                if let Some(command) = pending {
                    self.run_debug(command, view, now);
                }
                false
            }
            #[cfg(debug_assertions)]
            Action::DebugCancel => {
                self.debug_pending = None;
                self.hud.set_debug_pending(None);
                false
            }
            Action::Purchase => self.game.try_upgrade(),
            Action::MachineBody(tier) => tier
                .checked_sub(1)
                .and_then(|i| Level::ALL.get(i))
                .is_some_and(|&level| {
                    self.machines
                        .buy(&mut self.game, level, self.camera.world_center(), now)
                }),
            Action::MachineUpgrade(_, id) => self.machines.upgrade(&mut self.game, id),
            Action::Theme => {
                self.theme = Theme::from_dark(self.theme == Theme::Light);
                self.theme_chosen = true;
                self.schedule_save(now);
                false
            }
            Action::Sound => {
                self.sound = !self.sound;
                self.audio.set_enabled(self.sound);
                if self.sound {
                    self.audio.flip();
                }
                self.hud.set_sound(Some(self.sound));
                self.schedule_save(now);
                false
            }
            Action::ZoomIn => {
                self.zoom_button(view, ZOOM_FACTOR, now);
                false
            }
            Action::ZoomOut => {
                self.zoom_button(view, 1.0 / ZOOM_FACTOR, now);
                false
            }
            Action::Home => {
                self.advance_camera(now);
                self.tween_to(Camera::centered(view), 520, now);
                false
            }
            Action::Block => false,
        };
        if changed {
            self.audio.completed();
            self.update_hud();
            self.schedule_save(now);
        }
        self.window.request_redraw();
    }

    #[cfg(debug_assertions)]
    fn run_debug(&mut self, command: DebugAction, view: View, now: Instant) {
        use DebugAction::*;
        let success = match command {
            Reset | JumpItem | JumpBig => {
                self.game = match command {
                    JumpItem => Game::debug_before_item(),
                    JumpBig => Game::debug_after_1d(),
                    _ => Game::new(),
                };
                self.machines = Machines::default();
                self.reset_transient();
                self.hide_hint(now);
                let view = self.view();
                self.camera = Camera::centered(view);
                self.camera_touched = command != Reset;
                match command {
                    JumpItem => {
                        self.camera.center[0].block = 92;
                        self.camera.scale = view.clamp_scale(1.2 * WORLD_STEP * view.dpi);
                    }
                    JumpBig => {
                        self.camera.center = [Axis {
                            block: 0,
                            offset: 0.0,
                        }; 2];
                        self.camera.scale = view.clamp_scale(0.004 * WORLD_STEP * view.dpi);
                    }
                    _ => {}
                }
                self.camera.constrain(view);
                Ok("変更しました")
            }
            Grant(level, amount) => self
                .game
                .add_debug_bonus(level, &Amount::from(amount))
                .then_some("変更しました")
                .ok_or("実行できませんでした"),
            ClearBonus => {
                self.game.clear_debug_bonus();
                Ok("変更しました")
            }
            ManualMax => {
                self.game.set_manual_stage(16);
                Ok("変更しました")
            }
            ManualZero => {
                self.game.set_manual_stage(0);
                Ok("変更しました")
            }
            Pause => {
                self.machine_paused = !self.machine_paused;
                self.machines.set_paused(self.machine_paused);
                Ok("変更しました")
            }
            Speed => {
                self.machine_speed = if self.machine_speed == 1 { 10 } else { 1 };
                self.machines.set_speed(f64::from(self.machine_speed));
                Ok("変更しました")
            }
            ItemGet => {
                if let Some(unit) = self.game.item() {
                    self.effects.item_picked_up(unit, now);
                    self.audio.item();
                }
                self.game.set_drag_item(true);
                Ok("変更しました")
            }
            ItemReset => {
                self.game.set_drag_item(false);
                Ok("変更しました")
            }
            CopySave => self.copy_save(),
            PasteSave => self.paste_save(view, now),
        };
        if let Err(message) = success {
            eprintln!("{NAME}: {message}");
        }
        if success.is_ok() && !matches!(command, CopySave | Pause | Speed) {
            self.update_hud();
            self.schedule_save(now);
        }
        self.hud
            .set_debug_machine_state(self.machine_paused, self.machine_speed);
        self.hud
            .set_debug_feedback(Some(success.unwrap_or_else(|message| message).into()));
    }

    #[cfg(debug_assertions)]
    fn reset_transient(&mut self) {
        self.machine_paused = false;
        self.machine_speed = 1;
        self.effects = Effects::default();
        self.tween = None;
        self.gesture = Gesture::Idle;
        self.hint_shown = false;
        self.hint_flips = 0;
        self.hud.set_hint(false);
    }

    #[cfg(debug_assertions)]
    fn clipboard(&mut self) -> Result<&mut arboard::Clipboard, &'static str> {
        if self.clipboard.is_none() {
            self.clipboard = Some(arboard::Clipboard::new().map_err(|err| {
                eprintln!("{NAME}: clipboard unavailable: {err}");
                "クリップボードを利用できません"
            })?);
        }
        Ok(self.clipboard.as_mut().expect("clipboard was initialized"))
    }

    #[cfg(debug_assertions)]
    fn copy_save(&mut self) -> Result<&'static str, &'static str> {
        let text = save::to_clipboard(&self.snapshot()).map_err(|err| {
            eprintln!("{NAME}: clipboard encoding failed: {err}");
            "コピーできませんでした"
        })?;
        self.clipboard()?.set_text(text).map_err(|err| {
            eprintln!("{NAME}: clipboard write failed: {err}");
            "コピーできませんでした"
        })?;
        Ok("セーブデータをコピーしました")
    }

    #[cfg(debug_assertions)]
    fn paste_save(&mut self, view: View, now: Instant) -> Result<&'static str, &'static str> {
        if self.saves.is_none() {
            eprintln!("{NAME}: save paste refused because autosave is disabled");
            return Err("セーブデータを読み込めませんでした");
        }
        let text = self.clipboard()?.get_text().map_err(|err| {
            eprintln!("{NAME}: clipboard read failed: {err}");
            "セーブデータを読み込めませんでした"
        })?;
        let saved = if text.trim_start().starts_with('{') {
            legacy::decode(&text, now)
        } else {
            save::from_clipboard(&text)
        };
        let decoded = saved.and_then(|saved| {
            let (game, machines) = decode_components(&saved, now)?;
            Ok((saved, game, machines))
        });
        let (saved, game, machines) = decoded.map_err(|err| {
            eprintln!("{NAME}: rejected pasted save: {err}");
            "セーブデータを読み込めませんでした"
        })?;
        self.game = game;
        self.machines = machines;
        self.reset_transient();
        self.theme_chosen = saved.dark.is_some();
        self.theme = saved
            .dark
            .map_or_else(|| Theme::system(&self.window), Theme::from_dark);
        self.audio.set_enabled(saved.sound);
        self.sound = saved.sound;
        self.hud.set_sound(Some(saved.sound));
        self.hint_seen = saved.hint_seen;
        self.hint_at = (!self.hint_seen).then_some(now + HINT_DELAY);
        let view = View {
            bounds: self.game.bounds(),
            ..view
        };
        self.camera_touched = saved.camera.is_some();
        self.camera = Camera::centered(view);
        if let Some((blocks, offsets, scale)) = saved.camera {
            self.camera.center = std::array::from_fn(|i| Axis {
                block: blocks[i],
                offset: offsets[i],
            });
            self.camera.scale = scale * view.dpi;
            self.camera.constrain(view);
        }
        Ok("セーブデータを貼り付けました")
    }

    fn update_hud(&mut self) {
        self.hud.update(&self.game);
        self.hud
            .set_machine_rows(machine_rows(&self.game, &self.machines));
    }

    fn work(&mut self, target: Target, painting: bool, now: Instant) {
        let budget = self.game.manual_power();
        let units = match target {
            Target::Tile { .. } => 1,
            Target::Large(coord) => coord.level().tile_capacity(),
        };
        let mut work = Work::new(
            &self.game,
            self.camera,
            self.window.inner_size(),
            now,
            (budget as f64 / units as f64).clamp(1.0, 64.0),
        );
        if self
            .game
            .apply_with_events(target, budget, |event| work.event(&mut self.effects, event))
        {
            let flipped = work.flipped;
            self.after_work(work, if painting { 0.8 } else { 1.0 }, now);
            self.hint_flips = self.hint_flips.saturating_add(flipped);
            if !self.hint_seen && self.hint_flips >= 8 {
                self.hide_hint(now);
            }
        }
    }

    fn after_work(&mut self, work: Work, volume: f32, now: Instant) {
        self.audio.flip_with_volume(volume);
        if work.flipped > 1 {
            let ticks = (work.flipped - 1).min(4) as usize;
            self.audio.wave(
                Duration::from_secs_f64(work.delay.min(600.0) / 1000.0),
                ticks,
            );
        }
        if work.completed {
            self.audio.completed();
        }
        let item = self.game.item();
        if work.item != item
            && let Some(unit) = item
        {
            self.effects.item_appeared(unit, now);
        }
        if !work.held && self.game.has_drag_item() {
            if let Some(unit) = work.item {
                self.effects.item_picked_up(unit, now);
            }
            self.audio.item();
        }
        self.update_hud();
        self.schedule_save(now);
        self.window.request_redraw();
    }

    fn hide_hint(&mut self, now: Instant) {
        self.hint_seen = true;
        self.hint_shown = false;
        self.hint_at = None;
        self.hud.set_hint(false);
        self.schedule_save(now);
        self.window.request_redraw();
    }

    pub(super) fn idle(&mut self) -> Option<Instant> {
        let now = Instant::now();
        let view = self.view();
        if let Some((action, Some(at))) = self.tooltip
            && at <= now
        {
            if self.tooltip_target(view) == Some(action) {
                self.tooltip = Some((action, None));
                self.hud.set_tooltip(Some(action));
                self.window.request_redraw();
            } else {
                self.clear_tooltip();
            }
        }
        if self.hint_at.is_some_and(|at| at <= now) {
            if self.hint_shown {
                self.hide_hint(now);
            } else {
                self.hint_shown = true;
                self.hint_at = Some(now + HINT_DURATION);
                self.hud.set_hint(true);
                self.window.request_redraw();
            }
        }
        if self
            .machines
            .deadline()
            .is_some_and(|deadline| deadline <= now)
        {
            let mut work = Work::new(&self.game, self.camera, view.size, now, 8.0);
            if self
                .machines
                .tick_with_events(&mut self.game, now, |event| {
                    work.event(&mut self.effects, event)
                })
            {
                self.after_work(work, 0.45, now);
            }
            self.window.request_redraw();
        }
        self.try_paint(view, now);
        if self.save_at.is_some_and(|at| at <= now) {
            self.submit_save();
        }
        if self.retry_at.is_some_and(|at| at <= now) {
            self.retry_at = None;
            self.window.request_redraw();
        }
        let effects = self.effects.advance(now);
        let drawing = !self.occluded && view.size.width != 0 && view.size.height != 0;
        let active = drawing
            && (effects
                || self.tween.is_some()
                || self.machines.moving(now)
                || board::item_visible(self.camera, view.size, &self.game));
        if active {
            if self.frame_at.is_none_or(|at| at <= now) {
                self.window.request_redraw();
                self.frame_at = Some(now + FRAME_TIME);
            }
        } else {
            if self.frame_at.take().is_some() && drawing {
                self.window.request_redraw();
            }
            self.advance_camera(now);
        }
        let next_paint = match self.gesture {
            Gesture::Paint { next, .. } if next > now => Some(next),
            _ => None,
        };
        [
            self.frame_at,
            self.hint_at,
            self.retry_at,
            self.save_at,
            self.machines.deadline(),
            next_paint,
            self.tooltip.and_then(|(_, at)| at),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn redraw(&mut self, view: View, now: Instant) -> Result<(), String> {
        self.retry_at = None;
        if self.occluded || view.size.width == 0 || view.size.height == 0 {
            return Ok(());
        }
        self.advance_camera(now);
        let hover = self.hover_state(view).1;
        self.instances.clear();
        let mut painter = Painter {
            output: &mut self.instances,
            size: view.size,
            limit: MAX_INSTANCES - HUD_INSTANCE_RESERVE,
        };
        if self.effects.advance(now) || self.game.item().is_some() {
            board::paint_animated(
                &mut painter,
                self.camera,
                &self.game,
                hover,
                self.theme,
                &self.effects,
                now,
            );
        } else {
            board::paint(&mut painter, self.camera, &self.game, hover, self.theme);
        }
        paint_machines(&mut painter, &self.machines, self.camera, now, self.theme);
        painter.limit = MAX_INSTANCES;
        self.hud
            .paint(&mut painter, &self.game, self.theme, view.dpi, self.cursor);
        match self.renderer.draw(
            &self.window,
            view.size,
            board::background(self.theme),
            &self.instances,
        )? {
            Frame::Presented => self.retries = 0,
            Frame::Hidden => {}
            Frame::Retry(reason) => match self.retries {
                0..=2 => {
                    self.retries += 1;
                    self.retry_at = Some(now + Duration::from_millis(80));
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

    fn schedule_save(&mut self, now: Instant) {
        if self.saves.is_some() && self.save_at.is_none() {
            self.save_at = Some(now + SAVE_DELAY);
        }
    }

    fn submit_save(&mut self) {
        if self.save_at.take().is_none() {
            return;
        }
        if let Some(saves) = &self.saves {
            saves.submit(self.snapshot());
        }
    }

    fn snapshot(&self) -> Saved {
        Saved {
            board: self.game.encode(),
            machines: self.machines.encode(),
            dark: self.theme_chosen.then_some(self.theme == Theme::Dark),
            sound: self.sound,
            hint_seen: self.hint_seen,
            camera: self.camera_touched.then_some((
                self.camera.center.map(|axis| axis.block),
                self.camera.center.map(|axis| axis.offset),
                self.camera.scale / self.camera_dpi,
            )),
        }
    }

    pub(super) fn flush(&mut self) {
        if self.tween.is_some() {
            self.schedule_save(Instant::now());
        }
        self.submit_save();
        drop(self.saves.take());
    }
}

fn logical_wheel_y(delta: MouseScrollDelta, dpi: f64) -> f64 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => f64::from(y) * WHEEL_LINE_PIXELS,
        MouseScrollDelta::PixelDelta(p) => p.y / dpi,
    }
}

fn wheel_zoom_factor(delta: MouseScrollDelta, dpi: f64, control: bool) -> f64 {
    let y = logical_wheel_y(delta, dpi).clamp(-WHEEL_PIXEL_CAP, WHEEL_PIXEL_CAP);
    let coefficient = if control {
        FAST_PIXEL_ZOOM_COEFFICIENT
    } else {
        PIXEL_ZOOM_COEFFICIENT
    };
    match delta {
        // Ctrl keeps the same gain in log-zoom space for both kinds of wheel input.
        MouseScrollDelta::LineDelta(..) => {
            WHEEL_ZOOM_FACTOR.powf((y / WHEEL_LINE_PIXELS) * (coefficient / PIXEL_ZOOM_COEFFICIENT))
        }
        MouseScrollDelta::PixelDelta(_) => (y * coefficient).exp(),
    }
}

fn moved(a: [f64; 2], b: [f64; 2], dpi: f64) -> bool {
    (a[0] - b[0]).hypot(a[1] - b[1]) >= DRAG_START * dpi
}

fn decode_components(saved: &Saved, now: Instant) -> Result<(Game, Machines), String> {
    let game = Game::decode(&saved.board).map_err(|err| format!("invalid board: {err}"))?;
    let machines =
        Machines::decode(&saved.machines, now).map_err(|err| format!("invalid machines: {err}"))?;
    Ok((game, machines))
}

fn machine_rows(game: &Game, machines: &Machines) -> Vec<MachineTier> {
    Level::ALL
        .into_iter()
        .filter(|&level| Machines::tier_unlocked(game, level))
        .map(|level| {
            let tier = level as usize + 1;
            let mut owned = 0;
            let mut legacy = 0;
            let balance = game.available(level);
            let rows = machines
                .entries()
                .filter(|&(_, tier, _, _)| tier == level)
                .map(|(id, _, stage, old)| {
                    if old {
                        legacy += 1;
                    } else {
                        owned += 1;
                    }
                    let exponent = 6 * (level as u32) + stage;
                    let cost = Amount::machine_price(stage);
                    MachineRow {
                        id,
                        current: hud::capacity_label(exponent),
                        next: cost.as_ref().map(|_| hud::capacity_label(exponent + 1)),
                        enabled: cost.as_ref().is_some_and(|amount| balance.covers(amount)),
                        cost: cost
                            .map_or_else(String::new, |amount| hud::points_label(amount, level)),
                    }
                })
                .collect();
            let body = (owned < MACHINES_PER_TIER).then(|| MachineRow {
                id: tier,
                current: hud::capacity_label(6 * (level as u32)),
                next: None,
                cost: hud::points_label(MACHINE_PRICE, level),
                enabled: balance.covers(&Amount::from(MACHINE_PRICE)),
            });
            MachineTier {
                tier,
                owned,
                limit: MACHINES_PER_TIER,
                legacy,
                body,
                machines: rows,
            }
        })
        .collect()
}

fn paint_machines(
    painter: &mut Painter<'_>,
    machines: &Machines,
    camera: Camera,
    now: Instant,
    theme: Theme,
) {
    const PIPS: [&[[f64; 2]]; 5] = [
        &[[0.0, 0.0]],
        &[[-1.0, -1.0], [1.0, 1.0]],
        &[[-1.0, -1.0], [0.0, 0.0], [1.0, 1.0]],
        &[[-1.0, -1.0], [1.0, -1.0], [-1.0, 1.0], [1.0, 1.0]],
        &[
            [-1.0, -1.0],
            [1.0, -1.0],
            [0.0, 0.0],
            [-1.0, 1.0],
            [1.0, 1.0],
        ],
    ];
    let size = painter.size();
    for ((id, level, stage, _), (_, pos)) in machines.entries().zip(machines.views(now)) {
        let center = camera.project(size, pos);
        let exponent = f64::from(6 * (level as u32) + stage.min(52));
        let base = (14.0 / WORLD_STEP * 2.0f64.powf(exponent * 0.5) * camera.scale).max(6.0);
        let half = base * machines.pulse_scale(id, now);
        let border = if theme == Theme::Dark {
            [185, 190, 199]
        } else {
            [120, 127, 137]
        };
        painter.diamond(center, half + 0.75, border);
        painter.diamond(center, half, [181, 187, 196]);
        painter.diamond(center, half * 0.78, [147, 155, 168]);
        painter.diamond(center, half * 0.74, [235, 237, 241]);
        let spread = half * 0.222;
        let radius = (half * 0.095).max(0.55);
        for &[u, v] in PIPS[level as usize] {
            let x = center[0] + (u - v) * spread;
            let y = center[1] + (u + v) * spread;
            painter.rounded(
                x - radius,
                y - radius,
                2.0 * radius,
                2.0 * radius,
                radius,
                [30, 32, 36],
            );
        }
    }
}

struct Work {
    viewport: [[Axis; 2]; 2],
    now: Instant,
    step: f64,
    delay: f64,
    animated: usize,
    effects: usize,
    flipped: u64,
    completed: bool,
    item: Option<Coord>,
    held: bool,
}

impl Work {
    fn new(game: &Game, camera: Camera, size: PhysicalSize<u32>, now: Instant, n: f64) -> Self {
        let fallback = [camera.center; 2];
        let viewport = camera
            .world_at(size, [0.0, 0.0])
            .zip(camera.world_at(size, [f64::from(size.width), f64::from(size.height)]))
            .map_or(fallback, |(low, high)| [low, high]);
        Self {
            viewport,
            now,
            step: (560.0 / (n - 1.0).max(1.0)).min(55.0),
            delay: 0.0,
            animated: 0,
            effects: 0,
            flipped: 0,
            completed: false,
            item: game.item(),
            held: game.has_drag_item(),
        }
    }

    fn visible(&self, coord: Coord) -> bool {
        coord.bounds_in(Level::A).is_some_and(|bounds| {
            let [[left, top], [right, bottom]] = self.viewport;
            bounds.min().x() <= right.block
                && bounds.max().x() >= left.block
                && bounds.min().y() <= bottom.block
                && bounds.max().y() >= top.block
        })
    }

    fn flip_at(&mut self, coord: Coord) -> Option<Instant> {
        self.flipped += 1;
        if self.animated >= ANIMATION_BUDGET || !self.visible(coord) {
            return None;
        }
        let at = self.now + Duration::from_secs_f64(self.delay.min(900.0) / 1000.0);
        self.delay += if self.animated < 64 { self.step } else { 2.0 };
        self.animated += 1;
        Some(at)
    }

    fn event(&mut self, effects: &mut Effects, event: BoardEvent) {
        match event {
            BoardEvent::Flipped(Target::Tile { x, y, cell }) => {
                let coord = Coord::new(Level::A, x, y);
                if let Some(at) = self.flip_at(coord) {
                    effects.flip_tiles(coord, 1 << cell.index(), at);
                }
            }
            BoardEvent::Flipped(Target::Large(_)) => self.flipped += 1,
            BoardEvent::Completed { coord, was_large } => {
                self.completed = true;
                if was_large && let Some(at) = self.flip_at(coord) {
                    effects.flip_large(coord, at);
                }
                if self.effects < ANIMATION_BUDGET && self.visible(coord) {
                    self.effects += 1;
                    effects.completed(coord, self.now);
                }
            }
            BoardEvent::Opened(coord) if self.effects < ANIMATION_BUDGET && self.visible(coord) => {
                self.effects += 1;
                effects.opened(coord, self.now);
            }
            BoardEvent::Opened(_) => {}
        }
    }
}

#[derive(Default)]
struct SaveSlot {
    pending: Option<Saved>,
    closed: bool,
}

struct SaveWorker {
    slot: Arc<(Mutex<SaveSlot>, Condvar)>,
    worker: Option<JoinHandle<()>>,
}

impl SaveWorker {
    fn start() -> Option<Self> {
        let slot = Arc::new((Mutex::new(SaveSlot::default()), Condvar::new()));
        let pending = slot.clone();
        match thread::Builder::new()
            .name("8times8 save".into())
            .spawn(move || {
                let (m, c) = &*pending;
                let mut logged = None;
                loop {
                    let mut guard = c
                        .wait_while(m.lock().unwrap(), |slot| {
                            slot.pending.is_none() && !slot.closed
                        })
                        .unwrap();
                    let Some(saved) = guard.pending.take() else {
                        break;
                    };
                    drop(guard);
                    let error = save::store_validated(&saved, |state| {
                        decode_components(state, Instant::now()).map(|_| ())
                    })
                    .err();
                    if error != logged
                        && let Some(error) = &error
                    {
                        eprintln!("{NAME}: could not autosave: {error}");
                    }
                    logged = error;
                }
            }) {
            Ok(worker) => Some(Self {
                slot,
                worker: Some(worker),
            }),
            Err(error) => {
                eprintln!("{NAME}: could not start autosave: {error}");
                None
            }
        }
    }

    fn submit(&self, latest: Saved) {
        let (m, c) = &*self.slot;
        m.lock().unwrap().pending = Some(latest);
        c.notify_one();
    }
}

impl Drop for SaveWorker {
    fn drop(&mut self) {
        let (m, c) = &*self.slot;
        m.lock().unwrap().closed = true;
        c.notify_one();
        if self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            eprintln!("{NAME}: autosave worker stopped unexpectedly");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::PhysicalPosition;

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-12 * expected.abs().max(1.0),
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn wheel_notches_multiply_camera_scale_and_reverse_at_the_cursor() {
        let view = View {
            size: PhysicalSize::new(800, 600),
            dpi: 1.0,
            bounds: [-10, -10, 10, 10],
        };
        let pos = [320.0, 210.0];
        let mut camera = Camera::centered(view);
        let start = camera.scale;
        let anchor = camera.world_at(view.size, pos).unwrap();
        let factor =
            |notches| wheel_zoom_factor(MouseScrollDelta::LineDelta(0.0, notches), view.dpi, false);
        let assert_anchor = |camera: Camera| {
            let current = camera.world_at(view.size, pos).unwrap();
            for (before, after) in anchor.into_iter().zip(current) {
                assert_close(before.distance(after), 0.0);
            }
        };

        for notches in 1..=4 {
            assert!(camera.zoom_at(view, pos, factor(1.0)));
            assert_close(camera.scale, start * 1.2_f64.powi(notches));
            assert_anchor(camera);
        }
        for notches in (0..4).rev() {
            assert!(camera.zoom_at(view, pos, factor(-1.0)));
            assert_close(camera.scale, start * 1.2_f64.powi(notches));
            assert_anchor(camera);
        }
        assert_close(factor(4.0), factor(1.0).powi(4));
        assert_close(factor(-4.0), factor(-1.0).powi(4));
        assert_close(factor(4.0) * factor(-4.0), 1.0);
        assert_close(factor(1e6), factor(12.5));
        assert_close(factor(-1e6), factor(-12.5));
    }

    #[test]
    fn wheel_control_preserves_log_gain_for_notches() {
        let factor =
            |notches| wheel_zoom_factor(MouseScrollDelta::LineDelta(0.0, notches), 1.0, true);
        assert_close(factor(1.0), 1.2_f64.powf(0.01 / 0.0018));
        assert_close(factor(1.0) * factor(-1.0), 1.0);
    }

    #[test]
    fn wheel_pixels_keep_continuous_zoom_and_hud_scroll_units() {
        let dpi = 2.0;
        let delta = |y| MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, y));
        assert_close(
            logical_wheel_y(MouseScrollDelta::LineDelta(0.0, 1.0), dpi),
            16.0,
        );
        assert_close(logical_wheel_y(delta(2.5), dpi), 1.25);

        for (control, coefficient) in [(false, 0.0018), (true, 0.01)] {
            let factor = |y| wheel_zoom_factor(delta(y), dpi, control);
            assert_close(factor(2.5), (1.25_f64 * coefficient).exp());
            assert_close(factor(2.5).powi(2), factor(5.0));
            assert_close(factor(2.5) * factor(-2.5), 1.0);
            assert_close(factor(500.0), (200.0_f64 * coefficient).exp());
        }
    }
}
