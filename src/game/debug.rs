use super::*;

impl Game {
    /// The original debug checkpoint: one last white tile before the drag item.
    pub fn debug_before_item() -> Self {
        let mut game = Self::empty();
        let item = Coord::new(Level::C, 2, 0);
        let last_b = Coord::new(Level::B, 12, 0);
        let last_a = Coord::new(Level::A, 92, 0);
        let mut cleared = [0; LEVELS];
        let mut reopen = Vec::new();
        for coord in children(last_b, u64::MAX) {
            if coord == last_a {
                game.insert_fine(coord, Block::from_mask(!(1 << 27)));
            } else {
                debug_restore(&mut game, coord, &mut cleared);
                reopen.push(coord);
            }
        }
        for coord in children(item, u64::MAX).filter(|&coord| coord != last_b) {
            debug_restore(&mut game, coord, &mut cleared);
            reopen.push(coord);
        }
        for x in [0, 1] {
            let coord = Coord::new(Level::C, x, 0);
            debug_restore(&mut game, coord, &mut cleared);
            reopen.push(coord);
        }
        game.credit(&cleared);
        game.drag_item = DragItem::Waiting(item);
        game.grow_bounds(item);
        let mut events = Events::default();
        for coord in reopen {
            game.open_around(coord, &mut events);
        }
        game
    }

    /// The original debug checkpoint: the first D is complete, showing the new large groups.
    pub fn debug_after_1d() -> Self {
        let mut game = Self::empty();
        let coord = Coord::new(Level::D, 0, 0);
        let mut cleared = [0; LEVELS];
        debug_restore(&mut game, coord, &mut cleared);
        game.credit(&cleared);
        game.update_drag_item(coord);
        game.open_around(coord, &mut Events::default());
        game
    }
}

fn debug_restore(game: &mut Game, coord: Coord, cleared: &mut [u128; LEVELS]) {
    let folded = game.restore_done(coord, false, cleared);
    debug_assert!(folded, "debug checkpoint is already compact");
}
