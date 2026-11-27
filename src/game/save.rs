use super::*;

const MAGIC: &[u8] = b"8T8BOARD\x01";

impl Game {
    /// Encodes the native board and economy; callers store camera and machine state separately.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        bytes.push(self.manual_stage as u8);
        match self.drag_item {
            DragItem::Hidden => bytes.push(0),
            DragItem::Waiting(coord) => {
                bytes.push(1);
                put_coord(&mut bytes, coord);
            }
            DragItem::Held => bytes.push(2),
        }
        for axis in self.bounds {
            bytes.extend(axis.to_le_bytes());
        }
        for n in self.observed {
            bytes.extend((n as u64).to_le_bytes());
        }
        for level in Level::ALL {
            let account = self.ledger.account(level);
            for amount in [account.bonus(), account.spent()] {
                let digits = amount.to_string();
                bytes.extend((digits.len() as u16).to_le_bytes());
                bytes.extend(digits.as_bytes());
            }
        }

        let mut done = Vec::new();
        let mut groups = Vec::new();
        for (&coord, &node) in &self.nodes {
            match node {
                Node::Done { group } => done.push((coord, group)),
                Node::Branch(Branch { group: true, .. }) => groups.push(coord),
                _ => {}
            }
        }
        let sort_key = |coord: Coord| (coord.level(), coord.y(), coord.x());
        done.sort_unstable_by_key(|&(coord, _)| sort_key(coord));
        groups.sort_unstable_by_key(|&coord| sort_key(coord));
        bytes.extend(((done.len() + self.pending.len()) as u64).to_le_bytes());
        for (coord, group) in done {
            bytes.push(if group { 3 } else { 2 });
            put_coord(&mut bytes, coord);
        }
        for &coord in self.pending.values() {
            let (kind, state) = match self.nodes[&coord] {
                Node::Fine { block, .. } => (0, block.mask()),
                Node::Large { flipped, .. } => (1, flipped),
                _ => unreachable!("only live leaves are indexed"),
            };
            bytes.push(kind);
            put_coord(&mut bytes, coord);
            bytes.extend(state.to_le_bytes());
        }
        bytes.extend((groups.len() as u64).to_le_bytes());
        for coord in groups {
            put_coord(&mut bytes, coord);
        }
        bytes
    }

    /// Reads a native board save and verifies the sparse tree can be rebuilt.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        decode(bytes).map_err(str::to_owned)
    }
}

