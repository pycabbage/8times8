use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use _8times8::{Amount, BoardEvent, CellIndex, Coord, Game, Level, Target};

pub(super) const MACHINE_PRICE: u128 = 80;
pub(super) const MACHINES_PER_TIER: usize = 4;

const TIERS: [Level; 5] = [Level::A, Level::B, Level::C, Level::D, Level::E];
// Use the original board geometry internally so targeting and travel timing
// agree. The client sees these coordinates rescaled to an eight-step 1A pitch.
const WORLD_PITCH: f64 = 253.0;
const WORLD_CELL: f64 = 31.0;
const WORLD_TILE: f64 = 28.0;
const WORLD_BLOCK_GAP: f64 = 8.0;
const TILES_PER_WORLD: f64 = 8.0 / WORLD_PITCH;
// An old save can contain the normal purchase slots and at most four v1 extras.
#[cfg(any(debug_assertions, test))]
const LEGACY_IMPORT_LIMIT: usize = TIERS.len() * MACHINES_PER_TIER + 4;
// Divide last so the boundary also accepts callers that compute x * 8 / 253.
#[cfg(any(debug_assertions, test))]
const LEGACY_POSITION_LIMIT: f64 = ((1u64 << 53) - 1) as f64 * 8.0 / WORLD_PITCH;
const REST: Duration = Duration::from_millis(380);
const IDLE: Duration = Duration::from_secs(1);
const PAUSE_POLL: Duration = Duration::from_millis(400);
const PULSE: Duration = Duration::from_millis(320);
const RECORD_LEN: usize = 22;

#[derive(Default)]
pub(super) struct Machines {
    bots: Vec<Machine>,
    next_order: u64,
    paused: bool,
    speed: Option<f64>,
}

#[derive(Clone, Copy)]
enum Step {
    Waiting(Instant),
    Gliding {
        target: Target,
        to: [f64; 2],
        start: Instant,
        end: Instant,
    },
}

impl Step {
    fn deadline(self) -> Instant {
        match self {
            Self::Waiting(at) | Self::Gliding { end: at, .. } => at,
        }
    }
}

struct Machine {
    tier: Level,
    stage: u32,
    legacy: bool,
    position: [f64; 2],
    // A coarse assignment takes precedence but doesn't drop the last 1A claim.
    last_tile: Option<(Coord, CellIndex)>,
    large: Option<Coord>,
    pulse: Option<Instant>,
    order: u64,
    step: Step,
}

impl Machine {
    fn position_at(&self, now: Instant) -> [f64; 2] {
        let Step::Gliding { to, start, end, .. } = self.step else {
            return self.position;
        };
        let elapsed = now.saturating_duration_since(start).as_secs_f64();
        let t = (elapsed / (end - start).as_secs_f64()).min(1.0);
        let ease = if t < 0.5 {
            2.0 * t * t
        } else {
            1.0 - 2.0 * (1.0 - t) * (1.0 - t)
        };
        std::array::from_fn(|i| self.position[i] * (1.0 - ease) + to[i] * ease)
    }

    fn glide(&mut self, target: Target, now: Instant, speed: f64) {
        let to = target_center(target);
        let distance = distance(self.position, to);
        let duration =
            Duration::from_secs_f64((120.0 + distance * 2.5).min(1400.0) / (1000.0 * speed));
        self.large = match target {
            Target::Large(coord) => Some(coord),
            Target::Tile { x, y, cell } => {
                self.last_tile = Some((Coord::new(Level::A, x, y), cell));
                None
            }
        };
        self.step = Step::Gliding {
            target,
            to,
            start: now,
            end: now + duration,
        };
    }
}

pub(super) fn effective_power(tier: Level, stage: u32) -> u64 {
    1 << stage.saturating_add(6 * tier as u32).min(52)
}

impl Machines {
    pub(super) fn tier_unlocked(game: &Game, tier: Level) -> bool {
        TIERS.contains(&tier) && game.tier_unlocked(tier)
    }

