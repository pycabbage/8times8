/// The eight logical levels, from a single block (1A) to the largest unit (1H).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Level {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
}

impl Level {
    pub const ALL: [Self; 8] = [
        Self::A,
        Self::B,
        Self::C,
        Self::D,
        Self::E,
        Self::F,
        Self::G,
        Self::H,
    ];

    pub(crate) const fn index(self) -> usize {
        self as usize
    }

    pub const fn parent(self) -> Option<Self> {
        if self.index() + 1 < Self::ALL.len() {
            Some(Self::ALL[self.index() + 1])
        } else {
            None
        }
    }

    pub const fn child(self) -> Option<Self> {
        if self.index() == 0 {
            None
        } else {
            Some(Self::ALL[self.index() - 1])
        }
    }

    /// The number of units of `inner` along one edge, or `None` for a coarser `inner`.
    /// Use `Level::A` for the span in 1A blocks.
    pub const fn span_in(self, inner: Self) -> Option<u64> {
        if inner.index() <= self.index() {
            Some(1 << (3 * (self.index() - inner.index())))
        } else {
            None
        }
    }

    /// The number of individual tiles in a complete unit of this level.
    pub const fn tile_capacity(self) -> u64 {
        1 << (6 * (self.index() + 1))
    }
}

/// A row-major slot in an 8×8 grid, either a 1A tile or an immediate child unit.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CellIndex(u8);

impl CellIndex {
    pub const fn new(index: u8) -> Option<Self> {
        if index < 64 { Some(Self(index)) } else { None }
    }

    pub const fn from_col_row(col: u8, row: u8) -> Option<Self> {
        if col < 8 && row < 8 {
            Some(Self(row * 8 + col))
        } else {
            None
        }
    }

    pub const fn index(self) -> u8 {
        self.0
    }

    pub const fn col(self) -> u8 {
        self.0 % 8
    }

    pub const fn row(self) -> u8 {
        self.0 / 8
    }
}

/// An integer key whose axes are measured in units of its own level.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Coord {
    level: Level,
    x: i64,
    y: i64,
}

impl Coord {
    pub const fn new(level: Level, x: i64, y: i64) -> Self {
        Self { level, x, y }
    }

    pub const fn level(self) -> Level {
        self.level
    }

    pub const fn x(self) -> i64 {
        self.x
    }

    pub const fn y(self) -> i64 {
        self.y
    }

    pub fn checked_offset(self, dx: i64, dy: i64) -> Option<Self> {
        Some(Self::new(
            self.level,
            self.x.checked_add(dx)?,
            self.y.checked_add(dy)?,
        ))
    }

    pub fn parent(self) -> Option<Self> {
        let axis = |u| i64::try_from((i128::from(u) + 4).div_euclid(8)).ok();
        Some(Self::new(
            self.level.parent()?,
            axis(self.x)?,
            axis(self.y)?,
        ))
    }

    /// Returns the containing coordinate at `level`, including `self` at the same level.
    pub fn ancestor_at(mut self, level: Level) -> Option<Self> {
        if level < self.level {
            return None;
        }
        while self.level < level {
            self = self.parent()?;
        }
        Some(self)
    }

    /// Returns a selected child if both axes of that child fit in `i64`.
    pub fn child(self, cell: CellIndex) -> Option<Self> {
        let axis = |u, pos| i64::try_from(i128::from(u) * 8 - 4 + i128::from(pos)).ok();
        Some(Self::new(
            self.level.child()?,
            axis(self.x, cell.col())?,
            axis(self.y, cell.row())?,
        ))
    }

    /// The full inclusive rectangle in units of `inner`, or `None` if either
    /// axis endpoint is out of range or `inner` is coarser than this coordinate.
    pub fn bounds_in(self, inner: Level) -> Option<CoordBounds> {
        let span = i128::from(self.level.span_in(inner)?);
        // After k inverse steps the offset is -4 * (1 + 8 + ... + 8^(k-1)).
        // k <= 7, so these intermediates fit in i128 for every i64 axis.
        let offset = 4 * (span - 1) / 7;
        let x = i128::from(self.x) * span - offset;
        let y = i128::from(self.y) * span - offset;
        let point = |x, y| {
            Some(Self::new(
                inner,
                i64::try_from(x).ok()?,
                i64::try_from(y).ok()?,
            ))
        };
        Some(CoordBounds {
            min: point(x, y)?,
            max: point(x + span - 1, y + span - 1)?,
        })
    }
}

/// Inclusive integer bounds at the level of the two endpoints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoordBounds {
    min: Coord,
    max: Coord,
}

impl CoordBounds {
    pub const fn min(self) -> Coord {
        self.min
    }

    pub const fn max(self) -> Coord {
        self.max
    }

