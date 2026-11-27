use std::{
    collections::{HashMap, HashSet},
    f64::consts::{PI, TAU},
    time::{Duration, Instant},
};

use _8times8::{Amount, Block, Coord, Game, Level};
use winit::dpi::PhysicalSize;

use super::{Camera, Hit, Painter, SIDE, Theme};

// Geometry is specified in board units; one 1A pitch occupies SIDE camera units.
const FACE: f64 = 28.0;
const STEP: f64 = 31.0;
const FOOTPRINT: f64 = 245.0;
const PITCH: f64 = 253.0;
const GUTTER: f64 = PITCH - FOOTPRINT;
const RADIUS: f64 = 3.5;
const FRAME_PAD: f64 = 2.5;
const CELL_OFFSET: f64 = STEP * SIDE / PITCH;
const BLOCK_OFFSET: f64 = FOOTPRINT * SIDE / PITCH;
const NEIGHBORS: [(i64, i64); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];
const FLIP: Duration = Duration::from_millis(190);
const FADE: Duration = Duration::from_millis(700);
const HALO: Duration = Duration::from_millis(1_800);
const HALO_DELAY: Duration = Duration::from_millis(95);
const PULSE: Duration = Duration::from_millis(4_200);
const PULSE_DELAY: Duration = Duration::from_millis(490);
const ITEM_APPEAR: Duration = Duration::from_millis(1_200);
const ITEM_PICKUP: Duration = Duration::from_millis(1_600);
type Color = [u8; 3];

pub(super) fn cell_at_offset(offset: f64) -> Option<u8> {
    // The original assigns a small inter-tile gap to the preceding tile, but
    // the wider gutter between 1A blocks does not belong to either block.
    (0.0..BLOCK_OFFSET).contains(&offset).then(|| {
        (1..8)
            .take_while(|&col| offset >= f64::from(col) * CELL_OFFSET)
            .count() as u8
    })
}

pub(super) fn background(theme: Theme) -> Color {
    match theme {
        Theme::Light => [228, 228, 226],
        Theme::Dark => [42, 42, 44],
    }
}

struct Colors {
    bg: Color,
    white: Color,
    black: Color,
    ghost: Color,
    section: Color,
    separator: Color,
    halo: Color,
}

impl Colors {
    fn new(theme: Theme) -> Self {
        let bg = background(theme);
        // Pre-composite outline colors in display RGB, for the same result on
        // opaque software pixels and on an sRGB GPU surface.
        let tint = |rgb, alpha| mix(bg, rgb, alpha);
        match theme {
            Theme::Light => {
                let black = [46, 46, 48];
                Self {
                    bg,
                    white: [251, 251, 249],
                    black,
                    ghost: tint(black, 0.16),
                    section: tint(black, 0.14),
                    separator: tint(black, 0.5),
                    halo: black,
                }
            }
            Theme::Dark => {
                let white = [226, 226, 222];
                Self {
                    bg,
                    white,
                    black: [19, 19, 21],
                    ghost: tint(white, 0.12),
                    section: tint(white, 0.11),
                    separator: tint([150, 150, 146], 0.75),
                    halo: [200, 200, 196],
                }
            }
        }
    }
}

fn mix(base: Color, over: Color, alpha: f64) -> Color {
    std::array::from_fn(|i| {
        let base = f64::from(base[i]);
        (base + (f64::from(over[i]) - base) * alpha).round() as u8
    })
}

/// Just the board's transient visual state. Keeping it outside the game lets
/// a saved position restore immediately without replaying old effects.
pub(super) struct Effects {
    clock: Instant,
    tiles: HashMap<Coord, Vec<(u64, Instant)>>,
    large: HashMap<Coord, Instant>,
    opened: HashMap<Coord, Instant>,
    completed: HashMap<Coord, Instant>,
    item_appeared: Option<(Coord, Instant)>,
    item_picked_up: Option<(Coord, Instant)>,
}

impl Default for Effects {
    fn default() -> Self {
        Self {
            clock: Instant::now(),
            tiles: HashMap::new(),
            large: HashMap::new(),
            opened: HashMap::new(),
            completed: HashMap::new(),
            item_appeared: None,
            item_picked_up: None,
        }
    }
}

impl Effects {
    pub(super) fn flip_tiles(&mut self, coord: Coord, mask: u64, at: Instant) {
        if mask != 0 {
            self.tiles.entry(coord).or_default().push((mask, at));
        }
    }

    pub(super) fn flip_large(&mut self, coord: Coord, at: Instant) {
        self.large.insert(coord, at);
    }

    pub(super) fn opened(&mut self, coord: Coord, at: Instant) {
        self.opened.insert(coord, at);
    }

    pub(super) fn completed(&mut self, coord: Coord, at: Instant) {
        self.completed.insert(coord, at);
    }

    pub(super) fn item_appeared(&mut self, coord: Coord, at: Instant) {
        self.item_appeared = Some((coord, at));
    }

    pub(super) fn item_picked_up(&mut self, coord: Coord, at: Instant) {
        self.item_picked_up = Some((coord, at));
        self.item_appeared = None;
    }

    /// Discard expired effects and indicate whether to schedule another frame.
    pub(super) fn advance(&mut self, now: Instant) -> bool {
        self.tiles.retain(|_, masks| {
            masks.retain(|&(_, at)| now.saturating_duration_since(at) < FLIP);
            !masks.is_empty()
        });
        self.large
            .retain(|_, at| now.saturating_duration_since(*at) < FLIP);
        self.opened
            .retain(|_, at| now.saturating_duration_since(*at) < PULSE_DELAY + PULSE);
        self.completed
            .retain(|_, at| now.saturating_duration_since(*at) < HALO_DELAY + HALO);
        self.item_appeared = self
            .item_appeared
            .filter(|&(_, at)| now.saturating_duration_since(at) < ITEM_APPEAR);
        self.item_picked_up = self
            .item_picked_up
            .filter(|&(_, at)| now.saturating_duration_since(at) < ITEM_PICKUP);
        self.active()
    }

