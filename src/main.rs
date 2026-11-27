#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::process::ExitCode;

mod client;

fn main() -> ExitCode {
    match client::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report_error(&error);
            ExitCode::FAILURE
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn report_error(error: &str) {
    eprintln!("8times8: {error}");
}

#[cfg(target_os = "windows")]
fn report_error(error: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW,
    };

    let text: Vec<u16> = error
        .replace('\0', "\u{fffd}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // Both UTF-16 strings are nul-terminated and live for the duration of the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            windows_sys::w!("8times8"),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}
