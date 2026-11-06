#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod gui;

use assembly_scanner::{capture, eds, model, preflight, scanner, settings_file};

fn main() -> eframe::Result {
    gui::run()
}