fn decode(bytes: &[u8]) -> Result<Game, &'static str> {
    let mut input = Input(
        bytes
            .strip_prefix(MAGIC)
            .ok_or("unknown board save version")?,
    );
    let mut game = Game::empty();
    game.manual_stage = usize::from(input.byte()?);
    if game.manual_stage > MANUAL.len() {
        return Err("invalid manual stage");
    }
    game.drag_item = match input.byte()? {
        0 => DragItem::Hidden,
        1 => DragItem::Waiting(input.coord()?),
        2 => DragItem::Held,
        _ => return Err("unknown drag item state"),
    };
    if let DragItem::Waiting(coord) = game.drag_item {
        if coord.level() != Level::C {
            return Err("invalid drag item position");
        }
        game.grow_bounds(coord);
    }
    let mut bounds = [0; 4];
    for axis in &mut bounds {
        *axis = i64::from_le_bytes(input.bytes()?);
    }
    let mut observed = [0; LEVELS];
    for n in &mut observed {
        *n = input
            .u64()?
            .try_into()
            .map_err(|_| "unit count out of range")?;
    }
    for level in Level::ALL {
        let bonus = input.amount()?;
        let spent = input.amount()?;
        game.ledger
            .set_account(level, Account::new(Amount::ZERO, bonus, spent));
    }

    let leaves = input.count(18)?;
    let mut cleared = [0; LEVELS];
    let mut item_source: Option<Coord> = None;
    for _ in 0..leaves {
        let kind = input.byte()?;
        let coord = input.coord()?;
        if !game.fresh(coord) {
            return Err("overlapping or duplicate board records");
        }
        match kind {
            0 if coord.level() == Level::A => {
                let mask = input.u64()?;
                if mask == u64::MAX {
                    return Err("complete block saved as playable");
                }
                game.insert_fine(coord, Block::from_mask(mask));
            }
            1 if matches!(coord.level(), Level::B | Level::D | Level::F) => {
                let flipped = input.u64()?;
                if flipped >= coord.level().tile_capacity() {
                    return Err("invalid large tile progress");
                }
                game.insert_large(coord, flipped);
            }
            2 | 3 if kind == 2 || group_level(coord) => {
                if !game.restore_done(coord, kind == 3, &mut cleared) {
                    return Err("unfolded complete region");
                }
                if coord.level() >= Level::C
                    && item_source.is_none_or(|c| c.level() < coord.level())
                {
                    item_source = Some(coord);
                }
            }
            _ => return Err("unknown or incompatible board record"),
        }
    }
    for _ in 0..input.count(17)? {
        let coord = input.coord()?;
        if !group_level(coord)
            || !matches!(game.nodes.get(&coord), Some(Node::Branch(branch)) if !branch.group)
        {
            return Err("missing or duplicate large tile group");
        }
        game.update_group(coord);
    }
    if !input.0.is_empty() {
        return Err("unexpected data after board save");
    }
    for (&coord, node) in &game.nodes {
        if matches!(node, Node::Large { .. })
            && !coord.parent().is_some_and(|p| game.large_group(p))
        {
            return Err("large tile is missing its group");
        }
    }
    if game.covering_region(ORIGIN).is_none() {
        return Err("board is missing the starting block");
    }
    if game.has_locked_fine() {
        return Err("board contains a fine block behind a locked boundary");
    }
    if game
        .observed
        .iter()
        .zip(observed)
        .any(|(actual, saved)| *actual > saved)
    {
        return Err("unit history is smaller than the current board");
    }
    game.observed = observed;
    game.credit(&cleared);
    if game.item().is_some_and(|item| game.unit_complete(item)) {
        return Err("waiting item is already collected");
    }
    if bounds[0] > game.bounds[0]
        || bounds[1] > game.bounds[1]
        || bounds[2] < game.bounds[2]
        || bounds[3] < game.bounds[3]
    {
        return Err("saved bounds do not cover the board");
    }
    game.bounds = bounds;
    if game.drag_item == DragItem::Hidden
        && let Some(coord) = item_source
    {
        game.update_drag_item(coord);
    }
    Ok(game)
}

fn group_level(coord: Coord) -> bool {
    matches!(coord.level(), Level::C | Level::E | Level::G)
}

fn put_coord(bytes: &mut Vec<u8>, coord: Coord) {
    bytes.push(coord.level().index() as u8);
    bytes.extend(coord.x().to_le_bytes());
    bytes.extend(coord.y().to_le_bytes());
}

struct Input<'a>(&'a [u8]);

impl<'a> Input<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let (head, tail) = self.0.split_at_checked(n).ok_or("truncated board save")?;
        self.0 = tail;
        Ok(head)
    }

    fn bytes<const N: usize>(&mut self) -> Result<[u8; N], &'static str> {
        self.take(N)?.try_into().map_err(|_| "truncated board save")
    }

    fn byte(&mut self) -> Result<u8, &'static str> {
        Ok(self.bytes::<1>()?[0])
    }

    fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }

    fn count(&mut self, record_min: usize) -> Result<usize, &'static str> {
        let n = usize::try_from(self.u64()?).map_err(|_| "board record count out of range")?;
        if n > self.0.len() / record_min {
            return Err("board record count exceeds remaining data");
        }
        Ok(n)
    }

    fn coord(&mut self) -> Result<Coord, &'static str> {
        let level = *Level::ALL
            .get(usize::from(self.byte()?))
            .ok_or("invalid board level")?;
        let coord = Coord::new(
            level,
            i64::from_le_bytes(self.bytes()?),
            i64::from_le_bytes(self.bytes()?),
        );
        coord
            .bounds_in(Level::A)
            .ok_or("board coordinate out of range")?;
        Ok(coord)
    }

    fn amount(&mut self) -> Result<Amount, &'static str> {
        let len = usize::from(u16::from_le_bytes(self.bytes()?));
        let digits = std::str::from_utf8(self.take(len)?).map_err(|_| "invalid point count")?;
        digits.parse().map_err(|_| "invalid point count")
    }
}
