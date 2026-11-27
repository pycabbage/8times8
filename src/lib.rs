//! Headless foundations for 8 times 8: integer coordinates, 1A tile work,
//! and exact per-level accounting. This crate does not yet implement a board.

mod block;
mod economy;
mod grid;

pub use block::{Block, Flips};
pub use economy::{Account, Amount, AmountParseError, Balance, Ledger, PaymentError};
pub use grid::{CellIndex, Coord, CoordBounds, Level};
