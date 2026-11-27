use super::*;

fn unit(level: Level, x: i64, y: i64) -> Coord {
    Coord::new(level, x, y)
}

fn tile(x: i64, y: i64, index: u8) -> Target {
    Target::Tile {
        x,
        y,
        cell: cell(index),
    }
}

fn at(game: &Game, coord: Coord) -> Target {
    let min = coord.bounds_in(Level::A).unwrap().min();
    game.target_at(min.x(), min.y(), cell(0)).unwrap()
}

#[test]
fn starts_with_one_target_and_opens_cardinal_neighbors_in_order() {
    let mut game = Game::new();
    assert_eq!(game.bounds(), [0; 4]);
    assert!(game.targets().eq([tile(0, 0, 0)]));
    assert_eq!(game.manual_power(), 1);
    assert_eq!(game.next_upgrade(), Some((Level::A, 8)));
    assert_eq!(game.block_at(0, 0), Some(Block::new()));
    assert_eq!(game.item(), None);
    assert!(!game.has_drag_item());
    assert!(!game.activate(-1, 0, cell(0)));
    assert!(!game.activate(i64::MIN, i64::MAX, cell(0)));
    assert!(!game.apply(tile(0, 0, 0), 0));
    assert!(game.activate(0, 0, cell(27)));
    assert!(!game.activate(0, 0, cell(27)));
    assert_eq!(game.target_at(0, 0, cell(27)), None);
    assert_eq!(game.block_at(0, 0).unwrap().mask(), 1 << 27);
    assert!(game.apply(tile(0, 0, 0), 63));
    assert_eq!(game.cleared(Level::A), &Amount::from(1));
    assert!(
        game.targets()
            .eq([tile(1, 0, 0), tile(-1, 0, 0), tile(0, 1, 0), tile(0, -1, 0)])
    );
    assert_eq!(game.bounds(), [-1, -1, 1, 1]);
    assert!(game.can_open_a(-1, -1));
    assert!(!game.can_open_a(-1, 0));
    assert!(!game.can_open_a(-5, 0));
    assert!(!game.can_open_a(4, 0));
    assert!(game.apply(tile(-1, 0, 0), 64));
    assert!(game.activate(-1, -1, cell(42)));
    assert!(game.apply(tile(0, -1, 0), 64));
    assert_eq!(game.block_at(-1, -1).unwrap().mask(), 1 << 42);
}

#[test]
fn different_openings_use_exact_signed_parent_boundaries() {
    let mut game = Game::new();
    for x in (-4..=0).rev() {
        assert!(game.apply(tile(x, 0, 0), 64));
        assert!(game.unit_complete(unit(Level::A, x, 0)));
    }
    assert_eq!(game.block_at(-5, 0), None);

    // With five of the origin B's A units clear, the remaining work folds B
    // and opens all four of its edges (but does not spend a single flip on them).
    assert!(game.apply(tile(1, 0, 0), 59 * 64));
    assert_eq!(game.cleared(Level::A), &Amount::from(64));
    assert_eq!(game.cleared(Level::B), &Amount::from(1));
    assert_eq!(game.units_len(Level::A), 51); // 19 observed inside, 32 on edges
    let b = unit(Level::B, 0, 0);
    assert!(game.unit_complete(b));
    for child in children(b, u64::MAX) {
        assert!(game.unit_present(child));
        assert!(!game.unit_observed(child));
        assert!(!game.activate(child.x(), child.y(), cell(0)));
    }
    let edges =
        (-4..4).flat_map(|i| [tile(-5, i, 0), tile(4, i, 0), tile(i, -5, 0), tile(i, 4, 0)]);
    assert!(game.targets().eq(edges));
    assert_eq!(game.block_at(-5, -5), None);
}

