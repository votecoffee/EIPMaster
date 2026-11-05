#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod capture;
mod eds;
mod gui;
mod model;
mod scanner;

fn main() -> eframe::Result {
    gui::run()
}
