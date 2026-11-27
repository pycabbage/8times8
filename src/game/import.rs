use std::collections::HashSet;

use super::*;

/// Board data from a parsed original v1/v2 save, without its camera or machines.
/// Values within each level retain the order in the original save arrays.
#[derive(Debug, Default)]
pub struct LegacyBoard {
    pub blocks: Vec<(i64, i64, u64)>,
    pub done: Vec<Coord>,
    pub large: Vec<(Coord, u64)>,
    pub groups: Vec<Coord>,
    pub initial_stage: u8,
    pub bonus: [Amount; 8],
    pub spent: [Amount; 8],
    pub item: Option<Coord>,
    pub item_held: bool,
}

impl Game {
    /// Translates a parsed original save into the native sparse board.
    pub fn import_legacy(board: LegacyBoard) -> Result<Self, String> {
        import(board).map_err(str::to_owned)
    }
}

fn import(mut board: LegacyBoard) -> Result<Game, &'static str> {
    let mut game = Game::empty();
    game.manual_stage = usize::from(board.initial_stage);
    if game.manual_stage > MANUAL.len() {
        return Err("invalid original manual stage");
    }
    let mut cleared = [0; LEVELS];
    let mut reopen: [Vec<Coord>; LEVELS] = std::array::from_fn(|_| Vec::new());

    for (x, y, mask) in board.blocks {
        let coord = Coord::new(Level::A, x, y);
        check_fresh(&game, coord)?;
        if mask == u64::MAX {
            load_done(&mut game, coord, &mut cleared, &mut reopen);
        } else {
            game.insert_fine(coord, Block::from_mask(mask));
        }
    }
    board.done.sort_by_key(|coord| coord.level());
    for coord in board.done {
        if coord.level() == Level::A {
            return Err("complete A units must be saved as original blocks");
        }
        check_fresh(&game, coord)?;
        load_done(&mut game, coord, &mut cleared, &mut reopen);
    }

    let mut groups = HashSet::new();
    for coord in board.groups {
        if !matches!(coord.level(), Level::C | Level::E | Level::G)
            || coord.bounds_in(Level::A).is_none()
        {
            return Err("invalid original large group coordinate");
        }
        if !groups.insert(coord) {
            return Err("duplicate original large tile group");
        }
    }
    board.large.sort_by_key(|(coord, _)| coord.level());
    for (coord, flipped) in board.large {
        if !matches!(coord.level(), Level::B | Level::D | Level::F)
            || flipped >= coord.level().tile_capacity()
        {
            return Err("invalid original large tile level or progress");
        }
        check_fresh(&game, coord)?;
        let group = coord.parent().expect("large tile has a parent");
        if group.bounds_in(Level::A).is_none() {
            return Err("invalid original large group coordinate");
        }
        game.insert_large(coord, flipped);
        groups.insert(group); // Early v2 saves did not include group metadata.
    }
    for coord in groups {
        match game.nodes.get(&coord) {
            Some(Node::Branch(_)) => game.update_group(coord),
            Some(Node::Done { .. }) => {
                game.nodes.insert(coord, Node::Done { group: true });
            }
            None if game.unit_complete(coord) => {} // Obsolete markers under compacted parents.
            _ => return Err("original large group has no board records"),
        }
    }

    for (level, (bonus, spent)) in Level::ALL
        .into_iter()
        .zip(board.bonus.into_iter().zip(board.spent))
    {
        // Clearing debug bonuses does not roll back historical spending, so
        // a perfectly valid save may have spent more than its current credits.
        game.ledger
            .set_account(level, Account::new(Amount::ZERO, bonus, spent));
    }
    game.credit(&cleared);

    if game.covering_region(ORIGIN).is_none() {
        game.insert_fine(ORIGIN, Block::new());
    }
    if let Some(coord) = board.item
        && (coord.level() != Level::C || coord.bounds_in(Level::A).is_none())
    {
        return Err("invalid original drag item coordinate");
    }
    game.drag_item = match board.item {
        _ if board.item_held => DragItem::Held,
        Some(coord) if game.unit_complete(coord) => DragItem::Held,
        Some(coord) => {
            game.grow_bounds(coord);
            DragItem::Waiting(coord)
        }
        None => DragItem::Hidden,
    };
    if game.drag_item == DragItem::Hidden {
        // An older save may lack its item state. Seed from the earliest outer
        // completion at the highest level, before reopening the boundaries.
        if let Some(coord) = reopen[Level::C.index()..]
            .iter()
            .rev()
            .flatten()
            .copied()
            .find(|coord| matches!(game.nodes.get(coord), Some(Node::Done { .. })))
        {
            game.update_drag_item(coord);
        }
    }

    let mut events = Events::default();
    for coord in reopen.into_iter().flatten() {
        // During the original load, completed fine blocks are still present
        // when the outer edges reopen; coarser covered completions are not.
        if coord.level() == Level::A || matches!(game.nodes.get(&coord), Some(Node::Done { .. })) {
            game.open_around(coord, &mut events);
        }
    }
    if game.has_locked_fine() {
        return Err("original save contains a fine block behind a locked boundary");
    }
    Ok(game)
}

fn check_fresh(game: &Game, coord: Coord) -> Result<(), &'static str> {
    if coord.bounds_in(Level::A).is_none() {
        return Err("original board coordinate out of range");
    }
    if !game.fresh(coord) {
        return Err("overlapping or duplicate original board records");
    }
    Ok(())
}