    fn active(&self) -> bool {
        !self.tiles.is_empty()
            || !self.large.is_empty()
            || !self.opened.is_empty()
            || !self.completed.is_empty()
            || self.item_appeared.is_some()
            || self.item_picked_up.is_some()
    }
}

#[derive(Clone, Copy)]
struct Motion<'a> {
    effects: &'a Effects,
    now: Instant,
}

impl<'a> Motion<'a> {
    fn before(self, at: Instant, duration: Duration) -> Option<f64> {
        let elapsed = self.now.saturating_duration_since(at);
        (elapsed < duration).then(|| elapsed.as_secs_f64() / duration.as_secs_f64())
    }

    fn after(self, at: Instant, delay: Duration, duration: Duration) -> Option<f64> {
        let elapsed = self.now.checked_duration_since(at)?.checked_sub(delay)?;
        (elapsed < duration).then(|| elapsed.as_secs_f64() / duration.as_secs_f64())
    }

    fn tile_flips(self, coord: Coord) -> impl Iterator<Item = (u64, f64)> + 'a {
        self.effects
            .tiles
            .get(&coord)
            .into_iter()
            .flatten()
            .filter_map(move |&(mask, when)| Some((mask, self.before(when, FLIP)?)))
    }

    fn large_flip(self, coord: Coord) -> Option<f64> {
        self.before(*self.effects.large.get(&coord)?, FLIP)
    }

    fn opened(self, coord: Coord) -> Option<f64> {
        self.before(*self.effects.opened.get(&coord)?, FADE)
            .map(ease_out)
    }
}

struct Style<'a> {
    colors: Colors,
    motion: Option<Motion<'a>>,
}

impl Style<'_> {
    fn face(&self, coord: Coord, black: bool) -> Color {
        self.fade(
            coord,
            if black {
                self.colors.black
            } else {
                self.colors.white
            },
        )
    }

    fn fade(&self, coord: Coord, color: Color) -> Color {
        self.motion
            .and_then(|motion| motion.opened(coord))
            .map_or(color, |opacity| mix(self.colors.bg, color, opacity))
    }

    fn flip(&self, mut face: Rect, progress: f64) -> (Rect, Color) {
        let eased = if progress < 0.5 {
            2.0 * progress * progress
        } else {
            1.0 - 2.0 * (1.0 - progress) * (1.0 - progress)
        };
        let height = face.h * (0.12 + 0.88 * (PI * eased).cos().abs());
        face.top += (face.h - height) * 0.5;
        face.h = height;
        face.radius = face.radius.min(height * 0.5);
        let color = mix(
            self.colors.white,
            self.colors.black,
            ((eased - 0.3) / 0.4).clamp(0.0, 1.0),
        );
        (face, color)
    }
}