#[test]
fn manual_work_wraps_and_spill_uses_distance_row_major_ties_and_projection() {
    let mut game = Game::new();
    assert!(game.apply(tile(0, 0, 62), 4));
    assert_eq!(
        game.block_at(0, 0).unwrap().mask(),
        (1 << 62) | (1 << 63) | 3
    );
    assert!(!game.activate(0, 0, cell(62)));
    assert!(game.apply(tile(0, 0, 62), 2)); // queued machine anchor is already black
    assert_eq!(
        game.block_at(0, 0).unwrap().mask(),
        (1 << 62) | (1 << 63) | 15
    );

    let mut game = Game::empty();
    game.insert_fine(ORIGIN, Block::from_mask(!(1 << 27)));
    assert!(game.apply(tile(0, 0, 27), 8));
    // North and west are equally far. North is earlier by row, and starts
    // on column 3/bottom row, then wraps around in row-major order.
    assert_eq!(game.block_at(0, -1).unwrap().mask(), (u64::MAX << 59) | 3);
    for (x, y) in [(-1, 0), (1, 0), (0, 1)] {
        assert_eq!(game.block_at(x, y), Some(Block::new()));
    }

    let mut game = Game::empty();
    game.insert_fine(ORIGIN, Block::from_mask(!(1 << 31)));
    game.insert_fine(unit(Level::A, 1, 0), Block::from_mask(!(1 << 24)));
    assert!(game.apply(tile(0, 0, 31), 8));
    assert!(game.block_at(1, 0).unwrap().is_complete());
    assert_eq!(game.block_at(0, -1).unwrap().mask(), (1 << 63) | 31);
}

#[test]
fn work_widens_across_the_just_completed_b() {
    let mut game = Game::new();
    // The first 4096 flips finish the origin B; four more reach the nearest
    // now open B and its nearest 1A using the original click's position.
    assert!(game.apply(tile(0, 0, 63), 4100));
    assert!(game.unit_complete(unit(Level::B, 0, 0)));
    assert_eq!(game.block_at(4, 0).unwrap().mask(), (0b1111 << 56));
    assert_eq!(game.cleared(Level::A), &Amount::from(64));
}

#[test]
fn the_sixty_fourth_a_spills_from_the_new_b_boundary_into_its_c() {
    let mut game = Game::empty();
    let b = unit(Level::B, 0, 0);
    let last = unit(Level::A, 3, 0);
    let mut cleared = [0; LEVELS];
    for coord in children(b, u64::MAX).filter(|&coord| coord != last) {
        assert!(game.restore_done(coord, false, &mut cleared));
    }
    game.credit(&cleared);
    game.insert_fine(last, Block::from_mask(!(1 << 31)));
    let mut events = Vec::new();
    assert!(game.apply_with_events(tile(3, 0, 31), 8, |event| events.push(event)));
    assert!(game.unit_complete(b));
    assert_eq!(game.block_at(4, 0).unwrap().mask(), 0b1111111 << 24);
    assert_eq!(game.cleared(Level::A), &Amount::from(64));
    assert_eq!(game.cleared(Level::B), &Amount::from(1));
    assert_eq!(events[0], BoardEvent::Flipped(tile(3, 0, 31)));
    assert_eq!(
        events[1],
        BoardEvent::Completed {
            coord: last,
            was_large: false
        }
    );
    assert_eq!(
        events[2],
        BoardEvent::Completed {
            coord: b,
            was_large: false
        }
    );
    assert!(
        events[3..35]
            .iter()
            .all(|event| matches!(event, BoardEvent::Opened(_)))
    );
    assert!(
        events[35..]
            .iter()
            .copied()
            .eq((24..31).map(|index| BoardEvent::Flipped(tile(4, 0, index))))
    );
}

