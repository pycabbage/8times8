use std::collections::HashMap;

use crate::{Account, Amount, Balance, Block, CellIndex, Coord, Ledger, Level};

const ORIGIN_A: Coord = Coord::new(Level::A, 0, 0);
const ORIGIN_B: Coord = Coord::new(Level::B, 0, 0);
const ORIGIN_C: Coord = Coord::new(Level::C, 0, 0);
const SIDE: i64 = 8;
const CHILD_COUNT: u8 = 64;
const NEIGHBORS: [(i64, i64); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const UPGRADE_PRICES: [u128; 3] = [8, 8, 48];

/// The early manual game: 1A and 1B completion, with work up to eight tiles per click.
pub struct Game {
    blocks: HashMap<Coord, Block>,
    // A value of 64 represents the completed 1B in place of its 1A records.
    finished_children: HashMap<Coord, u8>,
    ledger: Ledger,
    manual_stage: u8,
}

impl Default for Game {
    fn default() -> Self {
        Self::new()
    }
}

impl Game {
    /// Starts with just the 1A block at (0, 0) open.
    pub fn new() -> Self {
        Self {
            blocks: HashMap::from([(ORIGIN_A, Block::new())]),
            finished_children: HashMap::new(),
            ledger: Ledger::default(),
            manual_stage: 0,
        }
    }

    /// Looks up a 1A block, or returns `None` if it has not been opened.
    /// A block inside a completed 1B is returned as fully flipped.
    pub fn block_at(&self, x: i64, y: i64) -> Option<Block> {
        let coord = Coord::new(Level::A, x, y);
        self.blocks.get(&coord).copied().or_else(|| {
            self.is_finished_b(parent_b(coord))
                .then_some(Block::from_mask(u64::MAX))
        })
    }

    /// Applies one manual click. Closed blocks and already flipped tiles are no-ops.
    /// Work left over after completing the target can continue within its 1B.
    pub fn activate(&mut self, x: i64, y: i64, start: CellIndex) -> bool {
        let origin = Coord::new(Level::A, x, y);
        // Entry restrictions are enforced at opening and never revoke access.
        let Some(block) = self.blocks.get(&origin) else {
            return false;
        };
        if block.mask() & (1 << start.index()) != 0 {
            return false;
        }

        let budget = self.manual_power();
        let mut remaining = budget - self.work_block(origin, start, budget);
        if remaining == 0 {
            return true;
        }
        let parent = parent_b(origin);
        while remaining > 0 && !self.is_finished_b(parent) {
            let Some(next) = self.closest_open_child(parent, origin, start) else {
                break;
            };
            remaining -= self.work_block(next, projected_tile(origin, start, next), remaining);
        }
        true
    }

    /// Total completed units at this level, including points already spent.
    pub fn cleared(&self, level: Level) -> &Amount {
        self.ledger.account(level).cleared()
    }

    /// Number of white tiles the current manual stage can flip in one click.
    pub fn manual_power(&self) -> u64 {
        1 << self.manual_stage
    }

    /// Buys the next implemented manual upgrade if its own level can pay.
    pub fn try_upgrade(&mut self) -> bool {
        let Some((level, price)) = self.next_upgrade() else {
            return false;
        };
        if self.ledger.try_pay(level, &Amount::from(price)).is_err() {
            return false;
        }
        self.manual_stage += 1;
        true
    }

    /// The next implemented upgrade's level and price, even if unaffordable.
    pub fn next_upgrade(&self) -> Option<(Level, u128)> {
        UPGRADE_PRICES
            .get(usize::from(self.manual_stage))
            .map(|&price| (Level::A, price))
    }

    /// Points still available to spend at this level.
    pub fn available(&self, level: Level) -> Balance {
        self.ledger.account(level).available()
    }

    fn is_finished_b(&self, coord: Coord) -> bool {
        self.finished_children.get(&coord) == Some(&CHILD_COUNT)
    }

    fn open_block(&mut self, coord: Coord) {
        let parent = parent_b(coord);
        if self.is_finished_b(parent) {
            return;
        }
        // There are no completed units at 1C or above in this version. The
        // origin 1C also belongs to the origin of every level above it.
        if parent.ancestor_at(Level::C) != Some(ORIGIN_C) {
            return;
        }
        if parent == ORIGIN_B || neighbors(parent).any(|near| self.is_finished_b(near)) {
            self.blocks.entry(coord).or_default();
        }
    }

    fn work_block(&mut self, coord: Coord, start: CellIndex, budget: u64) -> u64 {
        let block = self.blocks.get_mut(&coord).expect("work target is open");
        let flipped = block.flip_from(start, budget).len() as u64;
        if block.is_complete() {
            self.finish_a(coord);
        }
        flipped
    }

    fn finish_a(&mut self, coord: Coord) {
        self.credit_clear(Level::A);
        for near in neighbors(coord) {
            self.open_block(near);
        }

        let parent = parent_b(coord);
        let count = self.finished_children.entry(parent).or_default();
        *count += 1;
        if *count == CHILD_COUNT {
            self.finish_b(parent);
        }
    }

    fn finish_b(&mut self, coord: Coord) {
        // All children have paid their 1A rewards already.
        self.credit_clear(Level::B);
        for child in children(coord) {
            let removed = self.blocks.remove(&child);
            debug_assert!(removed.is_some_and(Block::is_complete));
        }

        let first = coord
            .bounds_in(Level::A)
            .expect("all children of a completed 1B are representable")
            .min();
        for i in 0..SIDE {
            // Visit the west, east, north, and south edges at each offset.
            for (dx, dy) in [(-1, i), (SIDE, i), (i, -1), (i, SIDE)] {
                if let Some(near) = first.checked_offset(dx, dy) {
                    self.open_block(near);
                }
            }
        }
    }

    fn credit_clear(&mut self, level: Level) {
        let account = self.ledger.account(level);
        let cleared = account
            .cleared()
            .checked_add(&Amount::from(1))
            .expect("the board cannot hold enough units to exceed the amount limit");
        self.ledger.set_account(
            level,
            Account::new(cleared, account.bonus().clone(), account.spent().clone()),
        );
    }

    fn closest_open_child(&self, parent: Coord, origin: Coord, start: CellIndex) -> Option<Coord> {
        children(parent)
            .filter(|coord| self.blocks.get(coord).is_some_and(|b| !b.is_complete()))
            .min_by_key(|&coord| {
                // Use half-tile units for exact tile-center to block-center distances.
                // All coordinates here share a 1B, so each axis differs by at most seven.
                let offset =
                    |delta: i64, index: u8| 2 * SIDE * delta + SIDE - (2 * i64::from(index) + 1);
                let dx = offset(coord.x() - origin.x(), start.col());
                let dy = offset(coord.y() - origin.y(), start.row());
                (dx * dx + dy * dy, coord.y(), coord.x())
            })
    }
}

fn parent_b(coord: Coord) -> Coord {
    coord.parent().expect("a 1A coordinate has a 1B parent")
}

fn neighbors(coord: Coord) -> impl Iterator<Item = Coord> {
    NEIGHBORS
        .into_iter()
        .filter_map(move |(dx, dy)| coord.checked_offset(dx, dy))
}

fn children(parent: Coord) -> impl Iterator<Item = Coord> {
    (0..CHILD_COUNT)
        .filter_map(CellIndex::new)
        .filter_map(move |index| parent.child(index))
}

fn projected_tile(origin: Coord, start: CellIndex, target: Coord) -> CellIndex {
    let project =
        |delta: i64, index: u8| (SIDE * delta + i64::from(index)).clamp(0, SIDE - 1) as u8;
    CellIndex::from_col_row(
        project(origin.x() - target.x(), start.col()),
        project(origin.y() - target.y(), start.row()),
    )
    .expect("projected tile is inside the block")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(index: u8) -> CellIndex {
        CellIndex::new(index).unwrap()
    }

    fn close_a(game: &mut Game, x: i64, y: i64) {
        loop {
            let block = game.block_at(x, y).expect("test target is open");
            if block.is_complete() {
                break;
            }
            let white = (!block.mask()).trailing_zeros() as u8;
            assert!(game.activate(x, y, cell(white)));
        }
    }

    fn next_child(game: &Game, parent: Coord) -> Coord {
        children(parent)
            .find(|coord| {
                game.block_at(coord.x(), coord.y())
                    .is_some_and(|block| !block.is_complete())
            })
            .expect("test parent has an open child to finish")
    }

    fn close_children(game: &mut Game, parent: Coord, count: u8) {
        for _ in 0..count {
            let coord = next_child(game, parent);
            close_a(game, coord.x(), coord.y());
        }
    }

    #[test]
    fn starts_with_one_open_block_and_one_flip_per_click() {
        let mut game = Game::default();
        assert_eq!(game.block_at(0, 0), Some(Block::new()));
        assert_eq!(game.manual_power(), 1);
        assert_eq!(game.next_upgrade(), Some((Level::A, 8)));
        for (x, y) in [(0, -1), (-1, 0), (1, 0), (0, 1), (i64::MIN, i64::MAX)] {
            assert_eq!(game.block_at(x, y), None);
            assert!(!game.activate(x, y, cell(0)));
        }
        for level in Level::ALL {
            assert_eq!(game.cleared(level), &Amount::ZERO);
            assert_eq!(game.available(level).to_string(), "0");
        }
        assert!(game.activate(0, 0, cell(27)));
        assert!(!game.activate(0, 0, cell(27)));
        assert_eq!(game.block_at(0, 0).unwrap().mask(), 1 << 27);
    }

    #[test]
    fn clearing_a_opens_four_neighbors_without_resetting_existing_tiles() {
        let mut game = Game::new();
        close_a(&mut game, 0, 0);
        for (x, y) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            assert_eq!(game.block_at(x, y), Some(Block::new()));
        }
        for (x, y) in [(1, 1), (-1, -1), (2, 0), (0, -2)] {
            assert_eq!(game.block_at(x, y), None);
        }
        assert_eq!(game.cleared(Level::A), &Amount::from(1));
        assert!(!game.activate(0, 0, cell(0)));

        close_a(&mut game, 1, 0);
        assert!(game.activate(1, 1, cell(42)));
        close_a(&mut game, 0, 1);
        assert_eq!(game.block_at(1, 1).unwrap().mask(), 1 << 42);
        assert_eq!(game.cleared(Level::A), &Amount::from(3));
    }

    #[test]
    fn cannot_open_across_an_unfinished_ancestor_boundary() {
        let mut game = Game::new();
        close_a(&mut game, 0, 0);
        for x in (-4..0).rev().chain(1..=3) {
            close_a(&mut game, x, 0);
        }
        for x in [-5, 4] {
            assert_eq!(game.block_at(x, 0), None);
            assert!(!game.activate(x, 0, cell(0)));
        }

        // At the origin 1C's eastern edge, adjacency at 1B is not enough.
        let edge = Coord::new(Level::B, 3, 0);
        game.finished_children.insert(edge, CHILD_COUNT);
        game.open_block(Coord::new(Level::A, 19, 0));
        assert_eq!(game.block_at(19, 0), Some(Block::new()));
        game.open_block(Coord::new(Level::A, 28, 0));
        assert_eq!(game.block_at(28, 0), None);
    }

    #[test]
    fn the_sixty_fourth_child_completes_b_and_opens_its_edges_once() {
        let mut game = Game::new();
        close_children(&mut game, ORIGIN_B, 63);
        assert_eq!(game.cleared(Level::A), &Amount::from(63));
        assert_eq!(game.cleared(Level::B), &Amount::ZERO);
        assert_eq!(game.finished_children.get(&ORIGIN_B), Some(&63));
        assert_eq!(game.block_at(4, 0), None);

        // Leave extra work on the winning click: it must not cross into the next 1B.
        let last = next_child(&game, ORIGIN_B);
        game.manual_stage = 3;
        game.blocks.insert(last, Block::from_mask(!1));
        assert!(game.activate(last.x(), last.y(), cell(0)));
        assert_eq!(game.cleared(Level::A), &Amount::from(64));
        assert_eq!(game.cleared(Level::B), &Amount::from(1));
        assert_eq!(game.available(Level::B).to_string(), "1");
        for child in children(ORIGIN_B) {
            assert!(!game.blocks.contains_key(&child));
            assert!(game.block_at(child.x(), child.y()).unwrap().is_complete());
            assert!(!game.activate(child.x(), child.y(), cell(0)));
        }
        for i in -4..4 {
            for (x, y) in [(-5, i), (4, i), (i, -5), (i, 4)] {
                assert_eq!(game.block_at(x, y), Some(Block::new()));
            }
        }
        assert_eq!(game.block_at(4, 4), None);
        assert_eq!(game.block_at(5, 0), None);

        // Compacting a second 1B neither drops work elsewhere nor reopens the first.
        game.manual_stage = 0;
        assert!(game.activate(-5, 0, cell(0)));
        close_children(&mut game, Coord::new(Level::B, 1, 0), 64);
        assert_eq!(game.block_at(-5, 0).unwrap().mask(), 1);
        assert!(!game.blocks.contains_key(&ORIGIN_A));
        assert_eq!(game.cleared(Level::A), &Amount::from(128));
        assert_eq!(game.cleared(Level::B), &Amount::from(2));
        assert_eq!(game.cleared(Level::C), &Amount::ZERO);
    }

    #[test]
    fn manual_work_wraps_but_cannot_start_on_a_flipped_tile() {
        let mut game = Game::new();
        game.manual_stage = 2;
        assert!(game.activate(0, 0, cell(62)));
        let mask = (1 << 62) | (1 << 63) | 3;
        assert_eq!(game.block_at(0, 0).unwrap().mask(), mask);
        assert!(!game.activate(0, 0, cell(62)));
        assert_eq!(game.block_at(0, 0).unwrap().mask(), mask);
    }

    #[test]
    fn spill_targets_the_nearest_open_child_with_row_major_ties() {
        let mut game = Game::new();
        game.manual_stage = 3;
        let start = cell(27);
        game.blocks
            .insert(ORIGIN_A, Block::from_mask(!(1 << start.index())));
        assert!(game.activate(0, 0, start));

        // North and west are equally far from tile 27. North is earlier by row,
        // and the projected starting tile there is 59 (column 3, bottom row).
        assert_eq!(game.block_at(0, -1).unwrap().mask(), (u64::MAX << 59) | 3);
        for (x, y) in [(-1, 0), (1, 0), (0, 1)] {
            assert_eq!(game.block_at(x, y), Some(Block::new()));
        }
    }

    #[test]
    fn spill_continues_after_another_nearby_child_completes() {
        let mut game = Game::new();
        game.manual_stage = 3;
        game.blocks.insert(ORIGIN_A, Block::from_mask(!(1 << 31)));
        game.blocks
            .insert(Coord::new(Level::A, 1, 0), Block::from_mask(!(1 << 24)));
        assert!(game.activate(0, 0, cell(31)));

        // From the right edge the east block is closer than the north block.
        // It takes one flip, then all six remaining flips go north.
        assert!(game.block_at(1, 0).unwrap().is_complete());
        assert_eq!(game.block_at(0, -1).unwrap().mask(), (1 << 63) | 31);
        assert_eq!(game.cleared(Level::A), &Amount::from(2));
    }

    #[test]
    fn upgrades_spend_only_the_required_a_and_leave_state_on_failure() {
        let mut game = Game::new();
        assert!(!game.try_upgrade());
        for (stage, price) in UPGRADE_PRICES.into_iter().enumerate() {
            close_children(&mut game, ORIGIN_B, (price - 1) as u8);
            let a_before = game.ledger.account(Level::A).clone();
            assert_eq!(
                game.available(Level::A).to_string(),
                (price - 1).to_string()
            );
            assert_eq!(game.manual_power(), 1 << stage);
            assert_eq!(game.next_upgrade(), Some((Level::A, price)));
            assert!(!game.try_upgrade());
            assert_eq!(game.ledger.account(Level::A), &a_before);
            assert_eq!(game.manual_power(), 1 << stage);
            assert_eq!(game.next_upgrade(), Some((Level::A, price)));

            close_children(&mut game, ORIGIN_B, 1);
            let cleared = game.cleared(Level::A).clone();
            assert!(game.try_upgrade());
            assert_eq!(game.cleared(Level::A), &cleared);
            assert_eq!(game.available(Level::A).to_string(), "0");
            assert_eq!(game.manual_power(), 2 << stage);
        }
        assert_eq!(game.cleared(Level::A), &Amount::from(64));
        assert_eq!(game.available(Level::B).to_string(), "1");
        assert_eq!(game.next_upgrade(), None);
        assert!(!game.try_upgrade());
        assert_eq!(game.manual_power(), 8);
    }
}
