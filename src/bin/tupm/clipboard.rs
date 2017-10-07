//! This module provides several platform-specific means of copying data to the clipboard.
//!
//! On Linux in X11 environments, the `xsel` or `xclip` command (depending on availability) will be
//! used.  On Mac OS, the `pbcopy` command will be used.

extern crate upm;

use std::env;
use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::process;
use base64;

/// The environment variable used to store the system path.
static PATH_ENV: &'static str = "PATH";
/// The environment variable used to store the X11 display.  If this environment variable is not
/// set, we assume that we are not running in an X11 environment.
#[cfg(target_os = "linux")]
static DISPLAY_ENV: &'static str = "DISPLAY";
/// The name of the Mac OS `pbcopy` command used to copy data to the clipboard.
#[cfg(target_os = "macos")]
static PBCOPY_COMMAND: &'static str = "pbcopy";
/// The name of the X11 `xsel` command used to copy data to the clipboard.
#[cfg(target_os = "linux")]
static XSEL_COMMAND: &'static str = "xsel";
/// The name of the X11 `xclip` command used to copy data to the clipboard.
#[cfg(target_os = "linux")]
static XCLIP_COMMAND: &'static str = "xclip";

/// Attempt to find the specified command on the path.
fn find_in_path(name: &str) -> Option<PathBuf> {
    env::var_os(PATH_ENV).and_then(|p| {
        env::split_paths(&p)
            .filter_map(|d| {
                let candidate = d.join(&name);
                if candidate.is_file() {
                    Some(candidate)
                } else {
                    None
                }
            })
            .next()
    })
}

/// Return the platform-specific external command used to copy data to the clipboard.
#[cfg(target_os = "macos")]
fn clipboard_command() -> Result<process::Command, String> {
    match find_in_path(PBCOPY_COMMAND) {
        Some(path) => Ok(process::Command::new(path)),
        None => Err("Cannot find pbcopy command in path.".to_string()),
    }
}

/// Return the platform-specific external command used to copy data to the clipboard.
#[cfg(target_os = "linux")]
fn clipboard_command() -> Result<process::Command, String> {
    if env::var_os(DISPLAY_ENV).is_none() {
        return Err("Non-X11 environments not supported.".to_string());
    }

    match find_in_path(XSEL_COMMAND) {
        Some(path) => {
            let mut command = process::Command::new(path);
            command.arg("-ib");
            Ok(command)
        }
        None => {
            match find_in_path(XCLIP_COMMAND) {
                Some(path) => {
                    let mut command = process::Command::new(path);
                    command.arg("-selection");
                    command.arg("clipboard");
                    Ok(command)
                }
                None => Err(format!("Cannot find xsel or xclip command in path.")),
            }
        }
    }
}

/// Return the platform-specific external command used to copy data to the clipboard.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn clipboard_command() -> Result<process::Command, String> {
    Err(
        "Clipboard support not implemented for this operating system.".to_string(),
    )
}

// Copy to clipboard using xterm-style using xterm-style OSC 52
// escape sequences, as specified in:
// http://invisible-island.net/xterm/ctlseqs/ctlseqs.html
fn clipboard_osc52(text: &str) {
    fn is_screen() -> bool {
        match env::var("TERM") {
            Ok(t) => t.starts_with("screen"),
            Err(_) => false,
        }
    }

    if ! is_screen() {
        // The simple case: embed a Base64 representation in the OSC 52
        // escape sequence.
        let data = base64::encode(&text);
        print!("\x1B]52;c;{}\x07", data);
        io::stdout().flush().unwrap();
    } else {
        // If using screen, we require chunking to pass through the data
        // to the upper-level terminal emulation.

        // Wrap every 76 characters of Base64 output, same as the Linux
        // base64 command.
        const WRAP_CHARS: usize = 76;
        let data = base64::encode(&text);
        let mut pos = 0usize;
        let total_length = data.len();
        let mut first: bool = true;
        loop {
            // Get the next slice
            let slice_top = if pos+WRAP_CHARS <= total_length {
                pos+WRAP_CHARS
            } else {
                total_length
            };
            let slice = &data[pos..slice_top];

            // Output the slice
            if first {
                first = false;
                print!("\x1BP\x1B]52;c;{}", slice);
                io::stdout().flush().unwrap();
            } else {
                print!("\x1B\x5C\x1BP{}", slice);
                io::stdout().flush().unwrap();
            }

            pos += WRAP_CHARS;
            if pos >= total_length {
                break;
            }
        }
        print!("\x07\x1B\\");
        io::stdout().flush().unwrap();
    }
}

/// Copy the provided string to the clipboard, if possible.
pub fn clipboard_copy(text: &str) -> Result<(), String> {
    // Use OSC 52 for clipboard copy, but only if this is enabled via
    // the OSC52 environment variable.
    if let Ok(_) = env::var("OSC52") {
        clipboard_osc52(text);
        return Ok(());
    }

    let mut command = match clipboard_command() {
        Ok(command) => command,
        Err(e) => return Err(e),
    };

    let process = match command
        .stdin(process::Stdio::piped())
        .stdout(process::Stdio::null())
        .stderr(process::Stdio::null())
        .spawn() {
        Err(e) => {
            return Err(format!("Cannot spawn clipboard copy command: {}", e));
        }
        Ok(process) => process,
    };

    match process.stdin.unwrap().write_all(text.as_bytes()) {
        Err(e) => Err(format!("Cannot write to clipboard helper: {}", e)),
        Ok(_) => Ok(()),
    }
}
