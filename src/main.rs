use std::process::ExitCode;

mod client;

fn main() -> ExitCode {
    match client::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("8times8: {error}");
            ExitCode::FAILURE
        }
    }
}