    pub(super) fn buy(
        &mut self,
        game: &mut Game,
        tier: Level,
        camera_center: [f64; 2],
        now: Instant,
    ) -> bool {
        let center = camera_center.map(|axis| axis / TILES_PER_WORLD);
        if !Self::tier_unlocked(game, tier)
            || self
                .bots
                .iter()
                .filter(|bot| bot.tier == tier && !bot.legacy)
                .count()
                >= MACHINES_PER_TIER
            || !center.iter().all(|axis| axis.is_finite())
            || !game.try_spend(tier, &Amount::from(MACHINE_PRICE))
        {
            return false;
        }
        let i = self.bots.len();
        let position = [
            center[0] + ((i % 3) as f64 - 1.0) * WORLD_CELL,
            center[1] + (((i / 3) % 3) as f64 - 1.0) * WORLD_CELL,
        ];
        self.add(tier, 0, false, position, now);
        true
    }

    pub(super) fn upgrade(&mut self, game: &mut Game, id: usize) -> bool {
        let Some(bot) = self.bots.get_mut(id) else {
            return false;
        };
        let Some(price) = Amount::machine_price(bot.stage) else {
            return false;
        };
        if !game.try_spend(bot.tier, &price) {
            return false;
        }
        bot.stage += 1;
        true
    }

    fn add(&mut self, tier: Level, stage: u32, legacy: bool, position: [f64; 2], now: Instant) {
        let delay = Duration::from_millis(60 + ((self.bots.len() + 1) % 8) as u64 * 110);
        let order = self.next_schedule_order();
        self.bots.push(Machine {
            tier,
            stage,
            legacy,
            position,
            last_tile: None,
            large: None,
            pulse: None,
            order,
            step: Step::Waiting(now + delay),
        });
    }

    fn next_schedule_order(&mut self) -> u64 {
        let order = self.next_order;
        self.next_order += 1;
        order
    }