    pub fn contains(self, point: Coord) -> bool {
        point.level == self.min.level
            && (self.min.x..=self.max.x).contains(&point.x)
            && (self.min.y..=self.max.y).contains(&point.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_checked_and_row_major() {
        for index in 0..64 {
            let cell = CellIndex::new(index).unwrap();
            assert_eq!(cell.index(), index);
            assert_eq!(CellIndex::from_col_row(index % 8, index / 8), Some(cell));
        }
        assert_eq!(CellIndex::new(64), None);
        assert_eq!(CellIndex::new(u8::MAX), None);
        assert_eq!(CellIndex::from_col_row(8, 0), None);
        assert_eq!(CellIndex::from_col_row(0, 8), None);
    }

    #[test]
    fn each_level_has_the_expected_span_and_capacity() {
        let spans = [1, 8, 64, 512, 4096, 32_768, 262_144, 2_097_152];
        let capacities = [
            64,
            4096,
            262_144,
            16_777_216,
            1_073_741_824,
            68_719_476_736,
            4_398_046_511_104,
            281_474_976_710_656,
        ];
        for (i, level) in Level::ALL.into_iter().enumerate() {
            assert_eq!(level.span_in(Level::A), Some(spans[i]));
            assert_eq!(level.tile_capacity(), capacities[i]);
            assert_eq!(level.parent(), Level::ALL.get(i + 1).copied());
            assert_eq!(level.child(), i.checked_sub(1).map(|j| Level::ALL[j]));
            for (j, inner) in Level::ALL.into_iter().enumerate() {
                assert_eq!(level.span_in(inner), i.checked_sub(j).map(|k| spans[k]));
            }
        }
    }

    #[test]
    fn parent_uses_euclidean_division_at_signed_boundaries() {
        for (axis, parent) in [(-13, -2), (-12, -1), (-5, -1), (-4, 0), (3, 0), (4, 1)] {
            for pair in Level::ALL.windows(2) {
                assert_eq!(
                    Coord::new(pair[0], axis, axis).parent(),
                    Some(Coord::new(pair[1], parent, parent))
                );
            }
        }
        assert_eq!(Coord::new(Level::H, 0, 0).parent(), None);
    }

    #[test]
    fn every_child_round_trips_to_its_parent() {
        for pair in Level::ALL.windows(2) {
            let parent = Coord::new(pair[1], -2, 1);
            for i in 0..64 {
                let child = parent.child(CellIndex::new(i).unwrap()).unwrap();
                assert_eq!(
                    child,
                    Coord::new(pair[0], -20 + i64::from(i % 8), 4 + i64::from(i / 8))
                );
                assert_eq!(child.parent(), Some(parent));
            }
        }
        let base = Coord::new(Level::A, 0, 0);
        assert_eq!(base.child(CellIndex::new(0).unwrap()), None);
        assert_eq!(Coord::new(Level::B, 0, 0).ancestor_at(Level::A), None);
    }

    #[test]
    fn bounds_are_inclusive_at_every_depth() {
        let offsets = [0, 4, 36, 292, 2_340, 18_724, 149_796, 1_198_372];
        for (i, level) in Level::ALL.into_iter().enumerate() {
            let outer = Coord::new(level, 1, -1);
            for (j, inner) in Level::ALL.into_iter().enumerate() {
                let Some(depth) = i.checked_sub(j) else {
                    assert_eq!(outer.bounds_in(inner), None);
                    continue;
                };
                let span = 8i64.pow(depth as u32);
                let first_x = span - offsets[depth];
                let first_y = -span - offsets[depth];
                let bounds = outer.bounds_in(inner).unwrap();
                assert_eq!(bounds.min(), Coord::new(inner, first_x, first_y));
                assert_eq!(
                    bounds.max(),
                    Coord::new(inner, first_x + span - 1, first_y + span - 1)
                );
                for point in [bounds.min(), bounds.max()] {
                    assert!(bounds.contains(point));
                    assert_eq!(point.ancestor_at(level), Some(outer));
                }
                for point in [
                    bounds.min().checked_offset(-1, 0),
                    bounds.min().checked_offset(0, -1),
                    bounds.max().checked_offset(1, 0),
                    bounds.max().checked_offset(0, 1),
                ] {
                    let point = point.unwrap();
                    assert!(!bounds.contains(point));
                    assert_ne!(point.ancestor_at(level), Some(outer));
                }
            }
        }
        let bounds = Coord::new(Level::B, 0, 0).bounds_in(Level::A).unwrap();
        assert!(!bounds.contains(Coord::new(Level::B, 0, 0)));
    }

    #[test]
    fn extreme_axes_are_checked_without_discarding_representable_children() {
        let edge = Coord::new(Level::A, i64::MIN, i64::MAX);
        let parent = edge.parent().unwrap();
        assert_eq!(parent, Coord::new(Level::B, -(1 << 60), 1 << 60));
        assert_eq!(
            parent.child(CellIndex::from_col_row(4, 3).unwrap()),
            Some(edge)
        );
        assert_eq!(parent.child(CellIndex::from_col_row(3, 3).unwrap()), None);
        assert_eq!(parent.child(CellIndex::from_col_row(4, 4).unwrap()), None);
        assert_eq!(parent.bounds_in(Level::A), None);
        assert_eq!(edge.bounds_in(Level::A).unwrap().min(), edge);
        assert_eq!(edge.checked_offset(-1, 0), None);
        assert_eq!(edge.checked_offset(0, 1), None);

        let inside = Coord::new(Level::B, parent.x() + 1, parent.y() - 1);
        let bounds = inside.bounds_in(Level::A).unwrap();
        assert_eq!(bounds.min().x(), i64::MIN + 4);
        assert_eq!(bounds.max().y(), i64::MAX - 4);
        assert_eq!(
            Coord::new(Level::H, i64::MAX, i64::MIN).bounds_in(Level::A),
            None
        );
        assert_eq!(
            Coord::new(Level::H, i64::MIN, i64::MAX).bounds_in(Level::A),
            None
        );
    }
}
