use std::{env, process::ExitCode};

fn main() -> ExitCode {
    let mut args = env::args_os().skip(1);
    match (args.next().as_deref(), args.next()) {
        (None, None) => help(),
        (Some(arg), None) if arg == "--help" || arg == "-h" => help(),
        (Some(arg), None) if arg == "--version" || arg == "-V" => {
            println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        }
        _ => {
            eprintln!("Invalid arguments. See eight-times-eight --help.");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

fn help() {
    println!(concat!(
        "8 times 8 — headless CLI\n\n",
        "Usage: eight-times-eight [--help | --version]\n\n",
        "  --help     Print this help.\n",
        "  --version  Print the package version."
    ));
}
