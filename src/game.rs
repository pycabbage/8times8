use std::collections::{BTreeMap, HashMap, hash_map::Entry};
use std::hash::{BuildHasher, RandomState};

use crate::{Account, Amount, Balance, Block, CellIndex, Coord, Flips, Ledger, Level};

mod debug;
pub use import::LegacyBoard;
mod import;
mod save;
#[cfg(test)]
mod tests;

const ORIGIN: Coord = Coord::new(Level::A, 0, 0);
const SIDE: i64 = 8;
const CHILDREN: u8 = 64;
const LEVELS: usize = Level::ALL.len();
const NEIGHBORS: [(i64, i64); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
const MANUAL: [(Level, u128, u64); 16] = [
    (Level::A, 8, 2),
    (Level::A, 8, 4),
    (Level::A, 48, 8),
    (Level::B, 8, 1 << 6),
    (Level::B, 8, 1 << 7),
    (Level::B, 8, 1 << 8),
    (Level::B, 48, 1 << 9),
    (Level::C, 8, 1 << 12),
    (Level::C, 8, 1 << 13),
    (Level::C, 8, 1 << 14),
    (Level::C, 48, 1 << 15),
    (Level::D, 8, 1 << 18),
    (Level::D, 8, 1 << 19),
    (Level::D, 8, 1 << 20),
    (Level::D, 48, 1 << 21),
    (Level::E, 8, 1 << 24),
];

/// An individual white tile, or an unfinished large tile representing a whole unit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Target {
    Tile { x: i64, y: i64, cell: CellIndex },
    Large(Coord),
}

/// Ordered work notifications for clients that animate a board action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoardEvent {
    /// An individual tile flipped, or an unfinished large tile gained progress.
    Flipped(Target),
    /// A unit finished (without emitting individual flips for implicit descendants).
    Completed { coord: Coord, was_large: bool },
    /// A playable fine block or a large tile appeared.
    Opened(Coord),
}

#[derive(Default)]
struct Events<'a>(Option<&'a mut dyn FnMut(BoardEvent)>);