#[test]
fn all_manual_upgrades_have_the_original_cost_and_budget() {
    let mut game = Game::new();
    let prices = [8, 8, 48, 8, 8, 8, 48, 8, 8, 8, 48, 8, 8, 8, 48, 8];
    let exponents = [0, 1, 2, 3, 6, 7, 8, 9, 12, 13, 14, 15, 18, 19, 20, 21, 24];
    for (stage, (level, price)) in [Level::A; 3]
        .into_iter()
        .chain([Level::B; 4])
        .chain([Level::C; 4])
        .chain([Level::D; 4])
        .chain([Level::E])
        .zip(prices)
        .enumerate()
    {
        let mut gain = [0; LEVELS];
        gain[level.index()] = price - 1;
        game.credit(&gain);
        assert_eq!(game.manual_power(), 1 << exponents[stage]);
        assert_eq!(game.next_upgrade(), Some((level, price)));
        assert_eq!(game.next_manual_power(), Some(1 << exponents[stage + 1]));
        assert!(!game.try_upgrade());
        gain[level.index()] = 1;
        game.credit(&gain);
        let cleared = game.cleared(level).clone();
        assert!(game.try_upgrade());
        assert_eq!(game.cleared(level), &cleared);
        assert_eq!(game.available(level).to_string(), "0");
    }
    assert_eq!(game.manual_power(), Level::D.tile_capacity());
    assert_eq!(game.next_upgrade(), None);
    assert_eq!(game.next_manual_power(), None);
    assert!(!game.try_upgrade());
}

#[test]
fn bulk_completion_folds_regions_and_credits_only_unfinished_descendants() {
    for level in [Level::B, Level::C, Level::D, Level::F, Level::H] {
        let mut game = Game::new();
        let inner = level.child().unwrap();
        assert!(game.apply(tile(0, 0, 0), inner.tile_capacity()));
        assert!(game.unit_complete(unit(inner, 0, 0)));
        let target = game.targets().next().unwrap();
        assert!(game.apply(target, level.tile_capacity() - inner.tile_capacity()));
        let completed = unit(level, 0, 0);
        assert!(game.unit_complete(completed));
        assert_eq!(
            game.nodes
                .keys()
                .filter(|coord| coord.ancestor_at(level) == Some(completed))
                .count(),
            1
        );
        for lower in Level::ALL.into_iter().take(level.index() + 1) {
            let counts = u128::from(level.tile_capacity() / lower.tile_capacity());
            assert_eq!(game.cleared(lower), &Amount::from(counts));
        }
        let bounds = completed.bounds_in(Level::A).unwrap();
        for at in [bounds.min(), bounds.max()] {
            assert_eq!(
                game.block_at(at.x(), at.y()),
                Some(Block::from_mask(u64::MAX))
            );
        }
        let next_large = match level {
            Level::D => Some(Level::B),
            Level::F => Some(Level::D),
            Level::H => Some(Level::F),
            _ => None,
        };
        if let Some(next_large) = next_large {
            assert_eq!(game.targets().count(), 32 * 64);
            assert!(game.targets().all(
                |target| matches!(target, Target::Large(coord) if coord.level() == next_large)
            ));
        }
        assert!(
            game.nodes.len() < 10_000,
            "a huge completed region must not be expanded"
        );
    }
}

#[test]
fn last_child_triggers_parent_chain_and_opens_after_it_gets_credit() {
    let mut game = Game::new();
    let h = unit(Level::H, 0, 0);
    let mut branch = h;
    // A chain of nearly complete branches down to a single white 1A.
    while branch.level() != Level::A {
        let last = branch.child(cell(36)).unwrap(); // (0,0) at every level
        for child in children(branch, u64::MAX).filter(|&child| child != last) {
            game.insert_leaf(child, Node::Done { group: false });
            game.mark_progress(child, child.level().tile_capacity());
            game.branch_mut(branch).done += 1;
            let mut counts = [0; LEVELS];
            count_region(&mut counts, child.level());
            game.credit(&counts);
        }
        branch = last;
    }
    let Some(Node::Fine { block, .. }) = game.nodes.get_mut(&ORIGIN) else {
        panic!("missing last block")
    };
    *block = Block::from_mask(!1);
    game.mark_progress(ORIGIN, 63);
    game.drag_item = DragItem::Held;
    assert!(game.activate(0, 0, cell(0)));
    assert!(game.unit_complete(h));
    for level in Level::ALL {
        assert_eq!(
            game.cleared(level),
            &Amount::from(u128::from(Level::H.tile_capacity() / level.tile_capacity()))
        );
    }
    for level in [
        Level::A,
        Level::B,
        Level::C,
        Level::D,
        Level::E,
        Level::F,
        Level::G,
    ] {
        assert!(!game.unit_observed(unit(level, 0, 0)));
    }
    assert_eq!(game.targets().count(), 32 * 64);
    assert!(
        game.targets()
            .all(|target| matches!(target, Target::Large(coord) if coord.level() == Level::F))
    );
}

