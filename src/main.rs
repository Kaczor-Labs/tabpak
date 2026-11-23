//! Tabpak: with no arguments the window opens; with arguments it runs in the terminal (`tabpak help`).

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui;
use tabpak::ui::{self, App, Prefs};

fn window() -> eframe::Result {
    let options = || eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Tabpak")
            .with_app_id("tabpak")
            .with_inner_size(ui::SIZE)
            .with_min_inner_size(ui::MIN_SIZE)
            .with_icon(icon()),
        ..Default::default()
    };
    // Metal on a Mac, OpenGL elsewhere, and a plain message when the computer can draw neither
    fastframe_graphics::Graphics::new("Tabpak").run_native(options, || {
        Box::new(|cc| {
            let app = App::new(Prefs::load());
            app.refresh_saved(&cc.egui_ctx);
            Ok(Box::new(app))
        })
    })
}

fn icon() -> egui::IconData {
    match image::load_from_memory(include_bytes!("../assets/icon.png")) {
        Ok(img) => {
            let img = img.to_rgba8();
            egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() }
        }
        Err(_) => egui::IconData::default(),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        if let Err(e) = window() {
            eprintln!("Tabpak couldn't open its window: {e}");
            std::process::exit(1);
        }
        return;
    }
    match tabpak::cli::run(&args) {
        Ok(text) => println!("{text}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
