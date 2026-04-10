use aistatus_app::cli::parse_args;
use aistatus_app::dispatch_command as dispatch_refresh_command;
use aistatus_auth::{dispatch_command, dispatch_profile_command};
use aistatus_core::{Command, command_names};
use aistatus_tui::{TuiModel, load_fixture, run_fixture_tui};

fn main() {
    let command = parse_args(std::env::args());
    match command {
        Command::Help => print_help(),
        Command::Tui(tui) => {
            let fixture_name = tui.fixtures.unwrap_or_else(|| "sample-quotas".into());
            let fixture_path = format!("crates/tui/tests/fixtures/{fixture_name}.json");
            let fixture = std::fs::read_to_string(&fixture_path).unwrap_or_else(|error| {
                eprintln!("failed to read tui fixture `{fixture_path}`: {error}");
                std::process::exit(1);
            });
            let loaded = load_fixture(&fixture).unwrap_or_else(|error| {
                eprintln!("failed to load tui fixture `{fixture_path}`: {error}");
                std::process::exit(1);
            });
            let mut model = TuiModel::new("aistatus", loaded.state)
                .with_refresh_command(loaded.refresh_command);
            if let Err(error) = run_fixture_tui(&mut model) {
                eprintln!("tui failed: {error}");
                std::process::exit(1);
            }
        }
        Command::Profile(profile) => match dispatch_profile_command(&Command::Profile(profile)) {
            Some(Ok(output)) => println!("{}", output.render()),
            Some(Err(error)) => {
                eprintln!("profile command failed: {error}");
                std::process::exit(1);
            }
            None => unreachable!("profile dispatch should always return output"),
        },
        Command::Auth => println!("auth scaffold"),
        Command::Refresh(refresh) => match dispatch_refresh_command(&Command::Refresh(refresh)) {
            Some(Ok(output)) => println!("{}", output.render()),
            Some(Err(error)) => {
                eprintln!("refresh failed: {error}");
                std::process::exit(1);
            }
            None => unreachable!("refresh dispatch should always return output"),
        },
        Command::Doctor(doctor) => match dispatch_command(&Command::Doctor(doctor)) {
            Some(Ok(report)) => println!("{}", report.render()),
            Some(Err(error)) => {
                eprintln!("doctor failed: {error}");
                std::process::exit(1);
            }
            None => unreachable!("doctor dispatch should always return a report"),
        },
    }
}

fn print_help() {
    println!("aistatus");
    println!("USAGE: aistatus <SUBCOMMAND>");
    println!("SUBCOMMANDS:");
    for name in command_names() {
        println!("  {name}");
    }
}
