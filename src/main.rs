#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod eds;
mod gui;

use assembly_scanner::{capture, model, scanner};

fn main() -> eframe::Result {
    gui::run()
}
