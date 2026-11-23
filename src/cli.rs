//! The same conversion from a terminal: `tabpak info <link>` and `tabpak convert <link>`.

use std::path::PathBuf;

use crate::convert::{self, Kind, Request, Source, Step};
use crate::songsterr;

pub const USAGE: &str = "\
tabpak                       open the window
tabpak info <link or id>     list the song's parts
tabpak search <words>        find songs on Songsterr
tabpak convert <link or id>  write the feedpak
    --only bass|guitar       convert only these parts (default: the kind the link names, else all)
    --audio recording|songsterr|<file>   where the sound comes from (default: recording)
    --video <YouTube id>     take the recording and its sync from this video
    --out <folder>           where the file goes (default: ~/quickRS-songs)
    --offset <seconds>       move every note later (negative: earlier)";

fn song_of(text: &str) -> Result<convert::Song, String> {
    let id = songsterr::song_id_of(text).ok_or("That isn't a Songsterr link or song number.")?;
    convert::load_song(id)
}

fn clock(seconds: f64) -> String {
    format!("{}:{:02}", seconds as u64 / 60, seconds as u64 % 60)
}

/// Run a command; the text to print, or the error.
pub fn run(args: &[String]) -> Result<String, String> {
    let mut out = Vec::new();
    match args.first().map(String::as_str) {
        Some("search") => {
            for hit in songsterr::search(&args[1..].join(" "))? {
                let kinds = [hit.has_guitar.then_some("guitar"), hit.has_bass.then_some("bass")].into_iter().flatten().collect::<Vec<_>>().join(", ");
                out.push(format!("{:>8}  {} - {}  ({kinds})", hit.song_id, hit.artist, hit.title));
            }
        }
        Some("info") => {
            let song = song_of(args.get(1).ok_or(USAGE)?)?;
            out.push(format!("{} - {}  ({} measures, {} at the tab's tempo)", song.artist, song.title, song.measures, clock(song.seconds)));
            for p in &song.parts {
                out.push(format!("  {:>2}  {:<6} {:<40} {:<14} {} notes", p.index, p.kind.label(), p.name, p.tuning_name, p.notes));
            }
            for v in &song.videos {
                out.push(format!("  video {}  {}  {} sync points", v.id, v.feature.as_deref().unwrap_or("main"), v.points.len()));
            }
            out.push(format!("  Songsterr sound: {}", if song.has_songsterr_sound() { "yes" } else { "no" }));
        }
        Some("convert") => {
            let link = args.get(1).ok_or(USAGE)?;
            let song = song_of(link)?;
            let mut kind = match songsterr::instrument_in_link(link) {
                Some("bass") => Some(Kind::Bass),
                _ => None,
            };
            let mut req = Request { parts: Vec::new(), source: Source::Recording, out_dir: convert::default_songs_dir(), offset: 0.0 };
            let mut all = false;
            let mut rest = args[2..].iter();
            while let Some(flag) = rest.next() {
                let mut value = || rest.next().ok_or_else(|| format!("{flag} needs a value\n\n{USAGE}"));
                match flag.as_str() {
                    "--only" => match value()?.as_str() {
                        "bass" => kind = Some(Kind::Bass),
                        "guitar" => kind = Some(Kind::Guitar),
                        "all" => all = true,
                        other => return Err(format!("--only takes bass, guitar or all, not {other}")),
                    },
                    "--audio" => {
                        req.source = match value()?.as_str() {
                            "recording" => Source::Recording,
                            "songsterr" => Source::Songsterr,
                            file => Source::File(PathBuf::from(file)),
                        }
                    }
                    "--video" => req.source = Source::Video(value()?.clone()),
                    "--out" => req.out_dir = PathBuf::from(value()?),
                    "--offset" => req.offset = value()?.parse().map_err(|_| "--offset takes seconds, like -0.03")?,
                    other => return Err(format!("unknown option {other}\n\n{USAGE}")),
                }
            }
            req.parts = song.parts_of(if all { None } else { kind });
            eprintln!("{} - {}", song.artist, song.title);
            let mut last = (Step::Audio, -1i32);
            let done = convert::convert(&song, &req, &mut |step, fraction| {
                let now = (step, (fraction * 10.0) as i32);
                if now != last {
                    last = now;
                    let name = match step {
                        Step::Audio => "audio",
                        Step::Sync => "sync",
                        Step::Pack => "pack",
                    };
                    if fraction >= 0.0 { eprintln!("  {name} {:.0}%", fraction * 100.0) } else { eprintln!("  {name}") }
                }
            })?;
            for (name, count) in &done.parts {
                eprintln!("  {name}: {count} notes and chords");
            }
            eprintln!("  audio: {}, {}; {} sync points for {} measures", done.audio, clock(done.seconds), done.points, done.measures);
            if let Some((hits, places)) = done.in_step {
                eprintln!("  sync check: notes line up with the sound at {hits} of {places} places");
            }
            for note in &done.notes {
                eprintln!("  {note}");
            }
            out.push(done.path.display().to_string());
        }
        _ => return Err(USAGE.into()),
    }
    Ok(out.join("\n"))
}