#[test]
fn first_d_opens_ordered_b_groups_that_spill_in_wrapped_reading_order() {
    let mut game = Game::new();
    assert!(game.apply(tile(0, 0, 0), Level::D.tile_capacity()));
    let first_groups = [
        unit(Level::C, -5, -4),
        unit(Level::C, 4, -4),
        unit(Level::C, -4, -5),
        unit(Level::C, -4, 4),
    ];
    let initial: Vec<_> = game.targets().collect();
    assert_eq!(initial.len(), 32 * 64);
    for (set, group) in initial.chunks(64).zip(first_groups) {
        assert!(game.large_group(group));
        assert!(
            set.iter()
                .copied()
                .eq(children(group, u64::MAX).map(Target::Large))
        );
    }
    let group = first_groups[0];
    let outside = group.checked_offset(-1, 0).unwrap();
    assert!(game.can_open_large_group(outside));
    assert!(!game.unit_present(outside));
    let clicked = group.child(cell(63)).unwrap();
    let first = group.child(cell(0)).unwrap();
    let coord = first.bounds_in(Level::A).unwrap().min();
    assert!(game.apply(Target::Large(clicked), Level::B.tile_capacity() + 17));
    assert_eq!(game.cleared(Level::A), &Amount::from(64u128.pow(3) + 64));
    assert_eq!(game.cleared(Level::B), &Amount::from(64u128.pow(2) + 1));
    assert!(game.unit_complete(clicked));
    assert_eq!(game.large_at(coord.x(), coord.y()), Some((first, 17)));
    assert_eq!(game.block_at(coord.x(), coord.y()), None);
    assert!(!game.unit_present(outside)); // a single coarse completion opens nothing
    assert!(game.apply(
        Target::Large(first),
        Level::C.tile_capacity() - Level::B.tile_capacity() - 17
    ));
    assert!(game.unit_complete(group));
    assert!(game.large_group(group));
    assert!(game.large_group(outside));
    assert_eq!(game.cleared(Level::C), &Amount::from(65));
    assert_eq!(game.large_at(coord.x(), coord.y()), None);
    assert_eq!(
        game.block_at(coord.x(), coord.y()),
        Some(Block::from_mask(u64::MAX))
    );
}

#[test]
fn drag_item_waits_by_the_second_c_and_is_collected_with_its_unit() {
    let mut game = Game::new();
    let capacity = Level::C.tile_capacity();
    assert!(game.apply(tile(0, 0, 0), capacity));
    assert_eq!(game.item(), None);
    let second = unit(Level::C, 1, 0);
    assert!(game.apply(at(&game, second), capacity));
    let item = game.item().expect("second C spawns the item");
    assert!(neighbors(second).any(|near| near == item));
    assert!(!game.unit_complete(item));
    assert!(!game.has_drag_item());
    let click = game
        .targets()
        .find(|&target| match target {
            Target::Tile { x, y, .. } => unit(Level::A, x, y).ancestor_at(Level::C) == Some(item),
            Target::Large(_) => false,
        })
        .unwrap();
    assert!(game.apply(click, capacity));
    assert!(game.unit_complete(item));
    assert_eq!(game.item(), None);
    assert!(game.has_drag_item());
}

