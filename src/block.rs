use std::iter::FusedIterator;

use crate::CellIndex;

/// One 1A block: each set bit is a flipped tile in row-major order.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Block {
    flipped: u64,
}

impl Block {
    pub const fn new() -> Self {
        Self::from_mask(0)
    }

    pub const fn from_mask(flipped: u64) -> Self {
        Self { flipped }
    }

    pub const fn mask(self) -> u64 {
        self.flipped
    }

    pub const fn flipped_count(self) -> u32 {
        self.flipped.count_ones()
    }

    pub const fn is_complete(self) -> bool {
        self.flipped == u64::MAX
    }

    /// Flips up to `budget` white tiles in one cyclic pass, starting at `start`.
    /// The block is updated immediately. The returned iterator reports the order
    /// without borrowing the block or allocating.
    pub fn flip_from(&mut self, start: CellIndex, budget: u64) -> Flips {
        let rotation = u32::from(start.index());
        let white = (!self.flipped).rotate_right(rotation);
        let selected = if budget >= u64::from(white.count_ones()) {
            white
        } else {
            let mut remaining = white;
            for _ in 0..budget {
                remaining &= remaining - 1;
            }
            white ^ remaining
        };
        self.flipped |= selected.rotate_left(rotation);
        Flips {
            start,
            remaining: selected,
        }
    }
}

/// The freshly flipped tiles of one operation, in work order.
#[derive(Clone, Debug)]
pub struct Flips {
    start: CellIndex,
    remaining: u64,
}

impl Iterator for Flips {
    type Item = CellIndex;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let offset = self.remaining.trailing_zeros() as u8;
        self.remaining &= self.remaining - 1;
        CellIndex::new((self.start.index() + offset) % 64)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let length = self.remaining.count_ones() as usize;
        (length, Some(length))
    }
}

impl ExactSizeIterator for Flips {}
impl FusedIterator for Flips {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_at_62_wraps_in_row_major_order() {
        let mut block = Block::new();
        let flips = block.flip_from(CellIndex::new(62).unwrap(), 3);
        assert_eq!(flips.len(), 3);
        assert!(flips.map(CellIndex::index).eq([62, 63, 0]));
        assert_eq!(block.mask(), (1 << 62) | (1 << 63) | 1);
        assert_eq!(block.flipped_count(), 3);
    }

    #[test]
    fn skips_black_tiles_including_an_already_flipped_origin() {
        let mask = (1 << 62) | 1;
        let mut block = Block::from_mask(mask);
        let start = CellIndex::new(62).unwrap();
        assert!(block.flip_from(start, 2).map(CellIndex::index).eq([63, 1]));
        assert!(block.flip_from(start, 2).map(CellIndex::index).eq([2, 3]));
        assert_eq!(block.mask(), mask | (1 << 63) | 0b1110);
    }

    #[test]
    fn zero_budget_is_a_noop() {
        let mut block = Block::from_mask(1);
        let mut flips = block.flip_from(CellIndex::new(1).unwrap(), 0);
        assert_eq!(flips.len(), 0);
        assert_eq!(flips.next(), None);
        assert_eq!(block.mask(), 1);
    }

    #[test]
    fn a_budget_of_64_visits_each_tile_exactly_once() {
        let mut block = Block::new();
        let start = CellIndex::new(1).unwrap();
        let flips = block.flip_from(start, 64);
        assert_eq!(flips.len(), 64);
        assert!(flips.map(CellIndex::index).eq((1..64).chain([0])));
        assert!(block.is_complete());
        assert_eq!(block.flipped_count(), 64);
        assert_eq!(block.flip_from(start, 64).next(), None);
    }

    #[test]
    fn oversized_budget_stops_within_one_pass() {
        let mut block = Block::from_mask(!((1 << 63) | (1 << 3)));
        let start = CellIndex::new(62).unwrap();
        assert!(
            block
                .flip_from(start, u64::MAX)
                .map(CellIndex::index)
                .eq([63, 3])
        );
        assert!(block.is_complete());
        assert_eq!(block.flip_from(start, u64::MAX).next(), None);
    }
}
