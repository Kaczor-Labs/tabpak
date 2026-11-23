//! The window draws its states without the network: a first frame that installs the look, then the stage.

use egui_kittest::Harness;
use tabpak::ui::{App, Prefs, SIZE};

fn frame(harness: &mut Harness<'_, App>) -> image::RgbaImage {
    for _ in 0..3 {
        harness.step();
    }
    harness.render().expect("the frame renders")
}

/// How many pixels differ between two frames.
fn changed(a: &image::RgbaImage, b: &image::RgbaImage) -> usize {
    a.pixels().zip(b.pixels()).filter(|(x, y)| x != y).count()
}

#[test]
fn every_offline_state_draws() {
    // one window for all states: quickRS's look keeps its textures for the process
    let app = App::new(Prefs::default());
    let mut harness = Harness::builder().with_size(egui::vec2(SIZE[0], SIZE[1])).wgpu().build_ui_state(|ui, app: &mut App| app.draw(ui), app);
    let empty = frame(&mut harness);
    let base = *empty.get_pixel(2, 2);
    let painted = empty.pixels().filter(|p| **p != base).count();
    assert!(painted > 100_000, "the sidebar and the stage are drawn, got {painted} px");

    harness.state_mut().show_failure("Songsterr has no tab at this link. Check the link and try again.");
    let failed = frame(&mut harness);
    assert!(changed(&empty, &failed) > 5_000, "the failure replaces the three steps");

    harness.state_mut().show_hits(Vec::new());
    let none = frame(&mut harness);
    assert!(changed(&failed, &none) > 1_000, "no hits says so in its own words");
}
