use aistatus_core::{
    Command, DoctorCommand, ProfileCommand, ProfileCommandAction, RefreshCommand, TuiCommand,
};

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Command {
    let mut iter = args.into_iter();
    let _bin = iter.next();
    match iter.next().as_deref() {
        None | Some("--help") | Some("-h") => Command::Help,
        Some("tui") => Command::Tui(parse_tui_args(iter)),
        Some("profile") => parse_profile_args(iter).map_or(Command::Help, Command::Profile),
        Some("auth") => Command::Auth,
        Some("refresh") => Command::Refresh(parse_refresh_args(iter)),
        Some("doctor") => Command::Doctor(parse_doctor_args(iter)),
        Some(_) => Command::Help,
    }
}

fn parse_refresh_args(args: impl IntoIterator<Item = String>) -> RefreshCommand {
    let mut command = RefreshCommand::default();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--config" => command.config = iter.next(),
            "--fixtures" => command.fixtures = iter.next(),
            "--force" => command.force = true,
            "--now-epoch-secs" => {
                command.now_epoch_secs = iter.next().and_then(|value| value.parse().ok())
            }
            _ => {}
        }
    }

    command
}

fn parse_doctor_args(args: impl IntoIterator<Item = String>) -> DoctorCommand {
    let mut command = DoctorCommand::default();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--config" => command.config = iter.next(),
            "--fixtures" => command.fixtures = iter.next(),
            _ => {}
        }
    }

    command
}

fn parse_tui_args(args: impl IntoIterator<Item = String>) -> TuiCommand {
    let mut command = TuiCommand::default();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        if arg == "--fixtures" {
            command.fixtures = iter.next();
        }
    }

    command
}

fn parse_profile_args(args: impl IntoIterator<Item = String>) -> Option<ProfileCommand> {
    let mut iter = args.into_iter();
    let subcommand = iter.next()?;
    let rest: Vec<String> = iter.collect();

    let config = option_value(&rest, "--config");
    let action = match subcommand.as_str() {
        "list" => ProfileCommandAction::List,
        "add" => ProfileCommandAction::Add {
            profile_id: required_value(&rest, "--id")?,
            display_name: required_value(&rest, "--name")?,
            auth_mode: required_value(&rest, "--auth-mode")?,
            account_kind: required_value(&rest, "--account-kind")?,
            provider: required_value(&rest, "--provider")?,
            membership_tier: option_value(&rest, "--membership-tier"),
            plan_type: option_value(&rest, "--plan-type"),
        },
        "edit" => ProfileCommandAction::Edit {
            profile_id: required_value(&rest, "--id")?,
            display_name: option_value(&rest, "--name"),
            refresh_interval_secs: option_value(&rest, "--refresh-interval-secs")
                .and_then(|value| value.parse().ok()),
            membership_tier: option_value(&rest, "--membership-tier"),
            plan_type: option_value(&rest, "--plan-type"),
        },
        "set-default" => ProfileCommandAction::SetDefault {
            profile_id: required_value(&rest, "--id")?,
        },
        "remove" => ProfileCommandAction::Remove {
            profile_id: required_value(&rest, "--id")?,
        },
        "login" => ProfileCommandAction::Login {
            profile_id: required_value(&rest, "--id")?,
            auth_mode: required_value(&rest, "--auth-mode")?,
            secret: required_value(&rest, "--secret")?,
            use_file_store: rest.iter().any(|arg| arg == "--file-store"),
        },
        "logout" => ProfileCommandAction::Logout {
            profile_id: required_value(&rest, "--id")?,
        },
        _ => return None,
    };

    Some(ProfileCommand { config, action })
}

fn option_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn required_value(args: &[String], flag: &str) -> Option<String> {
    option_value(args, flag)
}

#[cfg(test)]
mod tests {
    use super::parse_args;
    use aistatus_core::{Command, ProfileCommandAction};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn profile_add_missing_id_falls_back_to_help() {
        assert!(matches!(
            parse_args(args(&[
                "bin",
                "profile",
                "add",
                "--name",
                "Primary",
                "--auth-mode",
                "browser",
                "--account-kind",
                "chatgpt",
                "--provider",
                "codex_protocol"
            ])),
            Command::Help
        ));
    }

    #[test]
    fn profile_login_missing_secret_falls_back_to_help() {
        assert!(matches!(
            parse_args(args(&[
                "bin",
                "profile",
                "login",
                "--id",
                "acct-1",
                "--auth-mode",
                "browser"
            ])),
            Command::Help
        ));
    }

    #[test]
    fn profile_remove_does_not_treat_config_path_as_id() {
        assert!(matches!(
            parse_args(args(&[
                "bin",
                "profile",
                "remove",
                "--config",
                "/tmp/x.toml"
            ])),
            Command::Help
        ));
    }

    #[test]
    fn profile_logout_does_not_treat_config_path_as_id() {
        assert!(matches!(
            parse_args(args(&[
                "bin",
                "profile",
                "logout",
                "--config",
                "/tmp/x.toml"
            ])),
            Command::Help
        ));
    }

    #[test]
    fn profile_set_default_does_not_treat_config_path_as_id() {
        assert!(matches!(
            parse_args(args(&[
                "bin",
                "profile",
                "set-default",
                "--config",
                "/tmp/x.toml"
            ])),
            Command::Help
        ));
    }

    #[test]
    fn profile_remove_with_explicit_id_still_parses() {
        assert!(matches!(
            parse_args(args(&["bin", "profile", "remove", "--id", "acct-1"])),
            Command::Profile(profile) if matches!(profile.action, ProfileCommandAction::Remove { ref profile_id } if profile_id == "acct-1")
        ));
    }

    #[test]
    fn refresh_command_parses_real_options() {
        assert!(matches!(
            parse_args(args(&[
                "bin",
                "refresh",
                "--config",
                "/tmp/sample.toml",
                "--force",
                "--now-epoch-secs",
                "1234"
            ])),
            Command::Refresh(refresh)
                if refresh.config.as_deref() == Some("/tmp/sample.toml")
                    && refresh.force
                    && refresh.now_epoch_secs == Some(1234)
        ));
    }
}