fn ease_out(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Rect {
    left: f64,
    top: f64,
    w: f64,
    h: f64,
    radius: f64,
}

impl Rect {
    fn square([left, top]: [f64; 2], side: f64, radius: f64) -> Self {
        Self {
            left,
            top,
            w: side,
            h: side,
            radius,
        }
    }

    fn padded(self, pad: f64) -> Self {
        Self {
            left: self.left - pad,
            top: self.top - pad,
            w: self.w + 2.0 * pad,
            h: self.h + 2.0 * pad,
            radius: self.radius,
        }
    }

    fn contains(self, [x, y]: [f64; 2]) -> bool {
        let radius = self.radius.min(self.w * 0.5).min(self.h * 0.5).max(0.0);
        if self.w <= 0.0
            || self.h <= 0.0
            || x < self.left
            || x > self.left + self.w
            || y < self.top
            || y > self.top + self.h
        {
            return false;
        }
        let dx = ((x - self.left - self.w * 0.5).abs() - (self.w * 0.5 - radius)).max(0.0);
        let dy = ((y - self.top - self.h * 0.5).abs() - (self.h * 0.5 - radius)).max(0.0);
        dx * dx + dy * dy <= radius * radius
    }

    fn fill(self, p: &mut Painter<'_>, color: Color) {
        if self.radius == 0.0 {
            p.rect(self.left, self.top, self.w, self.h, color);
        } else {
            p.rounded(self.left, self.top, self.w, self.h, self.radius, color);
        }
    }

    fn outline(self, p: &mut Painter<'_>, width: f64, color: Color) {
        p.outline(
            self.left,
            self.top,
            self.w,
            self.h,
            self.radius,
            width,
            color,
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Detail {
    coarse: bool,
    rounded: bool,
    fine_done: bool,
}

impl Detail {
    fn at(face_px: f64) -> Self {
        Self {
            coarse: face_px < 4.0,
            rounded: face_px >= 10.0,
            fine_done: face_px >= 12.0,
        }
    }
}

fn span(level: Level) -> i64 {
    level.span_in(Level::A).expect("level is at least A") as i64
}

fn grain(level: Level, pitch_px: f64) -> Level {
    Level::ALL
        .into_iter()
        .find(|&l| l == level || span(l) as f64 * pitch_px >= 10.0)
        .expect("grain is no coarser than level")
}

// Unit boundaries can straddle i64::MIN or MAX. Keep origins wide until we
// project relative to the camera; adding a small offset then remains precise.
fn unit_start(index: i64, span: i64) -> i128 {
    i128::from(index) * i128::from(span) - i128::from(4 * (span - 1) / 7)
}

fn clamp_axis(axis: i128) -> i64 {
    axis.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn first_block(coord: Coord) -> [i64; 2] {
    let span = span(coord.level());
    [coord.x(), coord.y()].map(|axis| clamp_axis(unit_start(axis, span)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Area {
    min: [i64; 2],
    max: [i64; 2],
}

impl Area {
    fn at_level(self, level: Level) -> Self {
        let ancestor = |[x, y]: [i64; 2]| {
            let c = Coord::new(Level::A, x, y)
                .ancestor_at(level)
                .expect("1A has this ancestor");
            [c.x(), c.y()]
        };
        Self {
            min: ancestor(self.min),
            max: ancestor(self.max),
        }
    }

    fn around(coord: Coord, level: Level) -> Self {
        let span = coord.level().span_in(level).expect("contained level") as i64;
        let start = [coord.x(), coord.y()].map(|i| unit_start(i, span));
        Self {
            min: start.map(clamp_axis),
            max: start.map(|i| clamp_axis(i + i128::from(span - 1))),
        }
    }

    fn expanded(self, amount: i64) -> Self {
        Self {
            min: self.min.map(|i| i.saturating_sub(amount)),
            max: self.max.map(|i| i.saturating_add(amount)),
        }
    }

    fn intersect(self, other: Self) -> Option<Self> {
        let min = std::array::from_fn(|i| self.min[i].max(other.min[i]));
        let max = std::array::from_fn(|i| self.max[i].min(other.max[i]));
        (min[0] <= max[0] && min[1] <= max[1]).then_some(Self { min, max })
    }

    fn contains(self, coord: Coord) -> bool {
        (self.min[0]..=self.max[0]).contains(&coord.x())
            && (self.min[1]..=self.max[1]).contains(&coord.y())
    }

    fn coords(self, level: Level) -> impl Iterator<Item = Coord> {
        (self.min[1]..=self.max[1])
            .flat_map(move |y| (self.min[0]..=self.max[0]).map(move |x| Coord::new(level, x, y)))
    }
}

struct View {
    size: [f64; 2],
    camera: Camera,
    px: f64,
    pitch_px: f64,
    detail: Detail,
}

impl View {
    fn new(size: [f64; 2], camera: Camera) -> Self {
        let pitch_px = SIDE * camera.scale;
        let px = pitch_px / PITCH;
        Self {
            size,
            camera,
            px,
            pitch_px,
            detail: Detail::at(FACE * px),
        }
    }

    fn blocks(&self, padding_px: f64) -> Area {
        let bound = |i: usize, direction: f64| {
            let axis = self.camera.center[i];
            let reach = (self.size[i] * 0.5 + padding_px) / self.camera.scale;
            let relative = ((axis.offset + direction * reach) / SIDE).floor() as i64;
            axis.block.saturating_add(relative)
        };
        Area {
            min: std::array::from_fn(|i| bound(i, -1.0)),
            max: std::array::from_fn(|i| bound(i, 1.0)),
        }
    }

    fn origin(&self, coord: Coord) -> [f64; 2] {
        let span = span(coord.level());
        let axes = [coord.x(), coord.y()];
        std::array::from_fn(|i| {
            let center = self.camera.center[i];
            let difference = unit_start(axes[i], span) - i128::from(center.block);
            let camera_units = difference as f64 * SIDE - center.offset;
            camera_units.mul_add(self.camera.scale, self.size[i] * 0.5)
        })
    }

    fn footprint(&self, coord: Coord, radius: f64) -> Rect {
        let side = span(coord.level()) as f64 * self.pitch_px - GUTTER * self.px;
        Rect::square(self.origin(coord), side, radius * self.px)
    }

    fn visible(&self, rect: Rect) -> bool {
        rect.w > 0.0
            && rect.h > 0.0
            && rect.left < self.size[0]
            && rect.top < self.size[1]
            && rect.left + rect.w > 0.0
            && rect.top + rect.h > 0.0
    }

    fn fill(&self, p: &mut Painter<'_>, rect: Rect, color: Color) {
        if self.visible(rect) {
            rect.fill(p, color);
        }
    }

    fn outline(&self, p: &mut Painter<'_>, rect: Rect, width: f64, color: Color) {
        if self.outline_visible(rect, width) {
            rect.outline(p, width, color);
        }
    }

    fn outline_visible(&self, rect: Rect, width: f64) -> bool {
        if !self.visible(rect.padded(width * 0.5)) {
            return false;
        }
        // A huge section may enclose the screen with its entire stroke out of
        // view. Exclude that before handing it to either renderer.
        let inset = width * 0.5 + 1.0;
        let mut inner = rect.padded(-inset);
        inner.radius = (inner.radius - inset).max(0.0);
        let [w, h] = self.size;
        ![[0.0, 0.0], [w, 0.0], [0.0, h], [w, h]]
            .into_iter()
            .all(|point| inner.contains(point))
    }

    fn cells(&self, coord: Coord, coarse: bool, pad: f64) -> Cells {
        let origin = self.origin(coord);
        let stride = STEP * self.px;
        let side = if coarse { stride } else { FACE * self.px };
        let range = |i: usize| {
            let start = ((-origin[i] - pad - side) / stride).floor() + 1.0;
            let end = ((self.size[i] + pad - origin[i]) / stride).ceil();
            start.clamp(0.0, 8.0) as u8..end.clamp(0.0, 8.0) as u8
        };
        Cells {
            cols: range(0),
            rows: range(1),
            origin,
            stride,
            side,
            radius: if self.detail.rounded && !coarse {
                RADIUS * self.px
            } else {
                0.0
            },
        }
    }

    fn large(&self, coord: Coord, group: bool) -> Rect {
        let footprint = self.footprint(coord, 0.0);
        let cell_gap_ratio = (STEP - FACE) / STEP;
        let margin = footprint.w * cell_gap_ratio * 0.5;
        let mut face = footprint.padded(-margin);
        face.radius = face.w * (RADIUS / FACE);

        let pitch = span(coord.level()) as f64 * self.pitch_px;
        if group && pitch < 80.0 {
            // At a distance a wide outer gutter does the work of the separator.
            let gap_px = pitch * cell_gap_ratio;
            let target = (pitch * GUTTER / STEP).max(2.0);
            let inset = ((target - gap_px) * 0.5).min(face.w * 0.5);
            let slot = |axis: i64| (axis.rem_euclid(8) + 4) % 8;
            match slot(coord.x()) {
                0 => {
                    face.left += inset;
                    face.w -= inset;
                }
                7 => face.w -= inset,
                _ => {}
            }
            match slot(coord.y()) {
                0 => {
                    face.top += inset;
                    face.h -= inset;
                }
                7 => face.h -= inset,
                _ => {}
            }
            face.radius = face.radius.min(face.w * 0.5).min(face.h * 0.5);
        }
        face
    }
}

struct Cells {
    cols: std::ops::Range<u8>,
    rows: std::ops::Range<u8>,
    origin: [f64; 2],
    stride: f64,
    side: f64,
    radius: f64,
}

impl Cells {
    fn face(&self, col: u8, row: u8) -> Rect {
        Rect::square(
            [
                self.origin[0] + f64::from(col) * self.stride,
                self.origin[1] + f64::from(row) * self.stride,
            ],
            self.side,
            self.radius,
        )
    }
}

fn whole_block(mask: u64, detail: Detail) -> bool {
    (mask == 0 && detail.coarse) || (mask == u64::MAX && !detail.fine_done)
}

fn paint_block(p: &mut Painter<'_>, view: &View, coord: Coord, block: Block, style: &Style<'_>) {
    let mask = block.mask();
    let white = style.face(coord, false);
    let black = style.face(coord, true);
    if white == style.colors.bg && black == style.colors.bg {
        return;
    }
    let color = |flipped| if flipped { black } else { white };
    let turn = style
        .motion
        .filter(|motion| !view.detail.coarse && motion.effects.tiles.contains_key(&coord));
    let animating = turn.is_some_and(|motion| motion.tile_flips(coord).next().is_some());
    if whole_block(mask, view.detail) && !animating {
        let radius = if view.detail.rounded {
            2.0 * RADIUS
        } else {
            0.0
        };
        view.fill(p, view.footprint(coord, radius), color(mask != 0));
        return;
    }

    let cells = view.cells(coord, view.detail.coarse, 0.0);
    for row in cells.rows.clone() {
        let flipped = |col: u8| mask & (1u64 << (row * 8 + col)) != 0;
        let mut col = cells.cols.start;
        while col < cells.cols.end {
            let black = flipped(col);
            let mut face = cells.face(col, row);
            let index = row * 8 + col;
            col += 1;
            if view.detail.coarse {
                // Adjacent filled cells at this zoom have no gap; coalesce runs.
                let start = col - 1;
                while col < cells.cols.end && flipped(col) == black {
                    col += 1;
                }
                face.w = f64::from(col - start) * cells.stride;
            }
            if let Some(time) = turn.and_then(|motion| {
                motion
                    .tile_flips(coord)
                    .find_map(|(mask, time)| (mask & (1 << index) != 0).then_some(time))
            }) {
                let (face, ink) = style.flip(face, time);
                view.fill(p, face, style.fade(coord, ink));
            } else {
                view.fill(p, face, color(black));
            }
        }
    }
}

fn ghost_shapes(view: &View, coord: Coord, mut emit: impl FnMut(Rect)) {
    if view.detail.coarse {
        emit(view.footprint(coord, 0.0));
    } else {
        let cells = view.cells(coord, false, 0.5);
        for row in cells.rows.clone() {
            for col in cells.cols.clone() {
                emit(cells.face(col, row));
            }
        }
    }
}

fn in_large_group(game: &Game, coord: Coord) -> bool {
    coord
        .parent()
        .is_some_and(|parent| game.large_group(parent))
}

fn large_work(game: &Game, coord: Coord) -> Option<u64> {
    if !matches!(coord.level(), Level::B | Level::D | Level::F) {
        return None;
    }
    let [x, y] = first_block(coord);
    let (tile, progress) = game.large_at(x, y)?;
    (tile == coord).then_some(progress)
}

fn paint_large(
    p: &mut Painter<'_>,
    game: &Game,
    view: &View,
    coord: Coord,
    work: u64,
    style: &Style<'_>,
) {
    let face = view.large(coord, in_large_group(game, coord));
    let color = style.face(coord, false);
    if color == style.colors.bg {
        return;
    }
    view.fill(p, face, color);
    if work != 0
        && style
            .motion
            .and_then(|motion| motion.opened(coord))
            .is_none()
    {
        let progress = (work as f64 / coord.level().tile_capacity() as f64).sqrt();
        let side = face.w.min(face.h) * progress;
        let center = [
            face.left + (face.w - side) * 0.5,
            face.top + (face.h - side) * 0.5,
        ];
        view.fill(
            p,
            Rect::square(center, side, face.radius * progress),
            style.colors.black,
        );
    }
}

fn paint_completed(
    p: &mut Painter<'_>,
    game: &Game,
    view: &View,
    area: Area,
    coord: Coord,
    style: &Style<'_>,
    turning: &mut HashSet<Coord>,
) {
    let level = grain(coord.level(), view.pitch_px);
    if let Some(visible) = Area::around(coord, level).intersect(area.at_level(level)) {
        for part in visible.coords(level) {
            if let Some(motion) = style
                .motion
                .filter(|motion| !motion.effects.large.is_empty())
            {
                let covering_flip = std::iter::successors(Some(part), |c| c.parent())
                    .take_while(|c| c.level() <= coord.level())
                    .find(|&c| motion.large_flip(c).is_some());
                if let Some(large) = covering_flip {
                    turning.insert(large);
                    continue;
                }
            }
            if level == Level::A {
                paint_block(p, view, part, Block::from_mask(u64::MAX), style);
            } else {
                let mut face = view.large(part, in_large_group(game, part));
                if span(level) as f64 * view.pitch_px < 10.0 {
                    face.radius = 0.0;
                }
                view.fill(p, face, style.colors.black);
            }
        }
    }
}

fn frame_levels(count: impl Fn(Level) -> usize) -> impl Iterator<Item = Level> {
    Level::ALL.into_iter().skip(1).take_while(move |&level| {
        level == Level::B || count(level.child().expect("framed level has children")) >= 2
    })
}

fn section_rect(view: &View, coord: Coord) -> Rect {
    let level = coord.level() as u8 + 1;
    view.footprint(coord, 8.0 + 4.0 * f64::from(level))
        .padded(FRAME_PAD * view.px)
}

fn walk(game: &Game, area: Area, mut visit: impl FnMut(Coord) -> bool) {
    fn walk_one(
        game: &Game,
        visible: &[Area; 8],
        coord: Coord,
        visit: &mut impl FnMut(Coord) -> bool,
    ) {
        if game.unit_observed(coord)
            && visit(coord)
            && let Some(level) = coord.level().child()
            && let Some(area) = visible[level as usize].intersect(Area::around(coord, level))
        {
            for child in area.coords(level) {
                walk_one(game, visible, child, visit);
            }
        }
    }

    let [min_x, min_y, max_x, max_y] = game.bounds();
    let occupied = Area {
        min: [min_x, min_y],
        max: [max_x, max_y],
    }
    .at_level(Level::H);
    let visible = Level::ALL.map(|level| area.at_level(level));
    if let Some(roots) = occupied.intersect(visible[Level::H as usize]) {
        for root in roots.coords(Level::H) {
            walk_one(game, &visible, root, &mut visit);
        }
    }
}

#[derive(Clone, Copy)]
enum Surface {
    Block(Coord, Block),
    Large(Coord, u64),
    Completed(Coord),
}

#[derive(Default)]
struct Scene {
    sections: Vec<Coord>,
    groups: Vec<Coord>,
    surfaces: Vec<Surface>,
    ghosts: HashSet<Coord>,
    big_ghosts: HashSet<Coord>,
}

impl Scene {
    fn collect(game: &Game, viewport: Area, scan: Area) -> Self {
        let mut scene = Self::default();
        let last_frame = frame_levels(|level| game.units_len(level))
            .last()
            .unwrap_or(Level::A);
        walk(game, scan.expanded(1), |coord| {
            let level = coord.level();
            if level == Level::A {
                if let Some(block) = game.block_at(coord.x(), coord.y()) {
                    if viewport.contains(coord) {
                        scene.surfaces.push(Surface::Block(coord, block));
                    }
                    if !block.is_complete() {
                        scene.ghosts_near(game, viewport, coord);
                    }
                }
                return false;
            }
            let large = large_work(game, coord);
            if level <= last_frame {
                if game.large_group(coord) {
                    scene.groups.push(coord);
                } else if large.is_none() && !in_large_group(game, coord) {
                    scene.sections.push(coord);
                }
            }
            if let Some(work) = large {
                scene.surfaces.push(Surface::Large(coord, work));
                false
            } else if game.unit_complete(coord) {
                scene.surfaces.push(Surface::Completed(coord));
                false
            } else {
                true
            }
        });

        // Search from the sparse open tree, not the (potentially huge) empty
        // viewport. A source may be outside view while its neighboring group is visible.
        for (level, unlock) in [
            (Level::B, Level::D),
            (Level::D, Level::F),
            (Level::F, Level::H),
        ] {
            if game.cleared(unlock) == &Amount::ZERO {
                continue;
            }
            let parent = level.parent().expect("large tile has a group");
            let visible_groups = scan.at_level(parent);
            walk(game, scan.expanded(span(parent) + span(level)), |coord| {
                if coord.level() > level {
                    return !game.unit_complete(coord) && large_work(game, coord).is_none();
                }
                if large_work(game, coord).is_some() {
                    let own = coord.parent();
                    for (dx, dy) in NEIGHBORS {
                        if let Some(group) = coord.checked_offset(dx, dy).and_then(Coord::parent)
                            && Some(group) != own
                            && visible_groups.contains(group)
                            && !scene.big_ghosts.contains(&group)
                            && game.can_open_large_group(group)
                        {
                            scene.big_ghosts.insert(group);
                        }
                    }
                }
                false
            });
        }
        scene
    }

    fn ghosts_near(&mut self, game: &Game, viewport: Area, open: Coord) {
        for (dx, dy) in NEIGHBORS {
            if let Some(candidate) = open.checked_offset(dx, dy)
                && viewport.contains(candidate)
                && !self.ghosts.contains(&candidate)
                && game.can_open_a(candidate.x(), candidate.y())
            {
                if let Some(group) = game.large_ghost(candidate.x(), candidate.y()) {
                    self.big_ghosts.insert(group);
                } else {
                    self.ghosts.insert(candidate);
                }
            }
        }
    }
}

pub(super) fn paint(
    p: &mut Painter<'_>,
    camera: Camera,
    game: &Game,
    _hover: Option<Hit>,
    theme: Theme,
) {
    paint_frame(p, camera, game, theme, None);
}

pub(super) fn paint_animated(
    p: &mut Painter<'_>,
    camera: Camera,
    game: &Game,
    hover: Option<Hit>,
    theme: Theme,
    effects: &Effects,
    now: Instant,
) {
    if effects.active() || game.item().is_some() {
        paint_frame(p, camera, game, theme, Some(Motion { effects, now }));
    } else {
        paint(p, camera, game, hover, theme);
    }
}

fn paint_frame(
    p: &mut Painter<'_>,
    camera: Camera,
    game: &Game,
    theme: Theme,
    motion: Option<Motion<'_>>,
) {
    let size = p.size();
    let view = View::new([f64::from(size.width), f64::from(size.height)], camera);
    if view.size.contains(&0.0) || !view.px.is_finite() || view.px <= 0.0 {
        return;
    }
    let style = Style {
        colors: Colors::new(theme),
        motion,
    };
    let colors = &style.colors;
    let viewport = view.blocks(0.5);
    let scan = view.blocks(3.0 + FRAME_PAD * view.px);
    let scene = Scene::collect(game, viewport, scan);

    for coord in scene.sections {
        let width = 1.0 + 0.6 * f64::from(coord.level() as u8 - Level::B as u8);
        view.outline(p, section_rect(&view, coord), width, colors.section);
    }
    for coord in scene.ghosts {
        ghost_shapes(&view, coord, |rect| {
            view.outline(p, rect, 1.0, colors.ghost)
        });
    }
    for group in scene.big_ghosts {
        let level = group.level().child().expect("large group contains tiles");
        if let Some(visible) = Area::around(group, level).intersect(viewport.at_level(level)) {
            for coord in visible
                .coords(level)
                .filter(|&coord| !game.unit_present(coord))
            {
                view.outline(
                    p,
                    view.large(coord, in_large_group(game, coord)),
                    1.0,
                    colors.ghost,
                );
            }
        }
    }
    let mut turning = HashSet::new();
    for surface in scene.surfaces {
        match surface {
            Surface::Block(coord, block) => paint_block(p, &view, coord, block, &style),
            Surface::Large(coord, work) => paint_large(p, game, &view, coord, work, &style),
            Surface::Completed(coord) => {
                paint_completed(p, game, &view, viewport, coord, &style, &mut turning)
            }
        }
    }
    for group in scene.groups {
        let level = group.level().child().expect("large group contains tiles");
        if span(level) as f64 * view.pitch_px >= 80.0 {
            let rect = view.footprint(group, 0.0).padded(GUTTER * view.px * 0.5);
            view.outline(p, rect, 1.5, colors.separator);
        }
    }
    if let Some(motion) = motion {
        for coord in turning {
            if let Some(time) = motion.large_flip(coord) {
                let face = view.large(coord, in_large_group(game, coord));
                let (face, color) = style.flip(face, time);
                view.fill(p, face, color);
            }
        }
        paint_halos(p, &view, game, motion, colors);
        if let Some((coord, at)) = motion.effects.item_picked_up
            && let Some(t) = motion.after(at, Duration::ZERO, ITEM_PICKUP)
        {
            paint_pickup(p, &view, coord, t, colors);
        }
    }
    if let Some(item) = game.item() {
        paint_item(p, &view, item, &style);
    }
}

fn paint_halos(p: &mut Painter<'_>, view: &View, game: &Game, motion: Motion<'_>, colors: &Colors) {
    for (&coord, &at) in &motion.effects.completed {
        let Some(t) = motion.after(at, HALO_DELAY, HALO) else {
            continue;
        };
        let e = ease_out(t);
        let group = game.large_group(coord);
        let tier = coord.level() as u8;
        let (pad, radius, stroke) = if tier > 0 && !group {
            (
                (4.0 + 24.0 * e) * f64::from(tier),
                14.0,
                1.5 * view.px / view.px.max(0.3),
            )
        } else {
            let scale = span(coord.level()) as f64;
            (
                (2.0 + 5.0 * e) * scale,
                6.0 * scale,
                if group {
                    1.5
                } else {
                    1.5 * view.px / view.px.max(0.3)
                },
            )
        };
        let rect = view
            .footprint(coord, radius + pad * 0.3)
            .padded(pad * view.px);
        view.outline(
            p,
            rect,
            stroke,
            mix(colors.bg, colors.halo, 0.22 * (1.0 - t).powi(2)),
        );
    }

    for (&coord, &at) in &motion.effects.opened {
        let Some(t) = motion.after(at, PULSE_DELAY, PULSE) else {
            continue;
        };
        if game.unit_complete(coord) {
            continue;
        }
        let pulse = (0.5 - 0.5 * (TAU * t * 3.0).cos()) * (1.0 - t);
        let tint = mix(colors.bg, colors.halo, 0.35 * pulse);
        if tint == colors.bg {
            continue;
        }
        let (rect, stroke) = if coord.level() == Level::A {
            (
                view.footprint(coord, 7.0).padded(FRAME_PAD * view.px),
                2.0 * view.px / view.px.max(0.3),
            )
        } else {
            let face = view.large(coord, in_large_group(game, coord));
            let pad = face.w * 0.03;
            let mut rect = face.padded(pad);
            rect.radius += pad;
            (rect, 2.0 * view.px / view.px.max(0.02))
        };
        view.outline(p, rect, stroke, tint);
    }
}

fn circle([x, y]: [f64; 2], radius: f64) -> Rect {
    Rect::square([x - radius, y - radius], radius * 2.0, radius)
}

fn item_geometry(view: &View, coord: Coord) -> ([f64; 2], f64) {
    let rect = view.footprint(coord, 0.0);
    let center = [rect.left + rect.w * 0.5, rect.top + rect.h * 0.5];
    (center, (150.0 * view.px).max(46.0))
}

pub(super) fn item_visible(camera: Camera, size: PhysicalSize<u32>, game: &Game) -> bool {
    size.width != 0
        && size.height != 0
        && game.item().is_some_and(|coord| {
            let view = View::new([f64::from(size.width), f64::from(size.height)], camera);
            let (center, size) = item_geometry(&view, coord);
            view.visible(circle(center, size * 1.7 + 1.0))
        })
}

fn paint_pickup(p: &mut Painter<'_>, view: &View, coord: Coord, t: f64, colors: &Colors) {
    let (center, size) = item_geometry(view, coord);
    for ring in 0..4 {
        let phase = t * 1.25 - f64::from(ring) * 0.12;
        if (0.0..1.0).contains(&phase) {
            view.outline(
                p,
                circle(center, size * (0.4 + 2.2 * ease_out(phase))),
                2.2 - 0.4 * f64::from(ring),
                mix(colors.bg, colors.halo, 0.5 * (1.0 - phase)),
            );
        }
    }
}

fn paint_item(p: &mut Painter<'_>, view: &View, coord: Coord, style: &Style<'_>) {
    let (center, size) = item_geometry(view, coord);
    if !view.visible(circle(center, size * 1.7 + 1.0)) {
        return;
    }
    let time = style.motion.map_or(1.0, |motion| {
        motion
            .now
            .saturating_duration_since(motion.effects.clock)
            .as_secs_f64()
    });
    let opacity = style
        .motion
        .and_then(|motion| {
            let (item, at) = motion.effects.item_appeared?;
            (item == coord).then_some(())?;
            motion.before(at, ITEM_APPEAR).map(ease_out)
        })
        .unwrap_or(1.0);
    if opacity == 0.0 {
        return;
    }
    let colors = &style.colors;
    for ring in 0..3 {
        let phase = (time / 2.8 + f64::from(ring) / 3.0).fract();
        let radius = (0.55 + 1.15 * phase) * size;
        let alpha = 0.32 * (PI * phase).sin() * opacity;
        view.outline(
            p,
            circle(center, radius),
            1.6,
            mix(colors.bg, colors.halo, alpha),
        );
    }
    view.outline(
        p,
        circle(center, size * 0.62),
        1.0,
        mix(colors.bg, colors.halo, 0.22 * opacity),
    );

    let [x, y] = center;
    let y = y + (time * 1.6).sin() * size * 0.04;
    let ink = mix(colors.bg, [43, 43, 45], opacity);
    paint_pointer(p, view, [x, y], size * 0.82, ink);
    paint_pointer(
        p,
        view,
        [x, y],
        size * 0.75,
        mix(colors.bg, [243, 241, 236], opacity),
    );
}

fn paint_pointer(p: &mut Painter<'_>, view: &View, [cx, cy]: [f64; 2], size: f64, color: Color) {
    // Rasterize a pointer by horizontal spans so both backends get the same glyph.
    const SHAPE: [[f64; 2]; 7] = [
        [-0.36, -0.48],
        [0.33, -0.11],
        [0.09, -0.05],
        [0.30, 0.22],
        [0.15, 0.33],
        [-0.06, 0.05],
        [-0.25, 0.27],
    ];
    let polygon = SHAPE.map(|[x, y]| [cx + size * x, cy + size * y]);
    let first = (cy - size * 0.48).floor().max(0.0) as u32;
    let last = (cy + size * 0.33).ceil().min(view.size[1]).max(0.0) as u32;
    for row in first..last {
        let y = f64::from(row) + 0.5;
        let mut intersections = [0.0; 7];
        let mut n = 0;
        for ([ax, ay], [bx, by]) in polygon.into_iter().zip(polygon.into_iter().cycle().skip(1)) {
            if (ay > y) != (by > y) {
                intersections[n] = ax + (y - ay) * (bx - ax) / (by - ay);
                n += 1;
            }
        }
        intersections[..n].sort_unstable_by(f64::total_cmp);
        for pair in intersections[..n].as_chunks::<2>().0 {
            let rect = Rect {
                left: pair[0],
                top: f64::from(row),
                w: pair[1] - pair[0],
                h: 1.0,
                radius: 0.0,
            };
            view.fill(p, rect, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::Axis;

    const ORIGIN: Coord = Coord::new(Level::A, 0, 0);

    fn view_at(tile_px: f64) -> View {
        View::new(
            [640.0, 640.0],
            Camera {
                center: [Axis {
                    block: 0,
                    offset: BLOCK_OFFSET * 0.5,
                }; 2],
                scale: tile_px * PITCH / (FACE * SIDE),
            },
        )
    }

    #[test]
    fn intertile_gap_and_block_gutter_are_different_sizes() {
        let v = view_at(28.0);
        let cells = v.cells(ORIGIN, false, 0.0);
        let first = cells.face(0, 0);
        let last = cells.face(7, 7);
        let east = v.origin(Coord::new(Level::A, 1, 0));
        assert_eq!(first.w, 28.0);
        assert_eq!(first.radius, 3.5);
        assert_eq!(cells.face(1, 0).left - first.left - first.w, 3.0);
        assert_eq!(last.left + last.w - first.left, 245.0);
        assert_eq!(east[0] - last.left - last.w, 8.0);
        assert_eq!(cell_at_offset(FACE * SIDE / PITCH), Some(0));
        assert_eq!(cell_at_offset(CELL_OFFSET.next_down()), Some(0));
        for col in 1..8 {
            assert_eq!(cell_at_offset(f64::from(col) * CELL_OFFSET), Some(col));
        }
        assert_eq!(cell_at_offset(BLOCK_OFFSET.next_down()), Some(7));
        assert_eq!(cell_at_offset(BLOCK_OFFSET), None);
        assert_eq!(cell_at_offset(SIDE.next_down()), None);
        assert_eq!(cell_at_offset(-1.0), None);
        assert_eq!(cell_at_offset(f64::NAN), None);
    }

    #[test]
    fn face_modes_switch_at_four_ten_and_twelve_physical_pixels() {
        assert!(Detail::at(4.0_f64.next_down()).coarse);
        assert!(!Detail::at(4.0).coarse);
        assert!(!Detail::at(10.0_f64.next_down()).rounded);
        assert!(Detail::at(10.0).rounded);
        assert!(!Detail::at(12.0_f64.next_down()).fine_done);
        assert!(Detail::at(12.0).fine_done);
        assert!(whole_block(0, Detail::at(3.99)));
        assert!(!whole_block(0, Detail::at(4.0)));
        assert!(!whole_block(1, Detail::at(3.99)));
        assert!(whole_block(u64::MAX, Detail::at(11.99)));
        assert!(!whole_block(u64::MAX, Detail::at(12.0)));
        let coarse = view_at(2.0);
        let cells = coarse.cells(ORIGIN, true, 0.0);
        assert_eq!(cells.side, cells.stride);
    }

    #[test]
    fn ghost_uses_one_sharp_outline_at_a_distance_and_tile_outlines_up_close() {
        let outlines = |tile_px| {
            let mut out = Vec::new();
            ghost_shapes(&view_at(tile_px), ORIGIN, |r| out.push(r));
            out
        };
        let coarse = outlines(3.99);
        assert_eq!(coarse.len(), 1);
        assert_eq!(coarse[0].radius, 0.0);
        assert_eq!(outlines(4.0).len(), 64);
        assert!(outlines(9.99).iter().all(|r| r.radius == 0.0));
        assert!(outlines(10.0).iter().all(|r| r.radius > 0.0));
    }

    #[test]
    fn an_unfinished_neighbor_just_offscreen_can_own_a_ghost() {
        let game = Game::new();
        let area = Area {
            min: [-1, -1],
            max: [1, 1],
        };
        let all = Scene::collect(&game, area, area);
        assert_eq!(all.sections, [Coord::new(Level::B, 0, 0)]);
        assert_eq!(all.surfaces.len(), 1);
        assert_eq!(
            all.ghosts,
            HashSet::from(NEIGHBORS.map(|(x, y)| Coord::new(Level::A, x, y)))
        );
        assert!(all.big_ghosts.is_empty());

        let east = Area {
            min: [1, 0],
            max: [1, 0],
        };
        let scene = Scene::collect(&game, east, east);
        assert_eq!(scene.ghosts, HashSet::from([Coord::new(Level::A, 1, 0)]));
    }

    #[test]
    fn section_levels_require_spread_and_use_signed_group_origin() {
        let counts = [9, 2, 1, 8, 8, 8, 8, 8];
        assert!(frame_levels(|level| counts[level as usize]).eq([Level::B, Level::C]));
        assert!(frame_levels(|_| 1).eq([Level::B]));
        let v = view_at(28.0);
        let origin = v.origin(ORIGIN);
        let group = section_rect(&v, Coord::new(Level::B, 0, 0));
        assert_eq!(group.left, origin[0] - 4.0 * PITCH - FRAME_PAD);
        assert_eq!(group.top, origin[1] - 4.0 * PITCH - FRAME_PAD);
        assert_eq!(group.w, 8.0 * PITCH - GUTTER + 2.0 * FRAME_PAD);
        assert_eq!(group.radius, 16.0);
        assert!(!v.outline_visible(group, 1.0));
    }

    #[test]
    fn completed_grain_is_the_finest_legible_unit() {
        assert_eq!(grain(Level::D, 10.0), Level::A);
        assert_eq!(grain(Level::D, 10.0_f64.next_down()), Level::B);
        assert_eq!(grain(Level::D, 1.25_f64.next_down()), Level::C);
        assert_eq!(grain(Level::D, 0.125), Level::D);
        assert_eq!(grain(Level::B, 0.1), Level::B);
    }

    #[test]
    fn large_group_uses_wide_edge_gaps_only_below_eighty_pixels() {
        let edge = Coord::new(Level::B, -4, 0);
        let inner = Coord::new(Level::B, -3, 0);
        let distant = view_at(1.0);
        assert!(distant.large(edge, true).left > distant.large(edge, false).left);
        assert_eq!(distant.large(inner, true), distant.large(inner, false));
        let close = view_at(2.0);
        assert_eq!(close.large(edge, true), close.large(edge, false));
    }

    #[test]
    fn palettes_match_opaque_composite_samples() {
        let light = Colors::new(Theme::Light);
        assert_eq!(light.bg, [228, 228, 226]);
        assert_eq!(light.white, [251, 251, 249]);
        assert_eq!(light.black, [46, 46, 48]);
        assert_eq!(light.ghost, [199, 199, 198]);
        assert_eq!(light.section, [203, 203, 201]);
        let dark = Colors::new(Theme::Dark);
        assert_eq!(dark.bg, [42, 42, 44]);
        assert_eq!(dark.white, [226, 226, 222]);
        assert_eq!(dark.black, [19, 19, 21]);
        assert_eq!(dark.ghost, [64, 64, 65]);
        assert_eq!(dark.section, [62, 62, 64]);
    }

    #[test]
    fn flip_turns_in_place_and_times_expire_without_removing_live_pulses() {
        let at = Instant::now();
        let mut effects = Effects::default();
        effects.flip_tiles(ORIGIN, 1, at);
        effects.opened(ORIGIN, at);
        effects.completed(ORIGIN, at);
        effects.flip_large(Coord::new(Level::B, 1, 0), at);
        let now = at + FLIP / 2;
        let motion = Motion {
            effects: &effects,
            now,
        };
        assert_eq!(motion.tile_flips(ORIGIN).next(), Some((1, 0.5)));
        let style = Style {
            colors: Colors::new(Theme::Light),
            motion: Some(motion),
        };
        let face = Rect::square([0.0, 0.0], 28.0, 3.5);
        let (turned, color) = style.flip(face, 0.5);
        assert!((turned.h / 28.0 - 0.12).abs() < 1e-12);
        assert_eq!(turned.top + turned.h * 0.5, 14.0);
        assert_eq!(color, [149, 149, 149]);
        assert!(effects.advance(at + FLIP));
        assert!(effects.tiles.is_empty() && effects.large.is_empty());
        assert!(effects.advance(at + PULSE));
        assert!(!effects.advance(at + PULSE + PULSE_DELAY));
    }
}