    /// An in-flight action still lands after pausing, like the reference.
    #[cfg(any(debug_assertions, test))]
    pub(super) fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }

    /// Speed changes do not reschedule an action that is already in flight.
    #[cfg(any(debug_assertions, test))]
    pub(super) fn set_speed(&mut self, speed: f64) {
        if (1.0..=10.0).contains(&speed) {
            self.speed = Some(speed);
        }
    }

    /// Runs each due transition once and reports board changes, not animation.
    #[allow(dead_code)] // Convenience API for clients that do not consume board events.
    pub(super) fn tick(&mut self, game: &mut Game, now: Instant) -> bool {
        self.tick_with_events(game, now, |_| {})
    }

    /// Reports the same exact board changes as manual play, in work order.
    pub(super) fn tick_with_events(
        &mut self,
        game: &mut Game,
        now: Instant,
        mut on_event: impl FnMut(BoardEvent),
    ) -> bool {
        let mut due: Vec<_> = self
            .bots
            .iter()
            .enumerate()
            .map(|(id, bot)| (bot.step.deadline(), bot.order, id))
            .filter(|&(deadline, _, _)| deadline <= now)
            .collect();
        due.sort_unstable();
        let mut changed = false;
        let speed = self.speed.unwrap_or(1.0);
        for (_, _, id) in due {
            match self.bots[id].step {
                Step::Waiting(_) => {
                    if self.paused {
                        self.bots[id].step = Step::Waiting(now + PAUSE_POLL);
                    } else if let Some(target) = pick_target(game, id, &self.bots) {
                        self.bots[id].glide(target, now, speed);
                    } else {
                        self.bots[id].step = Step::Waiting(now + IDLE);
                    }
                }
                Step::Gliding { target, to, .. } => {
                    let bot = &mut self.bots[id];
                    bot.position = to;
                    if available_on_arrival(game, target)
                        && game.apply_with_events(
                            target,
                            effective_power(bot.tier, bot.stage),
                            &mut on_event,
                        )
                    {
                        changed = true;
                        bot.pulse = Some(now);
                    }
                    bot.step = Step::Waiting(now + REST.div_f64(speed));
                }
            }
            self.bots[id].order = self.next_schedule_order();
        }
        changed
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.bots.iter().map(|bot| bot.step.deadline()).min()
    }

    pub(super) fn moving(&self, now: Instant) -> bool {
        self.bots.iter().any(|bot| {
            matches!(bot.step, Step::Gliding { end, .. } if now < end)
                || bot.pulse.is_some_and(|start| now < start + PULSE)
        })
    }

    /// Render positions in tile steps; a 1A block occupies eight steps per axis.
    pub(super) fn views(&self, now: Instant) -> impl Iterator<Item = (Level, [f64; 2])> + '_ {
        self.bots.iter().map(move |bot| {
            (
                bot.tier,
                bot.position_at(now).map(|axis| axis * TILES_PER_WORLD),
            )
        })
    }

    /// Scale applied to a machine's normal size just after successful work.
    pub(super) fn pulse_scale(&self, id: usize, now: Instant) -> f64 {
        let Some(start) = self.bots.get(id).and_then(|bot| bot.pulse) else {
            return 1.0;
        };
        let t = now.saturating_duration_since(start).as_secs_f64() / PULSE.as_secs_f64();
        if t >= 1.0 {
            1.0
        } else {
            1.0 + 0.18 * (std::f64::consts::PI * t).sin()
        }
    }

    pub(super) fn entries(&self) -> impl Iterator<Item = (usize, Level, u32, bool)> + '_ {
        self.bots
            .iter()
            .enumerate()
            .map(|(id, bot)| (id, bot.tier, bot.stage, bot.legacy))
    }

    /// Restores an ordered list from an old save, with positions already
    /// converted by the parser to the client's eight-step 1A tile space.
    #[cfg(any(debug_assertions, test))]
    pub(super) fn import_legacy(
        bots: Vec<(Level, u32, bool, [f64; 2])>,
        now: Instant,
    ) -> Result<Self, String> {
        if bots.len() > LEGACY_IMPORT_LIMIT {
            return Err("too many machines in legacy save".into());
        }
        let mut machines = Self::default();
        let mut owned = [0; TIERS.len()];
        for (tier, stage, legacy, position) in bots {
            let index = tier as usize;
            if index >= TIERS.len() {
                return Err("invalid machine tier".into());
            }
            count_owned(&mut owned, index, legacy)?;
            // Old coordinates originated from a JS Number. In this range
            // they can be converted back to world units without overflow.
            if !position
                .iter()
                .all(|axis| axis.abs() <= LEGACY_POSITION_LIMIT)
            {
                return Err("invalid machine position in legacy save".into());
            }
            machines.add(
                tier,
                stage,
                legacy,
                position.map(|axis| axis / TILES_PER_WORLD),
                now,
            );
        }
        Ok(machines)
    }

    /// Fixed-size records in ID order: tier, legacy flag, stage (u32 LE), and
    /// settled X/Y (f64 LE, world units). Jobs resume from a fresh stagger.
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.bots.len() * RECORD_LEN);
        for bot in &self.bots {
            bytes.extend([bot.tier as u8, u8::from(bot.legacy)]);
            bytes.extend(bot.stage.to_le_bytes());
            for axis in bot.position {
                bytes.extend(axis.to_le_bytes());
            }
        }
        bytes
    }

    pub(super) fn decode(bytes: &[u8], now: Instant) -> Result<Self, String> {
        let (records, remainder) = bytes.as_chunks::<RECORD_LEN>();
        if !remainder.is_empty() {
            return Err("incomplete machine record".into());
        }
        let mut machines = Self::default();
        let mut owned = [0; TIERS.len()];
        for row in records {
            let index = usize::from(row[0]);
            let tier = *TIERS.get(index).ok_or("invalid machine tier")?;
            let legacy = match row[1] {
                0 => false,
                1 => true,
                _ => return Err("invalid machine legacy flag".into()),
            };
            count_owned(&mut owned, index, legacy)?;
            let stage = u32::from_le_bytes(row[2..6].try_into().unwrap());
            let position = std::array::from_fn(|i| {
                let start = 6 + i * 8;
                f64::from_le_bytes(row[start..start + 8].try_into().unwrap())
            });
            if !position.iter().all(|axis| axis.is_finite()) {
                return Err("invalid machine position".into());
            }
            machines.add(tier, stage, legacy, position, now);
        }
        Ok(machines)
    }
}