#[test]
fn optimized_boundary_matches_a_reading_of_each_base_edge_including_mixed_groups() {
    let completed = unit(Level::D, 0, 0);
    let mut game = Game::new();
    // Mixed groups preserve a touched ordinary B, rather than using a skip
    // whose range would forget that B's still-empty A boundary positions.
    let mixed = unit(Level::B, -37, -35);
    let first = mixed.bounds_in(Level::A).unwrap().min();
    let mut ref_game = Game::new();
    for state in [&mut game, &mut ref_game] {
        state.insert_fine(first.checked_offset(4, 4).unwrap(), Block::new());
        let node = state
            .nodes
            .insert(completed, Node::Done { group: false })
            .unwrap();
        state.fold(completed, node, &mut [0; LEVELS]);
        state.grow_bounds(completed);
        let mut credit = [0; LEVELS];
        count_region(&mut credit, Level::D);
        state.credit(&credit);
    }
    game.open_around(completed, &mut Events::default());
    let min = completed.bounds_in(Level::A).unwrap().min();
    let span = span_a(Level::D);
    for i in 0..span {
        for (dx, dy) in [(-1, i), (span, i), (i, -1), (i, span)] {
            ref_game.touch(min.checked_offset(dx, dy).unwrap(), &mut Events::default());
        }
    }
    assert_eq!(game.encode(), ref_game.encode());
    assert!(game.targets().any(
        |target| matches!(target, Target::Tile { x, y, .. } if x == -293 && y != first.y() + 4)
    ));
}

#[test]
fn save_round_trip_preserves_exact_balances_partial_work_item_and_target_order() {
    let mut game = Game::new();
    assert!(game.apply(tile(0, 0, 0), Level::D.tile_capacity()));
    let target = game.targets().nth(300).unwrap();
    assert!(game.apply(target, 129));
    assert!(game.try_upgrade());
    assert!(game.try_upgrade());
    assert!(game.try_spend(Level::D, &Amount::from(1)));
    let bytes = game.encode();
    let mut restored = Game::decode(&bytes).unwrap();
    assert_eq!(restored.encode(), bytes);
    assert_eq!(restored.bounds(), game.bounds());
    assert_eq!(restored.item(), game.item());
    assert_eq!(restored.available(Level::D).to_string(), "0");
    assert!(game.apply(target, 811));
    assert!(restored.apply(target, 811));
    assert_eq!(restored.encode(), game.encode());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(Game::decode(&trailing).is_err());
    // Truncation errors are bounded by available data even for count fields.
    for len in [0, 8, 10, 112, bytes.len() - 8, bytes.len() - 1] {
        assert!(Game::decode(&bytes[..len]).is_err());
    }
}

#[test]
fn native_debug_item_checkpoint_round_trips_and_requires_exactly_one_flip() {
    let game = Game::debug_before_item();
    assert_eq!(game.item(), Some(unit(Level::C, 2, 0)));
    assert_eq!(game.cleared(Level::A), &Amount::from(12_287));
    assert_eq!(game.cleared(Level::B), &Amount::from(191));
    assert_eq!(game.cleared(Level::C), &Amount::from(2));
    assert_eq!(game.manual_power(), 1);
    assert_eq!(game.targets().next(), Some(tile(92, 0, 27)));
    assert_eq!(game.block_at(92, 0), Some(Block::from_mask(!(1 << 27))));
    assert!(game.block_at(155, 0).is_some()); // the item C's right edge is finished, but the next C stays locked
    assert_eq!(game.block_at(156, 0), None);
    let bytes = game.encode();
    let mut game = Game::decode(&bytes).unwrap();
    assert_eq!(game.encode(), bytes);
    assert!(!game.activate(92, 0, cell(26)));
    assert!(game.activate(92, 0, cell(27)));
    assert!(game.has_drag_item());
    assert_eq!(game.item(), None);
    assert_eq!(game.cleared(Level::A), &Amount::from(12_288));
    assert_eq!(game.cleared(Level::B), &Amount::from(192));
    assert_eq!(game.cleared(Level::C), &Amount::from(3));
    assert_eq!(
        Game::decode(&game.encode()).unwrap().encode(),
        game.encode()
    );
}

