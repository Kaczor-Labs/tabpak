//! Draws the window's states off screen and saves each as a PNG: `cargo run --example shots -- <out folder>
//! [songs folder]`. Reads real songs from Songsterr, so it needs the network. Each state is drawn by a run of
//! its own (`... <out folder> <songs folder> <state>`), because the look keeps its textures per process.

use std::path::{Path, PathBuf};

use eframe::egui;
use tabpak::convert::{self, Done, Kind, Step};
use tabpak::ui::{App, Prefs, SIZE};
use tabpak::{audio, feedpak, songsterr};

fn shoot(out: &Path, name: &str, app: &mut App) {
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(SIZE[0], SIZE[1]))
        .with_pixels_per_point(2.0)
        .wgpu()
        .build_ui(|ui| app.draw(ui));
    // enough frames, far enough apart, for the keys that glide to come to rest
    for _ in 0..14 {
        harness.step();
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    let image = harness.render().expect("the frame renders");
    image.save(out.join(format!("{name}.png"))).expect("the picture saves");
    println!("{name}.png");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().expect("an output folder"));
    let songs = args.next().unwrap_or_default();
    std::fs::create_dir_all(&out).expect("the output folder");
    let Some(state) = args.next() else {
        for state in ["1-empty", "2-search", "3-song", "4-converting", "5-done", "6-many-parts", "7-my-file", "8-failed"] {
            let me = std::env::current_exe().expect("this program");
            let ok = std::process::Command::new(me).arg(&out).arg(&songs).arg(state).status().is_ok_and(|s| s.success());
            assert!(ok, "{state} didn't draw");
        }
        return;
    };
    let saved = if songs.is_empty() { Vec::new() } else { feedpak::list_saved(Path::new(&songs)) };
    let mut app = App::new(Prefs::default());
    app.show_saved(saved);
    let load = |id: u64| {
        let song = convert::load_song(id).expect("the song loads");
        let cover = song.videos.iter().take(3).find_map(|v| audio::cover(&v.id));
        (song, cover)
    };
    let link = "https://www.songsterr.com/a/wsa/popek-bollywodzka-produkcja-pakistanskie-disco-bass-tab-s403656";
    let popek = |app: &mut App| {
        let (song, cover) = load(403656);
        app.set_query(link);
        app.show_song(song, cover, Some(Kind::Bass));
    };
    match state.as_str() {
        "1-empty" => app.show_saved(Vec::new()),
        "2-search" => {
            app.set_query("master of puppets");
            app.show_hits(songsterr::search("master of puppets").expect("the search answers"));
        }
        "3-song" => popek(&mut app),
        "4-converting" => {
            popek(&mut app);
            app.set_sound(1);
            app.show_job_running(Step::Audio, 0.62);
        }
        "5-done" => {
            popek(&mut app);
            app.set_sound(1);
            app.show_job_done(Done {
                path: PathBuf::from("Popek - Bollywodzka Produkcja Pakistanskie Disco (Songsterr sound).feedpak"),
                parts: vec![("Electric Bass".into(), 959)],
                seconds: 240.1,
                audio: "Songsterr sound".into(),
                points: 0,
                measures: 129,
                notes: Vec::new(),
            });
        }
        "6-many-parts" => {
            let (song, cover) = load(455118);
            app.set_query("https://www.songsterr.com/a/wsa/metallica-master-of-puppets-tab-s455118");
            app.show_song(song, cover, None);
        }
        "7-my-file" => {
            popek(&mut app);
            app.set_sound(2);
        }
        "8-failed" => {
            app.set_query("https://www.songsterr.com/a/wsa/nothing-here-tab-s999999999");
            app.show_failure("Songsterr has no tab at this link. Check the link and try again.");
        }
        other => panic!("no state called {other}"),
    }
    shoot(&out, &state, &mut app);
}
