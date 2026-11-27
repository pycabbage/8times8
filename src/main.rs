use std::{env, process::ExitCode};

use eight_times_eight::{Block, CellIndex, Level};

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    match (args.next().as_deref(), args.next()) {
        (None, None) => help(),
        (Some(arg), None) if arg == "--help" || arg == "-h" => help(),
        (Some(arg), None) if arg == "--version" || arg == "-V" => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        }
        (Some(arg), None) if arg == "demo" => demo(),
        _ => {
            eprintln!("Invalid arguments. See eight-times-eight --help.");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

fn help() {
    println!(concat!(
        "8 times 8 — headless core demo\n\n",
        "Usage: eight-times-eight [demo | --help | --version]\n\n",
        "  demo       Flip all 64 tiles of one 1A block in two steps.\n",
        "  --help     Print this help.\n",
        "  --version  Print the package version."
    ));
}

fn demo() {
    let mut block = Block::new();
    let start = CellIndex::new(62).expect("slot 62 is inside 1A");
    for budget in [3, 61] {
        let flipped = block.flip_from(start, budget).len();
        println!(
            "1A: flipped {flipped} tiles; {}/{} flipped.",
            block.flipped_count(),
            Level::A.tile_capacity()
        );
    }
}