#[test]
fn native_debug_first_d_checkpoint_round_trips_the_open_large_groups() {
    let game = Game::debug_after_1d();
    assert!(game.unit_complete(unit(Level::D, 0, 0)));
    assert_eq!(game.cleared(Level::D), &Amount::from(1));
    assert_eq!(game.cleared(Level::C), &Amount::from(64));
    assert_eq!(game.cleared(Level::B), &Amount::from(4096));
    assert_eq!(game.cleared(Level::A), &Amount::from(262_144));
    assert_eq!(game.units_len(Level::A), 0); // no implicit subdivision was materialized
    assert_eq!(game.targets().count(), 32 * 64);
    assert!(
        game.targets()
            .all(|target| matches!(target, Target::Large(coord) if coord.level() == Level::B))
    );
    let bytes = game.encode();
    let restored = Game::decode(&bytes).unwrap();
    assert!(game.targets().eq(restored.targets()));
    assert_eq!(restored.encode(), bytes);
}

#[test]
fn bonuses_unlock_tiers_without_opening_board_groups_and_are_saved_exactly() {
    let mut game = Game::new();
    assert!(game.tier_unlocked(Level::A));
    assert!(!game.tier_unlocked(Level::D));
    let bonus = Amount::machine_price(90).unwrap();
    assert!(game.add_debug_bonus(Level::D, &bonus));
    assert!(game.tier_unlocked(Level::D));
    assert!(game.try_spend(Level::D, &bonus));
    let bytes = game.encode();
    let mut game = Game::decode(&bytes).unwrap();
    assert_eq!(game.encode(), bytes);
    assert_eq!(game.bonus(Level::D), &bonus);
    assert!(game.tier_unlocked(Level::D));
    assert!(game.apply(tile(0, 0, 0), Level::C.tile_capacity()));
    assert!(
        game.targets()
            .all(|target| matches!(target, Target::Tile { .. }))
    );
    game.clear_debug_bonus();
    assert!(!game.tier_unlocked(Level::D));
    assert_eq!(game.available(Level::D).to_string(), "0");
}

#[test]
fn native_decode_rejects_a_fine_block_behind_a_locked_boundary() {
    let mut game = Game::new();
    game.insert_fine(unit(Level::A, 4, 0), Block::new());
    assert_eq!(
        Game::decode(&game.encode()).err().as_deref(),
        Some("board contains a fine block behind a locked boundary")
    );
}

#[test]
fn native_decode_respawns_a_reset_item_once_after_two_completed_cs() {
    let mut game = Game::debug_after_1d();
    game.set_drag_item(false);
    assert_eq!(game.item(), None);
    let mut game = Game::decode(&game.encode()).unwrap();
    let item = game
        .item()
        .expect("the first D already contains two cleared Cs");
    assert!(!game.unit_complete(item));
    let bounds = item.bounds_in(Level::A).unwrap();
    let [min_x, min_y, max_x, max_y] = game.bounds();
    assert!(min_x <= bounds.min().x() && min_y <= bounds.min().y());
    assert!(max_x >= bounds.max().x() && max_y >= bounds.max().y());
    let saved = game.encode();
    assert_eq!(Game::decode(&saved).unwrap().encode(), saved);
    game.set_drag_item(true);
    assert!(Game::decode(&game.encode()).unwrap().has_drag_item());

    // A compact checkpoint with only two adjacent Cs has no stored space
    // for any eligible neighbor; item recovery must grow its saved bounds.
    let mut compact = Game::empty();
    let mut cleared = [0; LEVELS];
    for x in [0, 1] {
        assert!(compact.restore_done(unit(Level::C, x, 0), false, &mut cleared));
    }
    compact.credit(&cleared);
    let game = Game::decode(&compact.encode()).unwrap();
    assert!(game.item().is_some());
    assert_ne!(game.bounds(), compact.bounds());
    assert_eq!(
        Game::decode(&game.encode()).unwrap().encode(),
        game.encode()
    );
}
