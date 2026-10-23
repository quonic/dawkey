// SPDX-License-Identifier: AGPL-3.0-or-later
//! The plugin host process on its own, for dawkey-plugins' tests (Dawkey runs
//! the same thing as `dawkey plugin-host`).

fn main() -> std::process::ExitCode {
    match dawkey_plugins::run_plugin_host() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dawkey-plugin-host: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