impl Events<'_> {
    fn emit(&mut self, event: BoardEvent) {
        if let Some(report) = &mut self.0 {
            report(event);
        }
    }

    fn flips(&mut self, coord: Coord, flips: Flips) {
        if let Some(report) = &mut self.0 {
            for cell in flips {
                report(BoardEvent::Flipped(Target::Tile {
                    x: coord.x(),
                    y: coord.y(),
                    cell,
                }));
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Node {
    Fine { block: Block, order: u64 },
    Large { flipped: u64, order: u64 },
    Branch(Branch),
    Done { group: bool },
}

#[derive(Clone, Copy, Debug, Default)]
struct Branch {
    flipped: u64,
    children: u64,
    done: u8,
    group: bool,
    // True if the group is covered by 64 Large/Done children, with no holes.
    solid: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DragItem {
    Hidden,
    Waiting(Coord),
    Held,
}

/// The board and economy. An unexpanded large tile or a completed region is
/// a single leaf, regardless of how many individual tiles it represents.
pub struct Game {
    // Indexed sparse tree: each branch links to its immediate children by an
    // occupancy mask, so folding a subtree never scans unrelated board state.
    nodes: HashMap<Coord, Node>,
    pending: BTreeMap<u64, Coord>,
    next_order: u64,
    observed: [usize; LEVELS],
    bounds: [i64; 4],
    ledger: Ledger,
    manual_stage: usize,
    drag_item: DragItem,
}

impl Default for Game {
    fn default() -> Self {
        Self::new()
    }
}

impl Game {
    /// Starts with only the 1A block at (0, 0) open.
    pub fn new() -> Self {
        let mut game = Self::empty();
        game.insert_fine(ORIGIN, Block::new());
        game
    }

    fn empty() -> Self {
        Self {
            nodes: HashMap::new(),
            pending: BTreeMap::new(),
            next_order: 0,
            observed: [0; LEVELS],
            bounds: [0; 4],
            ledger: Ledger::default(),
            manual_stage: 0,
            drag_item: DragItem::Hidden,
        }
    }

    /// Returns the fine tile mask, including an implicit full mask under a
    /// completed ancestor. Unfinished large tiles have no fine mask.
    pub fn block_at(&self, x: i64, y: i64) -> Option<Block> {
        let coord = Coord::new(Level::A, x, y);
        match self.nodes.get(&coord) {
            Some(Node::Fine { block, .. }) => Some(*block),
            _ => self
                .unit_complete(coord)
                .then_some(Block::from_mask(u64::MAX)),
        }
    }

    /// Applies the current manual work budget to a white tile in a 1A block.
    pub fn activate(&mut self, x: i64, y: i64, start: CellIndex) -> bool {
        let coord = Coord::new(Level::A, x, y);
        self.white_tile(coord, start)
            && self.apply(Target::Tile { x, y, cell: start }, self.manual_power())
    }

    /// Applies up to `budget` flips, starting at the target and widening to
    /// each containing unit. A fine target can anchor an action at an already
    /// flipped tile, as a machine's queued action may do. Returns whether the board changed.
    pub fn apply(&mut self, target: Target, budget: u64) -> bool {
        self.apply_work(target, budget, &mut Events::default())
    }

    /// Applies work and streams visual events in work order. An observer can
    /// ignore or cap off-screen events without allocating in the game core.
    pub fn apply_with_events(
        &mut self,
        target: Target,
        budget: u64,
        mut report: impl FnMut(BoardEvent),
    ) -> bool {
        self.apply_work(target, budget, &mut Events(Some(&mut report)))
    }

    fn apply_work(&mut self, target: Target, budget: u64, events: &mut Events<'_>) -> bool {
        if budget == 0 {
            return false;
        }
        let (mut coord, hint) = match target {
            Target::Tile { x, y, cell } => {
                let coord = Coord::new(Level::A, x, y);
                if !matches!(self.nodes.get(&coord), Some(Node::Fine { .. })) {
                    return false;
                }
                (coord, Hint::at_tile(coord, cell))
            }
            Target::Large(coord) => {
                if !matches!(self.nodes.get(&coord), Some(Node::Large { .. })) {
                    return false;
                }
                (coord, Hint::center(coord))
            }
        };
        let mut left = self.fill(coord, budget, hint, events);
        while left > 0 {
            let Some(parent) = coord.parent() else { break };
            coord = parent;
            left = self.fill(coord, left, hint, events);
        }
        left != budget
    }

    /// Finds a playable target at a base-grid position and fine tile index.
    pub fn target_at(&self, x: i64, y: i64, cell: CellIndex) -> Option<Target> {
        if let Some((coord, _)) = self.large_at(x, y) {
            return Some(Target::Large(coord));
        }
        let coord = Coord::new(Level::A, x, y);
        self.white_tile(coord, cell)
            .then_some(Target::Tile { x, y, cell })
    }

    fn white_tile(&self, coord: Coord, cell: CellIndex) -> bool {
        matches!(self.nodes.get(&coord), Some(Node::Fine { block, .. }) if block.mask() & (1 << cell.index()) == 0)
    }

    /// Finds the active large tile covering a base-grid position, and its flipped count.
    pub fn large_at(&self, x: i64, y: i64) -> Option<(Coord, u64)> {
        let coord = Coord::new(Level::A, x, y);
        ancestors(coord).find_map(|unit| match self.nodes.get(&unit) {
            Some(Node::Large { flipped, .. }) => Some((unit, *flipped)),
            _ => None,
        })
    }

    /// One candidate per live block (first white tile) and large tile, in opening order.
    pub fn targets(&self) -> impl Iterator<Item = Target> + '_ {
        self.pending
            .values()
            .map(|&coord| match self.nodes[&coord] {
                Node::Fine { block, .. } => Target::Tile {
                    x: coord.x(),
                    y: coord.y(),
                    cell: cell((!block.mask()).trailing_zeros() as u8),
                },
                Node::Large { .. } => Target::Large(coord),
                _ => unreachable!("only live leaves are indexed"),
            })
    }

    /// Whether this unit is represented directly or contained in a completed region.
    pub fn unit_present(&self, coord: Coord) -> bool {
        self.unit_observed(coord) || self.unit_complete(coord)
    }

    /// Whether the sparse tree currently retains this unit explicitly.
    pub fn unit_observed(&self, coord: Coord) -> bool {
        self.nodes.contains_key(&coord)
    }

    pub fn unit_complete(&self, coord: Coord) -> bool {
        std::iter::once(coord)
            .chain(ancestors(coord))
            .any(|unit| matches!(self.nodes.get(&unit), Some(Node::Done { .. })))
    }

    /// Number of units opened at this level, including units later folded away.
    pub fn units_len(&self, level: Level) -> usize {
        self.observed[level.index()]
    }

    /// Tests whether an empty base position meets the ancestor entry rules.
    /// The caller supplies the adjacency to an open block when drawing previews.
    pub fn can_open_a(&self, x: i64, y: i64) -> bool {
        let coord = Coord::new(Level::A, x, y);
        self.covering_region(coord).is_none() && self.blocker_above(coord).is_none()
    }

    /// The 8×8 group that a valid base-position preview would open, if large.
    pub fn large_ghost(&self, x: i64, y: i64) -> Option<Coord> {
        if self.can_open_a(x, y) {
            self.coarse_target(Coord::new(Level::A, x, y))?.parent()
        } else {
            None
        }
    }

    /// Whether a represented unit opened as an 8×8 group of large tiles.
    pub fn large_group(&self, coord: Coord) -> bool {
        match self.nodes.get(&coord) {
            Some(Node::Branch(branch)) => branch.group,
            Some(Node::Done { group }) => *group,
            _ => false,
        }
    }

    /// Preview for a fresh group beside an unfinished group of the same tile size.
    pub fn can_open_large_group(&self, group: Coord) -> bool {
        if !matches!(group.level(), Level::C | Level::E | Level::G)
            || group.bounds_in(Level::A).is_none()
            || !self.fresh(group)
            || self.blocker_above(group).is_some()
        {
            return false;
        }
        NEIGHBORS.into_iter().any(|(dx, dy)| {
            group.checked_offset(dx, dy).is_some_and(|neighbor| {
                (0..8).any(|i| {
                    let (col, row) = match (dx, dy) {
                        (1, 0) => (0, i),
                        (-1, 0) => (7, i),
                        (0, 1) => (i, 0),
                        _ => (i, 7),
                    };
                    let child = neighbor.child(CellIndex::from_col_row(col, row).unwrap());
                    child.is_some_and(|coord| {
                        matches!(self.nodes.get(&coord), Some(Node::Large { .. }))
                    })
                })
            })
        })
    }

    /// Inclusive bounds in 1A units: min x, min y, max x, max y.
    pub fn bounds(&self) -> [i64; 4] {
        self.bounds
    }

    /// The 1C unit holding the waiting drag item, if any.
    pub fn item(&self) -> Option<Coord> {
        match self.drag_item {
            DragItem::Waiting(coord) => Some(coord),
            _ => None,
        }
    }

    pub fn has_drag_item(&self) -> bool {
        self.drag_item == DragItem::Held
    }

    /// Debug control: grant the item or reset its appearance trigger.
    pub fn set_drag_item(&mut self, held: bool) {
        self.drag_item = if held {
            DragItem::Held
        } else {
            DragItem::Hidden
        };
    }

    /// Total completed units at this level, including points already spent.
    pub fn cleared(&self, level: Level) -> &Amount {
        self.ledger.account(level).cleared()
    }

    /// Points still available to spend at this level.
    pub fn available(&self, level: Level) -> Balance {
        self.ledger.account(level).available()
    }

    pub fn bonus(&self, level: Level) -> &Amount {
        self.ledger.account(level).bonus()
    }

    /// Whether the account tier has been reached, including debug grants.
    pub fn tier_unlocked(&self, level: Level) -> bool {
        level == Level::A
            || *self.cleared(level) != Amount::ZERO
            || *self.bonus(level) != Amount::ZERO
    }

    /// Debug credits do not affect the board progression thresholds.
    pub fn add_debug_bonus(&mut self, level: Level, gain: &Amount) -> bool {
        let account = self.ledger.account(level);
        let Some(bonus) = account.bonus().checked_add(gain) else {
            return false;
        };
        self.ledger.set_account(
            level,
            Account::new(account.cleared().clone(), bonus, account.spent().clone()),
        );
        true
    }

    pub fn clear_debug_bonus(&mut self) {
        for level in Level::ALL {
            let account = self.ledger.account(level);
            self.ledger.set_account(
                level,
                Account::new(
                    account.cleared().clone(),
                    Amount::ZERO,
                    account.spent().clone(),
                ),
            );
        }
    }

    pub fn try_spend(&mut self, level: Level, cost: &Amount) -> bool {
        self.ledger.try_pay(level, cost).is_ok()
    }

    /// Number of white tiles the current manual stage can flip in one click.
    pub fn manual_power(&self) -> u64 {
        if self.manual_stage == 0 {
            1
        } else {
            MANUAL[self.manual_stage - 1].2
        }
    }

    pub fn manual_stage(&self) -> u8 {
        self.manual_stage as u8
    }

    /// Debug control: set the stage without charging points.
    pub fn set_manual_stage(&mut self, stage: u8) {
        self.manual_stage = usize::from(stage).min(MANUAL.len());
    }

    /// The power the next upgrade buys, whether or not it is affordable.
    pub fn next_manual_power(&self) -> Option<u64> {
        MANUAL.get(self.manual_stage).map(|upgrade| upgrade.2)
    }

    /// The next upgrade's level and price, whether or not it is affordable.
    pub fn next_upgrade(&self) -> Option<(Level, u128)> {
        MANUAL
            .get(self.manual_stage)
            .map(|&(level, price, _)| (level, price))
    }

    pub fn try_upgrade(&mut self) -> bool {
        let Some((level, price)) = self.next_upgrade() else {
            return false;
        };
        if !self.try_spend(level, &Amount::from(price)) {
            return false;
        }
        self.manual_stage += 1;
        true
    }

    fn fresh(&self, coord: Coord) -> bool {
        self.covering_region(coord).is_none() && !self.nodes.contains_key(&coord)
    }

    // For an A coordinate, tells whether the point is already occupied. For
    // coarser inputs, branches don't cover the full region, but leaves do.
    fn covering_region(&self, coord: Coord) -> Option<Coord> {
        std::iter::once(coord)
            .chain(ancestors(coord))
            .find(|unit| match self.nodes.get(unit) {
                Some(Node::Branch(branch)) => branch.solid,
                Some(_) => true,
                None => false,
            })
    }

    fn blocker_above(&self, coord: Coord) -> Option<Coord> {
        // Larger blockers are visited first so a whole locked span can be skipped.
        Level::ALL
            .into_iter()
            .rev()
            .take_while(|&level| level > coord.level())
            .filter_map(|level| coord.ancestor_at(level))
            .find(|&unit| {
                (unit.x() != 0 || unit.y() != 0)
                    && !neighbors(unit).any(|near| self.unit_complete(near))
            })
    }

    fn has_locked_fine(&self) -> bool {
        self.pending
            .values()
            .any(|&coord| coord.level() == Level::A && self.blocker_above(coord).is_some())
    }

    fn coarse_target(&self, coord: Coord) -> Option<Coord> {
        [
            (Level::H, Level::F),
            (Level::F, Level::D),
            (Level::D, Level::B),
        ]
        .into_iter()
        .filter(|&(threshold, _)| *self.cleared(threshold) != Amount::ZERO)
        .filter_map(|(_, level)| coord.ancestor_at(level))
        .find(|&unit| {
            self.fresh(unit)
                && unit
                    .parent()
                    .and_then(|group| group.bounds_in(Level::A))
                    .is_some()
        })
    }

    // Open one boundary point, returning the largest region known to be
    // covered or blocked (including the point) for skipping on straight edges.
    fn touch(&mut self, coord: Coord, events: &mut Events<'_>) -> Coord {
        if let Some(region) = self.covering_region(coord) {
            return self.covering_group(region).unwrap_or(region);
        }
        if let Some(blocker) = self.blocker_above(coord) {
            return blocker;
        }
        if let Some(large) = self.coarse_target(coord) {
            let group = large.parent().expect("large tile has a group");
            for child in children(group, u64::MAX) {
                if self.fresh(child) {
                    self.insert_large(child, 0);
                    events.emit(BoardEvent::Opened(child));
                }
            }
            self.update_group(group);
            return self.covering_group(large).unwrap_or(large);
        }
        self.insert_fine(coord, Block::new());
        events.emit(BoardEvent::Opened(coord));
        coord
    }

    fn covering_group(&self, coord: Coord) -> Option<Coord> {
        ancestors(coord)
            .find(|unit| matches!(self.nodes.get(unit), Some(Node::Branch(branch)) if branch.solid))
    }

    fn update_group(&mut self, coord: Coord) {
        let solid = matches!(self.nodes.get(&coord), Some(Node::Branch(branch)) if branch.children == u64::MAX)
            && children(coord, u64::MAX).all(|child| {
                matches!(
                    self.nodes.get(&child),
                    Some(Node::Large { .. } | Node::Done { .. })
                )
            });
        let branch = self.branch_mut(coord);
        branch.group = true;
        branch.solid = solid;
    }

    fn insert_fine(&mut self, coord: Coord, block: Block) {
        let order = self.add_order(coord);
        self.insert_leaf(coord, Node::Fine { block, order });
        self.mark_progress(coord, u64::from(block.flipped_count()));
    }

    fn insert_large(&mut self, coord: Coord, flipped: u64) {
        let order = self.add_order(coord);
        self.insert_leaf(coord, Node::Large { flipped, order });
        self.mark_progress(coord, flipped);
    }

    // Rebuilds a disjoint completed leaf without running live actions or
    // awarding points twice. Returns false if a parent should have been folded.
    fn restore_done(&mut self, coord: Coord, group: bool, cleared: &mut [u128; LEVELS]) -> bool {
        self.insert_leaf(coord, Node::Done { group });
        self.mark_progress(coord, coord.level().tile_capacity());
        count_region(cleared, coord.level());
        if let Some(parent) = coord.parent() {
            let branch = self.branch_mut(parent);
            branch.done += 1;
            branch.done < CHILDREN
        } else {
            true
        }
    }

    fn add_order(&mut self, coord: Coord) -> u64 {
        let order = self.next_order;
        self.next_order = order.checked_add(1).expect("opening index exhausted");
        self.pending.insert(order, coord);
        order
    }

    fn insert_leaf(&mut self, mut coord: Coord, node: Node) {
        let old = self.nodes.insert(coord, node);
        debug_assert!(old.is_none());
        let seen = &mut self.observed[coord.level().index()];
        *seen = seen.saturating_add(1);
        self.grow_bounds(coord);
        while let Some(parent) = coord.parent() {
            let bit = 1 << child_index(coord);
            match self.nodes.entry(parent) {
                Entry::Vacant(entry) => {
                    entry.insert(Node::Branch(Branch {
                        children: bit,
                        ..Branch::default()
                    }));
                    let seen = &mut self.observed[parent.level().index()];
                    *seen = seen.saturating_add(1);
                }
                Entry::Occupied(mut entry) => {
                    let Node::Branch(branch) = entry.get_mut() else {
                        unreachable!("leaf overlaps another leaf")
                    };
                    branch.children |= bit;
                    break;
                }
            }
            coord = parent;
        }
    }

    fn grow_bounds(&mut self, coord: Coord) {
        let bounds = coord
            .bounds_in(Level::A)
            .expect("represented bounds fit in i64");
        self.bounds[0] = self.bounds[0].min(bounds.min().x());
        self.bounds[1] = self.bounds[1].min(bounds.min().y());
        self.bounds[2] = self.bounds[2].max(bounds.max().x());
        self.bounds[3] = self.bounds[3].max(bounds.max().y());
    }

    fn branch_mut(&mut self, coord: Coord) -> &mut Branch {
        match self.nodes.get_mut(&coord) {
            Some(Node::Branch(branch)) => branch,
            _ => unreachable!("each leaf's ancestor is a branch"),
        }
    }

    fn mark_progress(&mut self, coord: Coord, flipped: u64) {
        if flipped > 0 {
            for ancestor in ancestors(coord) {
                self.branch_mut(ancestor).flipped += flipped;
            }
        }
    }

    fn fill(&mut self, coord: Coord, budget: u64, hint: Hint, events: &mut Events<'_>) -> u64 {
        if budget == 0 {
            return 0;
        }
        let remaining = match self.nodes.get(&coord) {
            Some(Node::Fine { block, .. }) => 64 - u64::from(block.flipped_count()),
            Some(Node::Large { flipped, .. }) => coord.level().tile_capacity() - flipped,
            Some(Node::Branch(branch)) => coord.level().tile_capacity() - branch.flipped,
            _ => return budget,
        };
        if budget >= remaining && coord.bounds_in(Level::A).is_some() {
            if events.0.is_some()
                && let Node::Fine { mut block, .. } = self.nodes[&coord]
            {
                events.flips(coord, block.flip_from(hint.tile_in(coord), remaining));
            }
            self.finish(coord, remaining, events);
            return budget - remaining;
        }
        match self.nodes.get_mut(&coord).expect("target exists") {
            Node::Fine { block, .. } => {
                events.flips(coord, block.flip_from(hint.tile_in(coord), budget));
            }
            Node::Large { flipped, .. } => {
                *flipped += budget;
                events.emit(BoardEvent::Flipped(Target::Large(coord)));
            }
            Node::Branch(Branch { group: true, .. }) => {
                return self.fill_group(coord, budget, hint, events);
            }
            Node::Branch(_) => return self.fill_nearest(coord, budget, hint, events),
            Node::Done { .. } => unreachable!(),
        }
        self.mark_progress(coord, budget);
        0
    }

    fn closest_live_child(&self, coord: Coord, hint: Hint) -> Option<Coord> {
        let Some(Node::Branch(branch)) = self.nodes.get(&coord) else {
            return None;
        };
        children(coord, branch.children)
            .filter(|child| !matches!(self.nodes[child], Node::Done { .. }))
            .min_by_key(|&child| hint.distance_to(Hint::center(child)))
    }

    fn fill_nearest(
        &mut self,
        coord: Coord,
        mut budget: u64,
        hint: Hint,
        events: &mut Events<'_>,
    ) -> u64 {
        for _ in 0..CHILDREN {
            let Some(child) = self.closest_live_child(coord, hint) else {
                break;
            };
            let left = self.fill(child, budget, hint, events);
            if left == 0 || left == budget {
                return left;
            }
            budget = left;
        }
        budget
    }

    fn fill_group(
        &mut self,
        coord: Coord,
        mut budget: u64,
        hint: Hint,
        events: &mut Events<'_>,
    ) -> u64 {
        let child_level = coord.level().child().expect("group has children");
        let from = hint
            .unit(child_level)
            .filter(|child| child.parent() == Some(coord))
            .or_else(|| self.closest_live_child(coord, hint));
        let Some(start) = from else { return budget };
        for offset in 0..CHILDREN {
            if let Some(child) = coord.child(cell((child_index(start) + offset) % CHILDREN)) {
                budget = self.fill(child, budget, hint, events);
                if budget == 0 {
                    break;
                }
            }
        }
        budget
    }

    fn finish(&mut self, coord: Coord, remaining: u64, events: &mut Events<'_>) {
        let group = self.large_group(coord);
        let previous = self
            .nodes
            .insert(coord, Node::Done { group })
            .expect("target exists");
        let was_large = matches!(previous, Node::Large { .. });
        let mut already_credited = [0; LEVELS];
        self.fold(coord, previous, &mut already_credited);
        self.grow_bounds(coord);
        self.mark_progress(coord, remaining);
        let mut totals = [0; LEVELS];
        count_region(&mut totals, coord.level());
        self.credit(&std::array::from_fn(|i| totals[i] - already_credited[i]));
        events.emit(BoardEvent::Completed { coord, was_large });

        // Large tiles already share an open group; only that group's completion
        // opens its boundary. For other units, opening precedes the parent chain.
        if !was_large {
            self.open_around(coord, events);
        }
        if coord.level() >= Level::C {
            self.update_drag_item(coord);
        }
        if let Some(parent) = coord.parent() {
            let branch = self.branch_mut(parent);
            branch.done += 1;
            if branch.done == CHILDREN {
                debug_assert_eq!(branch.flipped, parent.level().tile_capacity());
                self.finish(parent, 0, events);
            }
        }
    }

    fn fold(&mut self, coord: Coord, node: Node, credited: &mut [u128; LEVELS]) {
        match node {
            Node::Fine { order, .. } | Node::Large { order, .. } => {
                self.pending.remove(&order);
            }
            Node::Branch(branch) => {
                for child in children(coord, branch.children) {
                    let node = self
                        .nodes
                        .remove(&child)
                        .expect("every set bit has a child");
                    self.fold(child, node, credited);
                }
            }
            Node::Done { .. } => count_region(credited, coord.level()),
        }
    }

    fn credit(&mut self, counts: &[u128; LEVELS]) {
        for (level, gain) in Level::ALL
            .into_iter()
            .zip(counts)
            .filter(|(_, gain)| **gain > 0)
        {
            let account = self.ledger.account(level);
            let cleared = account
                .cleared()
                .checked_add(&Amount::from(*gain))
                .expect("representable coordinates cannot exhaust amount storage");
            self.ledger.set_account(
                level,
                Account::new(cleared, account.bonus().clone(), account.spent().clone()),
            );
        }
    }

    fn open_around(&mut self, coord: Coord, events: &mut Events<'_>) {
        if coord.level() == Level::A {
            for near in neighbors(coord) {
                self.touch(near, events);
            }
            return;
        }
        let bounds = coord
            .bounds_in(Level::A)
            .expect("completed unit has representable bounds");
        let min = bounds.min();
        let max = bounds.max();
        let span = span_a(coord.level());
        let mut cursors = [0; 4];
        // Merge four monotone edges in the base visit order: west, east,
        // north, south for each offset. Covered/blocked spans are no-ops,
        // so each side can advance independently by whole regions.
        while let Some((side, &offset)) = cursors
            .iter()
            .enumerate()
            .min_by_key(|&(side, &i)| (i, side))
        {
            if offset >= span {
                break;
            }
            let near = match side {
                0 => min.checked_offset(-1, offset),
                1 => Coord::new(Level::A, max.x(), min.y()).checked_offset(1, offset),
                2 => min.checked_offset(offset, -1),
                _ => Coord::new(Level::A, min.x(), max.y()).checked_offset(offset, 1),
            };
            let Some(point) = near else {
                cursors[side] = span;
                continue;
            };
            let region = self.touch(point, events);
            let Some(region_bounds) = region.bounds_in(Level::A) else {
                cursors[side] += 1;
                continue;
            };
            let (first, end) = if side < 2 {
                (min.y(), region_bounds.max().y())
            } else {
                (min.x(), region_bounds.max().x())
            };
            let next = i128::from(end) - i128::from(first) + 1;
            cursors[side] = next.min(i128::from(span)) as i64;
        }
    }

    fn update_drag_item(&mut self, finished: Coord) {
        match self.drag_item {
            DragItem::Waiting(coord) if self.unit_complete(coord) => {
                self.drag_item = DragItem::Held
            }
            DragItem::Hidden if *self.cleared(Level::C) >= Amount::from(2) => {
                let center = Hint::center(finished)
                    .unit(Level::C)
                    .expect("unit center exists");
                let free = |coord: &Coord| {
                    coord.bounds_in(Level::A).is_some() && !self.unit_complete(*coord)
                };
                let mut choices: Vec<_> = neighbors(center).filter(free).collect();
                for r in 2..64 {
                    if !choices.is_empty() {
                        break;
                    }
                    choices.extend(
                        (-r..=r)
                            .flat_map(|i| [(i, -r), (i, r), (-r, i), (r, i)])
                            .filter_map(|(dx, dy)| center.checked_offset(dx, dy))
                            .filter(free),
                    );
                }
                if !choices.is_empty() {
                    let pick = RandomState::new().hash_one(center) % choices.len() as u64;
                    let coord = choices[pick as usize];
                    self.drag_item = DragItem::Waiting(coord);
                    self.grow_bounds(coord);
                }
            }
            _ => {}
        }
    }
}

fn count_region(counts: &mut [u128; LEVELS], level: Level) {
    for (inner, n) in counts.iter_mut().enumerate().take(level.index() + 1) {
        *n += 1 << (6 * (level.index() - inner));
    }
}

fn cell(index: u8) -> CellIndex {
    CellIndex::new(index).expect("slot fits in 8×8")
}

fn ancestors(coord: Coord) -> impl Iterator<Item = Coord> {
    std::iter::successors(coord.parent(), |coord| coord.parent())
}

fn neighbors(coord: Coord) -> impl Iterator<Item = Coord> {
    NEIGHBORS
        .into_iter()
        .filter_map(move |(dx, dy)| coord.checked_offset(dx, dy))
}

fn children(coord: Coord, mut present: u64) -> impl Iterator<Item = Coord> {
    std::iter::from_fn(move || {
        if present == 0 {
            return None;
        }
        let index = present.trailing_zeros() as u8;
        present &= present - 1;
        Some(cell(index))
    })
    .filter_map(move |index| coord.child(index))
}

fn child_index(coord: Coord) -> u8 {
    let axis = |value: i64| ((value.rem_euclid(SIDE) + SIDE / 2) % SIDE) as u8;
    8 * axis(coord.y()) + axis(coord.x())
}

fn span_a(level: Level) -> i64 {
    level.span_in(Level::A).expect("A is the base level") as i64
}

// Sixteenths of a 1A keep tile centers, region centers and distances exact.
// All comparisons take place within the same H, and wide coordinates keep
// even base positions near the signed limit representable.
#[derive(Clone, Copy)]
struct Hint(i128, i128);

impl Hint {
    fn at_tile(coord: Coord, tile: CellIndex) -> Self {
        let axis = |value, pos| 16 * i128::from(value) + 2 * i128::from(pos) + 1;
        Self(axis(coord.x(), tile.col()), axis(coord.y(), tile.row()))
    }

    fn center(coord: Coord) -> Self {
        let span = i128::from(span_a(coord.level()));
        let axis = |value| 16 * (i128::from(value) * span - 4 * (span - 1) / 7) + 8 * span;
        Self(axis(coord.x()), axis(coord.y()))
    }

    fn tile_in(self, coord: Coord) -> CellIndex {
        let axis = |p: i128, value| (p - 16 * i128::from(value)).div_euclid(2).clamp(0, 7) as u8;
        CellIndex::from_col_row(axis(self.0, coord.x()), axis(self.1, coord.y())).unwrap()
    }

    fn unit(self, level: Level) -> Option<Coord> {
        Coord::new(
            Level::A,
            self.0.div_euclid(16).try_into().ok()?,
            self.1.div_euclid(16).try_into().ok()?,
        )
        .ancestor_at(level)
    }

    fn distance_to(self, other: Self) -> i128 {
        let dx = self.0 - other.0;
        let dy = self.1 - other.1;
        dx * dx + dy * dy
    }
}