fn count_owned(
    owned: &mut [usize; TIERS.len()],
    index: usize,
    legacy: bool,
) -> Result<(), &'static str> {
    if !legacy {
        if owned[index] >= MACHINES_PER_TIER {
            return Err("too many purchased machines in one tier");
        }
        owned[index] += 1;
    }
    Ok(())
}

fn pick_target(game: &Game, id: usize, bots: &[Machine]) -> Option<Target> {
    let bot = &bots[id];
    if let Some(coord) = bot.large.filter(|&coord| large_available(game, coord)) {
        return Some(Target::Large(coord));
    }
    if let Some((coord, cell)) = bot.last_tile
        && let Some(target) = first_white(game, coord, cell)
    {
        return Some(target);
    }

    let reserved: HashSet<_> = bots
        .iter()
        .enumerate()
        .filter(|&(other, _)| other != id)
        .filter_map(|(_, bot)| bot.last_tile.map(|(coord, _)| coord))
        .collect();

    // No spatial index: search the playable frontier only when a bot has no
    // usable current target. Keep opening order within each level; on ties a
    // lower level coarse tile wins, but 1A wins against any coarse tile.
    let mut small = None;
    let mut taken = None;
    let mut large = None;
    for target in game.targets() {
        match target {
            Target::Tile { x, y, .. } => {
                let coord = Coord::new(Level::A, x, y);
                let candidate = if reserved.contains(&coord) {
                    &mut taken
                } else {
                    &mut small
                };
                keep_nearer(candidate, coord, distance(bot.position, unit_center(coord)));
            }
            Target::Large(coord) => {
                let d = distance(bot.position, unit_center(coord));
                if large.is_none_or(|(best_d, best): (f64, Coord)| {
                    d < best_d || (d == best_d && coord.level() < best.level())
                }) {
                    large = Some((d, coord));
                }
            }
        }
    }
    let preferred = small.or(taken);
    if let Some((d, coord)) = large
        && preferred.is_none_or(|(small_d, _)| d < small_d)
    {
        return Some(Target::Large(coord));
    }
    let (_, coord) = preferred?;
    nearest_white(game, coord, bot.position)
}

fn keep_nearer<T>(best: &mut Option<(f64, T)>, item: T, d: f64) {
    if best.as_ref().is_none_or(|&(current, _)| d < current) {
        *best = Some((d, item));
    }
}

fn distance(from: [f64; 2], to: [f64; 2]) -> f64 {
    (to[0] - from[0]).hypot(to[1] - from[1])
}

fn unit_center(coord: Coord) -> [f64; 2] {
    let span = coord.level().span_in(Level::A).unwrap();
    let offset = (4 * (span - 1) / 7) as f64;
    let span = span as f64;
    let center_offset = (span * WORLD_PITCH - WORLD_BLOCK_GAP) / 2.0;
    [coord.x(), coord.y()].map(|axis| (axis as f64 * span - offset) * WORLD_PITCH + center_offset)
}

fn tile_center(x: i64, y: i64, cell: CellIndex) -> [f64; 2] {
    [
        x as f64 * WORLD_PITCH + f64::from(cell.col()) * WORLD_CELL + WORLD_TILE / 2.0,
        y as f64 * WORLD_PITCH + f64::from(cell.row()) * WORLD_CELL + WORLD_TILE / 2.0,
    ]
}