fn load_done(
    game: &mut Game,
    coord: Coord,
    cleared: &mut [u128; LEVELS],
    reopen: &mut [Vec<Coord>; LEVELS],
) {
    let has_no_ready_parent = game.restore_done(coord, false, cleared);
    reopen[coord.level().index()].push(coord);
    if has_no_ready_parent {
        return;
    }
    for parent in ancestors(coord) {
        // The children accounted for all flips and rewards at lower levels.
        // Folding closes the parent and adds only its own point.
        let old = game
            .nodes
            .insert(parent, Node::Done { group: false })
            .unwrap();
        game.fold(parent, old, &mut [0; LEVELS]);
        game.grow_bounds(parent);
        cleared[parent.level().index()] += 1;
        reopen[parent.level().index()].push(parent);
        let Some(up) = parent.parent() else { break };
        let branch = game.branch_mut(up);
        branch.done += 1;
        if branch.done != CHILDREN {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_origin_restores_the_mask_and_economy() {
        let blank = Game::import_legacy(LegacyBoard::default()).unwrap();
        assert_eq!(blank.encode(), Game::new().encode());
        let original = LegacyBoard {
            blocks: vec![(0, 0, 1 << 25)],
            initial_stage: 3,
            bonus: std::array::from_fn(|i| Amount::from(if i == 0 { 10 } else { 0 })),
            spent: std::array::from_fn(|i| Amount::from(if i == 0 { 7 } else { 0 })),
            ..LegacyBoard::default()
        };
        let game = Game::import_legacy(original).unwrap();
        assert_eq!(game.block_at(0, 0).unwrap().mask(), 1 << 25);
        assert_eq!(game.manual_power(), 8);
        assert_eq!(game.available(Level::A).to_string(), "3");
        assert_eq!(
            Game::decode(&game.encode()).unwrap().encode(),
            game.encode()
        );
    }

    #[test]
    fn legacy_v1_fine_completion_folds_and_reopens_in_original_block_order() {
        let b = Coord::new(Level::B, 0, 0);
        let blocks = children(b, u64::MAX)
            .map(|coord| (coord.x(), coord.y(), u64::MAX))
            .collect();
        let game = Game::import_legacy(LegacyBoard {
            blocks,
            ..LegacyBoard::default()
        })
        .unwrap();
        assert_eq!(game.cleared(Level::A), &Amount::from(64));
        assert_eq!(game.cleared(Level::B), &Amount::from(1));
        assert!(game.unit_complete(b));
        assert!(children(b, u64::MAX).all(|coord| !game.unit_observed(coord)));
        assert_eq!(game.targets().count(), 32);
        let target = |x, y| Target::Tile {
            x,
            y,
            cell: cell(0),
        };
        assert!(
            game.targets()
                .take(3)
                .eq([target(-5, -4), target(-4, -5), target(-3, -5)])
        );
    }

    #[test]
    fn legacy_v2_uses_ancestor_rewards_once_and_reopens_the_first_d() {
        let fixture = Game::debug_after_1d();
        let done = Coord::new(Level::D, 0, 0);
        let game = Game::import_legacy(LegacyBoard {
            done: vec![done],
            groups: vec![Coord::new(Level::C, 0, 0)],
            item: fixture.item(),
            ..LegacyBoard::default()
        })
        .unwrap();
        assert_eq!(game.encode(), fixture.encode());
        assert!(game.unit_complete(done));
        assert_eq!(game.targets().count(), 32 * 64);
        assert_eq!(game.cleared(Level::C), &Amount::from(64));
        assert_eq!(game.cleared(Level::B), &Amount::from(4096));
        assert_eq!(game.cleared(Level::A), &Amount::from(262_144));
    }

    #[test]
    fn legacy_early_v2_large_progress_and_sparse_group_survive_native_save() {
        let large = Coord::new(Level::B, -45, -36);
        let group = large.parent().unwrap();
        let game = Game::import_legacy(LegacyBoard {
            done: vec![Coord::new(Level::D, 0, 0)],
            large: vec![(large, 123)],
            item_held: true,
            ..LegacyBoard::default()
        })
        .unwrap();
        let at = large.bounds_in(Level::A).unwrap().min();
        assert_eq!(game.large_at(at.x(), at.y()), Some((large, 123)));
        assert!(game.large_group(group));
        assert_eq!(game.targets().next(), Some(Target::Large(large)));
        assert!(
            matches!(game.nodes[&group], Node::Branch(branch) if branch.children.count_ones() == 1)
        );
        assert!(game.has_drag_item());
        let bytes = game.encode();
        assert_eq!(Game::decode(&bytes).unwrap().encode(), bytes);
    }

    #[test]
    fn legacy_import_rejects_overlapping_duplicate_and_invalid_rows() {
        let b = Coord::new(Level::B, 0, 0);
        let cases = [
            LegacyBoard {
                blocks: vec![(0, 0, 0), (0, 0, 1)],
                ..LegacyBoard::default()
            },
            LegacyBoard {
                blocks: vec![(0, 0, 0)],
                done: vec![b],
                ..LegacyBoard::default()
            },
            LegacyBoard {
                done: vec![Coord::new(Level::H, i64::MAX, 0)],
                ..LegacyBoard::default()
            },
            LegacyBoard {
                large: vec![(b, Level::B.tile_capacity())],
                ..LegacyBoard::default()
            },
            LegacyBoard {
                groups: vec![b, b],
                ..LegacyBoard::default()
            },
            LegacyBoard {
                item: Some(b),
                ..LegacyBoard::default()
            },
            LegacyBoard {
                initial_stage: u8::MAX,
                ..LegacyBoard::default()
            },
        ];
        for board in cases {
            assert!(Game::import_legacy(board).is_err());
        }
    }
}
