#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod input;
mod native_drawing;
mod renderer;
mod startup;
mod ui_timer;
mod window_state;
fn w11term_data_path() -> anyhow::Result<std::path::PathBuf> {
    w11term::keys::data_dir()
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            startup::report(&error);
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("w11term=info"))
        .init();
    let Some(options) = w11term::options::Options::parse()? else {
        return Ok(());
    };
    if options.benchmark {
        w11term::session::benchmark();
        return Ok(());
    }
    windows_reactor::App::run_component::<app::TerminalApp>(options)?;
    Ok(())
}
