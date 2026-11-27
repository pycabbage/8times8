//! Game rules for 8times8: the 1A/1B board, manual play, integer coordinates,
//! and exact per-level accounting.

mod block;
mod economy;
mod game;
mod grid;

pub use block::{Block, Flips};
pub use economy::{Account, Amount, AmountParseError, Balance, Ledger, PaymentError};
pub use game::Game;
pub use grid::{CellIndex, Coord, CoordBounds, Level};
