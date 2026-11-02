mod args;
mod hub;
mod limits;
mod notify;
mod session;
mod signin;
mod text;
mod waybar;

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let code = match args::parse(&arguments) {
        Ok(args::Command::Help) => {
            println!("{}", args::help());
            0
        }
        Ok(command) => dispatch(command),
        Err(error) => {
            text::error(&format!("{error}\nRun 'openusage --help' for usage."));
            2
        }
    };
    std::process::exit(code);
}

fn dispatch(command: args::Command) -> i32 {
    match command {
        args::Command::Help => 0,
        args::Command::Read { token, force } => limits::print_limits(token.as_deref(), force),
        args::Command::Accounts => session::accounts(),
        args::Command::Add {
            provider,
            organization,
        } => signin::add(&provider, organization),
        args::Command::Remove { account } => session::remove(&account),
        args::Command::Rename { account, name } => session::rename(&account, &name),
        args::Command::Refresh => session::refresh(),
        args::Command::Waybar { once, icon } => waybar::run(once, &icon),
        args::Command::WaybarRefresh => waybar::request_refresh(),
        args::Command::Alerts(change) => session::alerts(change),
        args::Command::Panel => session::panel(),
        args::Command::HubList => hub::list(),
        args::Command::HubAdd { url } => hub::add(&url),
        args::Command::HubRemove { hub } => hub::remove(&hub),
    }
}