fn target_center(target: Target) -> [f64; 2] {
    match target {
        Target::Large(coord) => unit_center(coord),
        Target::Tile { x, y, cell } => tile_center(x, y, cell),
    }
}

fn first_white(game: &Game, coord: Coord, start: CellIndex) -> Option<Target> {
    let (x, y) = (coord.x(), coord.y());
    let white = !game.block_at(x, y)?.mask();
    let offset = white
        .rotate_right(u32::from(start.index()))
        .trailing_zeros();
    if offset >= 64 {
        return None;
    }
    let cell = CellIndex::new((start.index() + offset as u8) % 64)?;
    Some(Target::Tile { x, y, cell })
}

fn nearest_white(game: &Game, coord: Coord, position: [f64; 2]) -> Option<Target> {
    let (x, y) = (coord.x(), coord.y());
    let mut white = !game.block_at(x, y)?.mask();
    let mut best = None;
    while white != 0 {
        let cell = CellIndex::new(white.trailing_zeros() as u8).unwrap();
        white &= white - 1;
        keep_nearer(&mut best, cell, distance(position, tile_center(x, y, cell)));
    }
    let (_, cell) = best?;
    Some(Target::Tile { x, y, cell })
}

fn large_available(game: &Game, coord: Coord) -> bool {
    let Some(bounds) = coord.bounds_in(Level::A) else {
        return false;
    };
    let min = bounds.min();
    !game.unit_complete(coord)
        && game.target_at(min.x(), min.y(), CellIndex::new(0).unwrap())
            == Some(Target::Large(coord))
}

