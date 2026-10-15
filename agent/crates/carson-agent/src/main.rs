//! carson-agent: the host-side daemon. Milestone 1 is a placeholder binary;
//! capture, encoding, input and the tray UI arrive in milestones 2–4.

use std::process::ExitCode;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!(
                "carson-agent {VERSION} (protocol v{})",
                carson_proto::PROTOCOL_VERSION
            );
            ExitCode::SUCCESS
        }
        None | Some("--help" | "-h") => {
            println!("carson-agent {VERSION}\n\nUSAGE:\n    carson-agent [--version | --help]");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("carson-agent: unknown argument {other:?} (try --help)");
            ExitCode::from(2)
        }
    }
}
