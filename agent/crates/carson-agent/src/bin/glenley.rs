//! `glenley`: the tray app. A GUI program on Windows (no console window); with arguments it
//! behaves like `glenley-agent`.
#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    carson_agent::main_entry(true)
}
