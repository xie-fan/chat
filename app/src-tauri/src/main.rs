#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(desktop)]
    if chat_app_lib::maybe_run_cli() {
        return;
    }
    chat_app_lib::run();
}