fn available_on_arrival(game: &Game, target: Target) -> bool {
    match target {
        Target::Large(coord) => large_available(game, coord),
        // Another action may have flipped the intended bit. Keep the
        // original work anchor and let Game advance to the next white bit.
        Target::Tile { x, y, .. } => game
            .block_at(x, y)
            .is_some_and(|block| !block.is_complete()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tile(x: i64, y: i64, i: u8) -> Target {
        Target::Tile {
            x,
            y,
            cell: CellIndex::new(i).unwrap(),
        }
    }

    fn tile_claim(target: Target) -> (Coord, CellIndex) {
        let Target::Tile { x, y, cell } = target else {
            panic!("expected fine tile");
        };
        (Coord::new(Level::A, x, y), cell)
    }

    #[test]
    fn ownership_limit_excludes_legacy_and_prices_spend_only_own_tier() {
        let mut game = Game::new();
        let mut machines = Machines::default();
        let now = Instant::now();
        for tier in Level::ALL {
            assert_eq!(Machines::tier_unlocked(&game, tier), tier == Level::A);
        }
        assert!(game.add_debug_bonus(Level::A, &Amount::from(400)));
        assert!(game.add_debug_bonus(Level::B, &Amount::from(80)));
        assert!(game.add_debug_bonus(Level::F, &Amount::from(80)));
        assert!(!Machines::tier_unlocked(&game, Level::F));
        assert!(!machines.buy(&mut game, Level::F, [0.0; 2], now));

        machines.add(Level::A, 0, true, [0.0; 2], now);
        for _ in 0..MACHINES_PER_TIER {
            assert!(machines.buy(&mut game, Level::A, [0.0; 2], now));
        }
        assert!(!machines.buy(&mut game, Level::A, [0.0; 2], now));
        assert_eq!(game.available(Level::A).to_string(), "80");
        assert!(machines.buy(&mut game, Level::B, [0.0; 2], now));
        assert!(Machines::tier_unlocked(&game, Level::B));
        assert_eq!(game.available(Level::B).to_string(), "0");
        assert!(!machines.upgrade(&mut game, 5));
        assert_eq!(game.available(Level::A).to_string(), "80");

        assert!(machines.upgrade(&mut game, 0));
        assert_eq!(game.available(Level::A).to_string(), "60");
        assert!(machines.upgrade(&mut game, 0));
        assert!(!machines.upgrade(&mut game, 0));
        assert!(!machines.upgrade(&mut game, usize::MAX));
        assert_eq!(game.available(Level::A).to_string(), "0");
        assert_eq!(machines.entries().next(), Some((0, Level::A, 2, true)));
        assert_eq!(machines.entries().last(), Some((5, Level::B, 0, false)));
    }

    #[test]
    fn power_caps_but_upgrade_prices_stay_exact() {
        let mut game = Game::new();
        let mut machines = Machines::default();
        let now = Instant::now();
        let price = Amount::machine_price(79).unwrap();
        let balance = price.checked_sub(&Amount::from(1)).unwrap();
        assert!(game.add_debug_bonus(Level::E, &balance));
        machines.add(Level::E, 79, false, [0.0; 2], now);
        assert!(!machines.upgrade(&mut game, 0));
        assert!(game.add_debug_bonus(Level::E, &Amount::from(1)));
        assert!(machines.upgrade(&mut game, 0));
        assert_eq!(game.available(Level::E).to_string(), "0");
        assert_eq!(machines.entries().next(), Some((0, Level::E, 80, false)));

        for (ordinal, tier) in TIERS.into_iter().enumerate() {
            assert_eq!(effective_power(tier, 0), 1 << (6 * ordinal));
            assert_eq!(effective_power(tier, 1), 2 << (6 * ordinal));
            assert_eq!(effective_power(tier, u32::MAX), 1 << 52);
        }
    }

    #[test]
    fn selection_uses_opening_order_and_preferred_then_reserved_blocks() {
        let mut game = Game::new();
        let mut machines = Machines::default();
        let center = unit_center(Coord::new(Level::A, 0, 0));
        let now = Instant::now();
        assert!(game.apply(tile(0, 0, 0), 64));
        let choices: Vec<_> = game.targets().map(|target| tile_claim(target).0).collect();
        assert_eq!(choices.len(), 4);
        let choice =
            |machines: &Machines| tile_claim(pick_target(&game, 0, &machines.bots).unwrap()).0;
        machines.add(Level::A, 0, false, center, now);
        assert_eq!(choice(&machines), choices[0]);

        for (i, &coord) in choices.iter().enumerate() {
            machines.add(Level::A, 0, true, center, now);
            machines.bots[i + 1].last_tile = Some((coord, CellIndex::new(0).unwrap()));
            assert_eq!(
                choice(&machines),
                choices.get(i + 1).copied().unwrap_or(choices[0])
            );
        }

        let current = choices[3];
        let (x, y) = (current.x(), current.y());
        assert!(game.apply(tile(x, y, 63), 1));
        machines.bots[0].last_tile = Some(tile_claim(tile(x, y, 63)));
        machines.bots[0].position = unit_center(choices[0]);
        assert_eq!(pick_target(&game, 0, &machines.bots), Some(tile(x, y, 0)));
    }

    #[test]
    fn targets_large_tiles_without_reserving_them_and_drops_stale_arrivals() {
        let mut game = Game::new();
        assert!(game.apply(tile(0, 0, 0), Level::D.tile_capacity()));
        assert!(Machines::tier_unlocked(&game, Level::D));
        let large = game
            .targets()
            .find(|t| matches!(t, Target::Large(_)))
            .unwrap();
        let Target::Large(coord) = large else {
            unreachable!()
        };
        let now = Instant::now();
        let mut machines = Machines::default();
        for _ in 0..2 {
            machines.add(Level::A, 0, false, unit_center(coord), now);
        }
        let start = machines.deadline().unwrap();
        assert!(!machines.tick(&mut game, start));
        assert_eq!(machines.bots[0].large, Some(coord));
        assert_eq!(pick_target(&game, 1, &machines.bots), Some(large));

        assert!(game.apply(large, coord.level().tile_capacity()));
        assert!(!machines.tick(&mut game, start + Duration::from_millis(120)));
        assert!(!available_on_arrival(&game, large));
        assert_ne!(machines.bots[1].large, Some(coord));
        assert_eq!(
            machines.pulse_scale(0, start + Duration::from_millis(280)),
            1.0
        );
    }

    #[test]
    fn arrivals_keep_original_anchor_if_another_action_hit_that_bit() {
        let mut game = Game::new();
        let now = Instant::now();
        let target = tile(0, 0, 0);
        let mut machines = Machines::default();
        let start_pos = [WORLD_TILE / 2.0 - 32.0, WORLD_TILE / 2.0];
        machines.add(Level::A, 0, false, start_pos, now);
        let start = now + Duration::from_millis(170);
        assert_eq!(machines.deadline(), Some(start));
        assert!(!machines.tick(&mut game, start));
        let end = start + Duration::from_millis(200);
        assert_eq!(machines.deadline(), Some(end));
        let midway = start + Duration::from_millis(50);
        let (tier, position) = machines.views(midway).next().unwrap();
        assert_eq!(tier, Level::A);
        assert_eq!(
            position,
            [(-18.0 + 4.0) * TILES_PER_WORLD, 14.0 * TILES_PER_WORLD]
        );
        assert!(machines.moving(midway));

        assert!(game.apply(target, 1));
        // A purchase mid-glide takes effect on this arrival.
        assert!(game.add_debug_bonus(Level::A, &Amount::from(20)));
        assert!(machines.upgrade(&mut game, 0));
        let mut events = Vec::new();
        assert!(machines.tick_with_events(&mut game, end, |event| events.push(event)));
        assert_eq!(
            events,
            [
                BoardEvent::Flipped(tile(0, 0, 1)),
                BoardEvent::Flipped(tile(0, 0, 2))
            ]
        );
        assert_eq!(
            machines.views(end).next().unwrap().1,
            [14.0 * TILES_PER_WORLD; 2]
        );
        assert_eq!(game.block_at(0, 0).unwrap().mask(), 0b111);
        assert_eq!(machines.deadline(), Some(end + REST));
        assert_eq!(machines.pulse_scale(0, end + PULSE / 2), 1.18);
        assert!(!machines.moving(end + PULSE));
    }

    #[test]
    fn debug_pause_does_not_cancel_in_flight_work() {
        let mut game = Game::new();
        let now = Instant::now();
        let mut machines = Machines::default();
        machines.add(Level::A, 0, false, target_center(tile(0, 0, 0)), now);
        machines.set_speed(10.0);
        machines.set_paused(true);
        let start = machines.deadline().unwrap();
        assert!(!machines.tick(&mut game, start));
        assert_eq!(machines.deadline(), Some(start + PAUSE_POLL));
        assert!(!machines.moving(start));

        machines.set_paused(false);
        assert!(!machines.tick(&mut game, start + PAUSE_POLL));
        let end = start + PAUSE_POLL + Duration::from_millis(12);
        assert_eq!(machines.deadline(), Some(end));
        machines.set_paused(true);
        assert!(machines.tick(&mut game, end));
        assert_eq!(machines.deadline(), Some(end + Duration::from_millis(38)));
    }

    #[test]
    fn same_deadline_keeps_schedule_order_after_a_delayed_tick() {
        let mut game = Game::new();
        let mut machines = Machines::default();
        let now = Instant::now();
        let at = target_center(tile(0, 0, 0));
        machines.add(Level::A, 6, false, at, now);
        machines.add(Level::A, 6, false, at, now);
        machines.bots[0].step = Step::Waiting(now + Duration::from_millis(400));

        let start = now + Duration::from_millis(500);
        assert!(!machines.tick(&mut game, start));
        let end = start + Duration::from_millis(120);
        assert_eq!(machines.deadline(), Some(end));
        assert!(machines.tick(&mut game, end));
        assert_eq!(machines.bots[0].pulse, None);
        assert_eq!(machines.bots[1].pulse, Some(end));
        assert_eq!(game.cleared(Level::A), &Amount::from(1));
    }

    #[test]
    fn save_round_trip_retains_rows_and_settled_positions_not_jobs() {
        let now = Instant::now();
        let mut machines = Machines::default();
        machines.add(Level::A, 0, false, [0.0; 2], now);
        machines.add(Level::B, 412, false, [63.25, -712.5], now);
        machines.add(Level::A, 1, true, [-7.5, 12.0], now);
        let settled: Vec<_> = machines.views(now).collect();
        machines.bots[0].glide(tile(0, 0, 42), now, 1.0);
        let bytes = machines.encode();

        let later = now + Duration::from_secs(5);
        let loaded = Machines::decode(&bytes, later).unwrap();
        assert!(loaded.entries().eq(machines.entries()));
        assert!(loaded.views(later).eq(settled));
        assert_eq!(loaded.encode(), bytes);
        assert!(!loaded.moving(later));
        assert_eq!(loaded.deadline(), Some(later + Duration::from_millis(170)));

        assert!(Machines::decode(&bytes[..bytes.len() - 1], later).is_err());
        assert!(
            Machines::decode(&bytes[..RECORD_LEN].repeat(MACHINES_PER_TIER + 1), later).is_err()
        );
        let mut invalid = bytes;
        invalid[6..14].copy_from_slice(&f64::NAN.to_le_bytes());
        assert!(Machines::decode(&invalid, later).is_err());
    }

    #[test]
    fn legacy_import_preserves_list_order_and_tile_positions() {
        let now = Instant::now();
        let bots = vec![
            (Level::B, 3, false, [24.0, -6.0]),
            (Level::A, 1, true, [0.0; 2]),
            (Level::E, 79, false, [-4.0, 12.0]),
        ];
        let imported = Machines::import_legacy(bots.clone(), now).unwrap();
        for (index, ((id, tier, stage, legacy), (view_tier, view_pos))) in
            imported.entries().zip(imported.views(now)).enumerate()
        {
            let (expected_tier, expected_stage, expected_legacy, expected_pos) = bots[index];
            assert_eq!(
                (id, tier, stage, legacy),
                (index, expected_tier, expected_stage, expected_legacy)
            );
            assert_eq!(view_tier, tier);
            assert!(
                view_pos
                    .into_iter()
                    .zip(expected_pos)
                    .all(|(a, b)| (a - b).abs() < 1e-12)
            );
            assert_eq!(
                imported.bots[id].step.deadline(),
                now + Duration::from_millis(170 + 110 * id as u64)
            );
        }
        let native_bytes = imported.encode();
        assert_eq!(
            Machines::decode(&native_bytes, now).unwrap().encode(),
            native_bytes
        );
    }

    #[test]
    fn legacy_import_rejects_invalid_slots_counts_and_positions() {
        let now = Instant::now();
        let mut full: Vec<_> = TIERS
            .into_iter()
            .flat_map(|t| [(t, 0, false, [0.0; 2]); MACHINES_PER_TIER])
            .collect();
        full.extend([(Level::A, 1, true, [0.0; 2]); 4]);
        assert!(Machines::import_legacy(full.clone(), now).is_ok());
        full.push((Level::A, 1, true, [0.0; 2]));
        assert!(Machines::import_legacy(full, now).is_err());
        assert!(Machines::import_legacy(vec![(Level::A, 0, false, [0.0; 2]); 5], now).is_err());
        assert!(Machines::import_legacy(vec![(Level::F, 0, true, [0.0; 2])], now).is_err());
        let boundary = [-LEGACY_POSITION_LIMIT, LEGACY_POSITION_LIMIT];
        assert!(Machines::import_legacy(vec![(Level::A, 0, true, boundary)], now).is_ok());
        for invalid in [
            f64::NAN,
            f64::INFINITY,
            -f64::INFINITY,
            LEGACY_POSITION_LIMIT.next_up(),
        ] {
            assert!(
                Machines::import_legacy(vec![(Level::A, 0, true, [0.0, invalid])], now).is_err()
            );
        }
    }
}
