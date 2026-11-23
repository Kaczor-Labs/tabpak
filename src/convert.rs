//! One song from Songsterr to a feedpak: load its parts, fetch the audio, place the notes on it, write the file.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use serde_json::{Value, json};

use crate::audio::{self, Pcm};
use crate::chart::{self, Slot};
use crate::{feedpak, songsterr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bass,
    Guitar,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Bass => "bass",
            Kind::Guitar => "guitar",
        }
    }
}

/// A guitar or bass part of the tab.
#[derive(Clone, Debug)]
pub struct Part {
    /// The part's number on Songsterr.
    pub index: usize,
    pub name: String,
    pub kind: Kind,
    pub tuning_name: String,
    pub strings: usize,
    pub capo: i64,
    /// Notes and chords.
    pub notes: usize,
    /// How busy the part is in each played measure, 0 to 1.
    pub density: Vec<f32>,
    track: Value,
}

/// A YouTube video Songsterr has synced to the tab: one time per played measure.
#[derive(Clone, Debug)]
pub struct Video {
    pub id: String,
    /// None for the song's main video; "alternative", "backing", "playthrough" and the like otherwise.
    pub feature: Option<String>,
    pub points: Vec<f64>,
}

#[derive(Clone, Debug)]
pub struct Song {
    pub id: u64,
    pub revision: i64,
    pub title: String,
    pub artist: String,
    pub transcriber: String,
    pub parts: Vec<Part>,
    pub videos: Vec<Video>,
    /// Played measures, repeats written out.
    pub measures: usize,
    /// The song's length at the tab's own tempo.
    pub seconds: f64,
    /// Every sounding track's number: what Songsterr's own sound has a stem for.
    sounding: Vec<usize>,
    synth: Option<String>,
    order: Vec<usize>,
    slots: Vec<Slot>,
}

impl Song {
    /// Songsterr can play this tab with its own sound.
    /// How long the song runs, and whether that is the recording's time (else it is the tab's own tempo).
    pub fn length(&self) -> (f64, bool) {
        let nominal: Vec<f64> = self.slots.iter().map(|s| s.seconds).collect();
        match candidates(self, None).first().filter(|v| v.points.len() >= 2) {
            Some(v) => (*chart::slot_times(&nominal, &v.points).0.last().unwrap_or(&self.seconds), true),
            None => (self.seconds, false),
        }
    }

    pub fn has_songsterr_sound(&self) -> bool {
        self.synth.is_some()
    }

    /// The parts picked when a song opens: the kind asked for, else everything.
    pub fn parts_of(&self, kind: Option<Kind>) -> Vec<usize> {
        let of_kind: Vec<usize> = self.parts.iter().filter(|p| Some(p.kind) == kind).map(|p| p.index).collect();
        if of_kind.is_empty() { self.parts.iter().map(|p| p.index).collect() } else { of_kind }
    }
}

fn is_fretted(track: &Value) -> bool {
    let program = track["instrumentId"].as_i64().unwrap_or(-1);
    let strings = track["tuning"].as_array().map(Vec::len).unwrap_or(0);
    !track["isVocalTrack"].as_bool().unwrap_or(false)
        && !track["isEmpty"].as_bool().unwrap_or(false)
        && (songsterr::is_bass_program(program) || songsterr::is_guitar_program(program))
        && (4..=8).contains(&strings)
}

/// Display names: two parts with one name are told apart by their instrument, else by a number.
fn part_names(meta: &Value, picked: &[usize]) -> Vec<String> {
    let field = |i: usize, key: &str| meta["tracks"][i][key].as_str().unwrap_or("").to_string();
    picked
        .iter()
        .map(|&i| {
            // a part the tabber left unnamed goes by its instrument
            let named = |j: usize| Some(field(j, "name").trim().to_string()).filter(|n| !n.is_empty()).unwrap_or_else(|| field(j, "instrument"));
            let name = named(i);
            let same: Vec<usize> = picked.iter().copied().filter(|&j| named(j) == name).collect();
            if same.len() < 2 {
                return name;
            }
            let mut instruments: Vec<String> = same.iter().map(|&j| field(j, "instrument")).collect();
            instruments.sort();
            instruments.dedup();
            if instruments.len() > 1 {
                format!("{name} ({})", field(i, "instrument"))
            } else {
                format!("{name} {}", same.iter().position(|&j| j == i).unwrap_or(0) + 1)
            }
        })
        .collect()
}

fn offline(e: String) -> String {
    if e == "not found" {
        "Songsterr has no tab at this link. Check the link and try again.".into()
    } else {
        format!("Songsterr didn't answer ({e}). Check the connection and try again.")
    }
}

/// Fetch a song's guitar and bass parts and the videos synced to it.
pub fn load_song(song_id: u64) -> Result<Song, String> {
    let meta = songsterr::meta(song_id).map_err(offline)?;
    let empty = Vec::new();
    let listed = meta["tracks"].as_array().unwrap_or(&empty);
    let fretted: Vec<usize> = (0..listed.len()).filter(|&i| is_fretted(&listed[i])).collect();
    if fretted.is_empty() {
        return Err("This tab has no guitar or bass part to convert.".into());
    }
    let (tracks, videos) = std::thread::scope(|scope| {
        let videos = scope.spawn(|| songsterr::videos(&meta));
        let handles: Vec<_> = fretted.iter().map(|&i| scope.spawn({ let meta = &meta; move || songsterr::track(meta, i) })).collect();
        let tracks: Vec<Result<Value, String>> = handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("the download stopped".into()))).collect();
        (tracks, videos.join().unwrap_or_else(|_| Ok(Vec::new())))
    });
    let tracks: Vec<Value> = tracks.into_iter().collect::<Result<_, _>>().map_err(offline)?;

    let order = chart::unroll(&tracks[0]);
    let slots = chart::walk(&tracks[0], &order);
    let nominal: Vec<f64> = slots.iter().map(|s| s.seconds).collect();
    let (starts, _) = chart::slot_times(&nominal, &[]);

    let names = part_names(&meta, &fretted);
    let mut parts = Vec::new();
    for ((&index, track), name) in fretted.iter().zip(tracks).zip(names) {
        let bass = songsterr::is_bass_program(track["instrumentId"].as_i64().unwrap_or(-1));
        let own_order = chart::unroll(&track);
        if own_order.len() != order.len() {
            continue; // a part whose measures don't line up with the song's can't be placed in time
        }
        let (_, notes) = chart::build_chart(&track, &starts, &name);
        if notes == 0 {
            continue;
        }
        parts.push(Part {
            index,
            name,
            kind: if bass { Kind::Bass } else { Kind::Guitar },
            tuning_name: chart::tuning_name(&chart::tuning_offsets(&track), bass),
            strings: track["strings"].as_u64().unwrap_or(6) as usize,
            capo: track["capo"].as_i64().unwrap_or(0),
            notes,
            density: chart::density(&track, &own_order),
            track,
        });
    }
    if parts.is_empty() {
        return Err("The guitar and bass parts of this tab are empty.".into());
    }

    let videos = videos
        .unwrap_or_default()
        .iter()
        .filter(|v| v["status"].as_str().unwrap_or("done") == "done")
        .filter_map(|v| {
            Some(Video {
                id: v["videoId"].as_str().filter(|s| !s.is_empty())?.to_string(),
                feature: v["feature"].as_str().map(str::to_string),
                points: v["points"].as_array().map(|p| p.iter().filter_map(Value::as_f64).collect()).unwrap_or_default(),
            })
        })
        .collect();
    let synth = meta["audios"]["current"]["hash"].as_str().or(meta["audioV4"].as_str()).filter(|s| !s.is_empty());
    Ok(Song {
        id: song_id,
        revision: meta["revisionId"].as_i64().unwrap_or(0),
        title: meta["title"].as_str().unwrap_or("Untitled").to_string(),
        artist: meta["artist"].as_str().unwrap_or("Unknown artist").to_string(),
        transcriber: meta["author"]["name"].as_str().unwrap_or("Songsterr").to_string(),
        parts,
        videos,
        measures: order.len(),
        seconds: *starts.last().unwrap_or(&0.0),
        sounding: (0..listed.len()).filter(|&i| !listed[i]["isEmpty"].as_bool().unwrap_or(false)).collect(),
        synth: synth.map(str::to_string),
        order,
        slots,
    })
}

/// Where the song's audio comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// The first synced YouTube video that downloads: the real recording.
    Recording,
    /// This YouTube video.
    Video(String),
    /// Songsterr's own sound, one stem per instrument.
    Songsterr,
    /// The player's own audio file.
    File(PathBuf),
}

#[derive(Clone, Debug)]
pub struct Request {
    /// Part numbers to convert.
    pub parts: Vec<usize>,
    pub source: Source,
    pub out_dir: PathBuf,
    /// Seconds to move every note later (negative: earlier): for a file of the player's own that starts
    /// earlier or later than the video Songsterr synced.
    pub offset: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Audio,
    Sync,
    Pack,
}

#[derive(Clone, Debug)]
pub struct Done {
    pub path: PathBuf,
    /// Each converted part and its count of notes and chords.
    pub parts: Vec<(String, usize)>,
    pub seconds: f64,
    /// "YouTube vnNCygVlHcI", "Songsterr sound", "your file".
    pub audio: String,
    pub points: usize,
    pub measures: usize,
    /// Things that went another way than asked: a video that was skipped, measures without sync.
    pub notes: Vec<String>,
    /// Songsterr sound only: of the places where a part's sound starts after silence, how many have a note
    /// there, and how many places there are.
    pub in_step: Option<(usize, usize)>,
    /// The check found a part whose notes and sound are apart; `notes` starts with which.
    pub out_of_step: bool,
}

/// How many of the places where a part's sound starts have a note of its chart there. Songsterr's sound trails
/// its note by a few milliseconds, and leads it by up to 50 when the note slides in.
fn notes_at(places: &[f64], times: &[f64]) -> usize {
    let has_note = |t: f64| times.get(times.partition_point(|&n| n < t - 0.03)).is_some_and(|&n| n <= t + 0.06);
    places.iter().filter(|&&t| has_note(t)).count()
}

/// Whether a part's notes and its sound are apart: more than a quarter of the checked places have no note (one
/// stray click in the sound is not enough), or the last note comes after the sound has ended.
fn apart(places: usize, hits: usize, last_note: Option<f64>, last_sound: f64) -> bool {
    let misses = places - hits;
    (misses >= 2 && misses * 4 > places) || last_note.is_some_and(|t| t > last_sound + 0.1)
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Scratch, String> {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("tabpak-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't make a working folder ({e})."))?;
        Ok(Scratch(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Videos to try, best first: the song's main video, then the others by how completely they are synced.
fn candidates(song: &Song, wanted: Option<&str>) -> Vec<Video> {
    if let Some(id) = wanted {
        let known = song.videos.iter().find(|v| v.id == id).cloned();
        return vec![known.unwrap_or(Video { id: id.to_string(), feature: None, points: Vec::new() })];
    }
    let mut list = song.videos.clone();
    list.sort_by_key(|v| (v.feature.is_some(), (song.measures as i64 - v.points.len() as i64).abs()));
    list.dedup_by(|a, b| a.id == b.id);
    list
}

fn stem_url(song: &Song, hash: &str, track: usize) -> String {
    format!("https://audio4-1.songsterr.com/{}/{}/{hash}/100/s/{track}.opus", song.id, song.revision)
}

/// Songsterr's sound for every sounding track, decoded; `report` gets 0 to 1.
fn songsterr_stems(song: &Song, report: &mut dyn FnMut(f32)) -> Result<Vec<(usize, Pcm)>, String> {
    let hash = song.synth.as_deref().ok_or("Songsterr has no sound of its own for this tab. Use the recording instead.")?;
    let (tx, rx) = mpsc::channel();
    let total = song.sounding.len();
    std::thread::scope(|scope| {
        for lane in 0..4 {
            let tx = tx.clone();
            scope.spawn(move || {
                for &track in song.sounding.iter().skip(lane).step_by(4) {
                    let pcm = songsterr::http_get(&stem_url(song, hash, track)).and_then(|data| audio::decode_bytes(data, "opus"));
                    let _ = tx.send((track, pcm));
                }
            });
        }
        drop(tx);
        let mut stems = Vec::new();
        for (done, (track, pcm)) in rx.iter().enumerate() {
            report((done + 1) as f32 / total.max(1) as f32);
            match pcm {
                Ok(pcm) => stems.push((track, pcm)),
                // a silent track has no stem; a part the player picked must have one
                Err(e) if song.parts.iter().any(|p| p.index == track) => return Err(format!("Songsterr's sound didn't download ({e}). Try again, or use the recording.")),
                Err(_) => {}
            }
        }
        stems.sort_by_key(|(track, _)| *track);
        Ok(stems)
    })
}

fn json_bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

/// Convert the picked parts and write the feedpak. `report` gets each step and how far it is (0 to 1, or
/// negative when there is no telling).
pub fn convert(song: &Song, req: &Request, report: &mut dyn FnMut(Step, f32)) -> Result<Done, String> {
    let picked: Vec<&Part> = song.parts.iter().filter(|p| req.parts.contains(&p.index)).collect();
    if picked.is_empty() {
        return Err("Pick at least one part.".into());
    }
    let nominal: Vec<f64> = song.slots.iter().map(|s| s.seconds).collect();
    let mut notes = Vec::new();
    let scratch = Scratch::new()?;

    // 1. the audio
    report(Step::Audio, 0.0);
    let (mut video, mut points): (Option<Video>, Vec<f64>) = (None, Vec::new());
    let mut stems: Vec<(usize, Pcm)> = Vec::new();
    let mut recording: Option<Pcm> = None;
    let (label, kind) = match &req.source {
        Source::Songsterr => {
            stems = songsterr_stems(song, &mut |f| report(Step::Audio, f))?;
            ("Songsterr sound".to_string(), "songsterr")
        }
        Source::File(path) => {
            recording = Some(audio::decode(path).map_err(|e| format!("That audio file didn't open: {e}. Pick an MP3, M4A, FLAC, WAV or OGG file."))?);
            video = candidates(song, None).into_iter().next();
            ("your file".to_string(), "file")
        }
        Source::Recording | Source::Video(_) => {
            let wanted = if let Source::Video(id) = &req.source { Some(id.as_str()) } else { None };
            for v in candidates(song, wanted) {
                match audio::download(&v.id, &scratch.0, &mut |f| report(Step::Audio, f * 0.9)) {
                    Ok(path) => match audio::decode(&path) {
                        Ok(pcm) => {
                            recording = Some(pcm);
                            video = Some(v);
                            break;
                        }
                        Err(e) => notes.push(format!("Video {} skipped: {e}", v.id)),
                    },
                    Err(e) if e.contains("isn't installed") => return Err(e),
                    Err(e) => notes.push(format!("Video {} skipped: {e}", v.id)),
                }
            }
            let Some(v) = &video else {
                let why = if song.videos.is_empty() { "Songsterr has no video synced to this tab" } else { "None of this song's videos downloaded" };
                return Err(format!("{why}. Use Songsterr's sound or your own audio file instead."));
            };
            (format!("YouTube {}", v.id), "youtube")
        }
    };
    if let Some(v) = &video {
        points = v.points.clone();
    }

    // 2. the notes on the audio
    report(Step::Sync, -1.0);
    let (mut starts, unsynced) = chart::slot_times(&nominal, &points);
    if unsynced > 0 {
        notes.push(format!("{unsynced} of {} measures have no sync point; they follow the tab's tempo.", song.measures));
    }
    if points.is_empty() && req.source != Source::Songsterr {
        notes.push("Songsterr has no sync for this audio: the notes follow the tab's tempo from the start of the file.".into());
    } else if matches!(req.source, Source::File(_)) && req.offset == 0.0 {
        notes.push("The notes follow Songsterr's sync for the song's video. If your file starts earlier or later, convert again with an offset.".into());
    }
    starts.iter_mut().for_each(|t| *t += req.offset);

    // 3. the pack
    report(Step::Pack, 0.0);
    let guitar_ids = ["lead".to_string(), "rhythm".to_string()].into_iter().chain((3..).map(|k| format!("guitar{k}")));
    let bass_ids = ["bass".to_string()].into_iter().chain((2..).map(|k| format!("bass{k}")));
    let (mut guitar_ids, mut bass_ids) = (guitar_ids, bass_ids);
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let (mut entries, mut done_parts, mut part_stems) = (Vec::new(), Vec::new(), Vec::new());
    for part in &picked {
        let id = if part.kind == Kind::Bass { bass_ids.next() } else { guitar_ids.next() }.unwrap_or_default();
        let (chart, count) = chart::build_chart(&part.track, &starts, &part.name);
        let path = format!("arrangements/{id}.json");
        entries.push(json!({"id": id, "name": part.name, "type": part.kind.label(), "file": path, "tuning": chart["tuning"], "capo": chart["capo"]}));
        files.push((path, json_bytes(&chart)));
        done_parts.push((part.name.clone(), count));
        let mut times: Vec<f64> = ["notes", "chords"].iter().flat_map(|k| chart[*k].as_array().into_iter().flatten()).filter_map(|n| n["t"].as_f64()).collect();
        times.sort_by(f64::total_cmp);
        part_stems.push((part.index, id, part.name.clone(), part.kind, times));
    }
    let tracks: Vec<&Value> = song.parts.iter().map(|p| &p.track).collect();
    let sections = chart::section_marks(&tracks, &song.order, &starts);
    files.push(("song_timeline.json".into(), json_bytes(&chart::timeline(&song.slots, &starts, sections))));

    // stems: the recording alone, or each picked part, the rest of the band, and the whole mix
    let mut to_encode: Vec<(Value, Pcm)> = Vec::new();
    let (mut in_step, mut out_of_step): (Option<(usize, usize)>, bool) = (None, false);
    if let Some(pcm) = recording {
        to_encode.push((json!({"id": "full", "file": "stems/full.ogg", "default": true}), pcm));
    } else {
        let all: Vec<&Pcm> = stems.iter().map(|(_, pcm)| pcm).collect();
        let mut full = audio::mix(&all).ok_or("Songsterr's sound came back empty. Try again, or use the recording.")?;
        let gain = if full.peak() > 0.98 { 0.98 / full.peak() } else { 1.0 };
        full.scale(gain);
        let backing: Vec<&Pcm> = stems.iter().filter(|(track, _)| !part_stems.iter().any(|(index, ..)| index == track)).map(|(_, pcm)| pcm).collect();
        let backing = audio::mix(&backing).map(|mut pcm| {
            pcm.scale(gain);
            pcm
        });
        for (index, id, name, kind, times) in &part_stems {
            let Some(at) = stems.iter().position(|(track, _)| track == index) else { continue };
            let mut pcm = stems.swap_remove(at).1;
            // the check: this part's notes against its own sound
            let (places, last_sound) = audio::sound_starts(&pcm);
            let places: Vec<f64> = places.iter().map(|t| t + req.offset).collect();
            let hits = notes_at(&places, times);
            if apart(places.len(), hits, times.last().copied(), last_sound + req.offset) {
                let found = if places.len() > hits { format!("notes line up at {hits} of {} checked places", places.len()) } else { "its last note comes after the sound ends".to_string() };
                notes.insert(0, format!("{name} is out of step with Songsterr's sound: {found}. Use the recording for this song."));
                out_of_step = true;
            }
            in_step = Some((in_step.map_or(0, |(h, _)| h) + hits, in_step.map_or(0, |(_, p)| p) + places.len()));
            pcm.scale(gain);
            // quickRS finds a part's own stem by "bass" or "guitar" in its id and name
            let word = kind.label();
            let shown = if name.to_lowercase().contains(word) { name.clone() } else { format!("{name} ({word})") };
            to_encode.push((json!({"id": id, "file": format!("stems/{id}.ogg"), "default": true, "name": shown}), pcm));
        }
        if let Some(pcm) = backing {
            to_encode.push((json!({"id": "backing", "file": "stems/backing.ogg", "default": true, "name": "Rest of the band"}), pcm));
        }
        to_encode.push((json!({"id": "full", "file": "stems/full.ogg", "default": false, "name": "Full mix"}), full));
    }
    let seconds = to_encode.iter().map(|(_, pcm)| pcm.seconds()).fold(0.0, f64::max);
    let total = to_encode.len();
    let encoded: Vec<Result<Vec<u8>, String>> = std::thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        let handles: Vec<_> = to_encode
            .iter()
            .map(|(_, pcm)| {
                let tx = tx.clone();
                scope.spawn(move || {
                    let out = audio::encode_ogg(pcm);
                    let _ = tx.send(());
                    out
                })
            })
            .collect();
        drop(tx);
        for (done, ()) in rx.iter().enumerate() {
            report(Step::Pack, (done + 1) as f32 / total as f32);
        }
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("the encoder stopped".into()))).collect()
    });
    let mut stem_entries = Vec::new();
    for ((entry, _), data) in to_encode.iter().zip(encoded) {
        let data = data.map_err(|e| format!("The audio didn't encode ({e})."))?;
        files.push((entry["file"].as_str().unwrap_or_default().to_string(), data));
        stem_entries.push(entry.clone());
    }

    let mut manifest = json!({
        "feedpak_version": feedpak::VERSION,
        "title": song.title,
        "artist": song.artist,
    });
    let fields = manifest.as_object_mut().expect("an object");
    if req.source == Source::Songsterr {
        fields.insert("album".into(), json!("Songsterr sound"));
    }
    fields.insert("duration".into(), json!((seconds * 1000.0).round() / 1000.0));
    fields.insert("authors".into(), json!([{"name": song.transcriber, "role": "transcriber"}]));
    fields.insert("arrangements".into(), Value::Array(entries));
    fields.insert("stems".into(), Value::Array(stem_entries));
    fields.insert("song_timeline".into(), json!("song_timeline.json"));
    let mut cover_ids: Vec<String> = video.iter().map(|v| v.id.clone()).collect();
    cover_ids.extend(candidates(song, None).into_iter().map(|v| v.id));
    if let Some(cover) = cover_ids.iter().take(3).find_map(|id| audio::cover(id)) {
        files.push(("cover.jpg".into(), cover));
        fields.insert("cover".into(), json!("cover.jpg"));
    }
    let video_id = video.as_ref().map(|v| v.id.as_str()).unwrap_or("");
    fields.insert("songsterr".into(), json!({"song_id": song.id, "revision_id": song.revision, "video_id": video_id, "audio": kind}));
    files.insert(0, ("manifest.yaml".into(), feedpak::yaml(&manifest).into_bytes()));

    std::fs::create_dir_all(&req.out_dir).map_err(|e| format!("Couldn't make the songs folder ({e}). Pick another folder."))?;
    let suffix = if req.source == Source::Songsterr { " (Songsterr sound)" } else { "" };
    let name = format!("{} - {}{suffix}.feedpak", feedpak::safe_name(&song.artist), feedpak::safe_name(&song.title));
    let path = req.out_dir.join(name);
    feedpak::write_zip(&path, &files).map_err(|e| format!("Couldn't write the song file ({e}). Check the songs folder and try again."))?;
    report(Step::Pack, 1.0);
    Ok(Done { path, parts: done_parts, seconds, audio: label, points: points.len(), measures: song.measures, notes, in_step, out_of_step })
}

/// The folder songs go to unless the player picks another.
pub fn default_songs_dir() -> PathBuf {
    home().join("quickRS-songs")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// quickRS's settings file on this computer, when quickRS has run here.
pub fn quickrs_config() -> Option<PathBuf> {
    let dirs = fastframe_dirs::Dirs::platform("", "", "quickRS")?;
    let path = dirs.config.join("config.json");
    path.is_file().then_some(path)
}

/// Whether quickRS looks for songs in `dir`. None when quickRS isn't set up here.
pub fn quickrs_knows(dir: &Path) -> Option<bool> {
    let config: Value = serde_json::from_slice(&std::fs::read(quickrs_config()?).ok()?).ok()?;
    let listed = config["library_dirs"].as_array()?;
    Some(listed.iter().filter_map(Value::as_str).any(|d| Path::new(d) == dir))
}

/// Add `dir` to the folders quickRS looks for songs in.
pub fn quickrs_add(dir: &Path) -> Result<(), String> {
    let path = quickrs_config().ok_or("quickRS isn't set up on this computer yet. Open it once, then try again.")?;
    let mut config: Value = serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let mut listed = config["library_dirs"].as_array().cloned().unwrap_or_default();
    listed.push(json!(dir.to_string_lossy()));
    config["library_dirs"] = Value::Array(listed);
    let text = serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?;
    fastframe_file::write(&path, text).map_err(|e| format!("Couldn't change quickRS's settings ({e})."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_named_parts_are_told_apart() {
        let meta = json!({"tracks": [
            {"name": "Electric Guitar", "instrument": "Distortion Guitar"},
            {"name": "Electric Guitar", "instrument": "Overdriven Guitar"},
            {"name": "Bass", "instrument": "Electric Bass (finger)"},
            {"name": "Bass", "instrument": "Electric Bass (finger)"},
            {"name": "Lead", "instrument": "Distortion Guitar"},
        ]});
        assert_eq!(
            part_names(&meta, &[0, 1, 2, 3, 4]),
            ["Electric Guitar (Distortion Guitar)", "Electric Guitar (Overdriven Guitar)", "Bass 1", "Bass 2", "Lead"]
        );
    }

    #[test]
    fn the_sync_check_tells_in_step_from_apart() {
        let times = [1.0, 2.0, 3.0, 4.0];
        // the sound trails its note a little, or leads it when the note slides in; 3.2 has no note
        assert_eq!(notes_at(&[1.008, 1.95, 3.2], &times), 2);
        assert!(!apart(4, 3, Some(4.0), 5.0)); // one stray click in the sound
        assert!(apart(4, 2, Some(4.0), 5.0));
        assert!(apart(0, 0, Some(6.0), 5.0)); // the tab runs on after the sound has ended
    }

    #[test]
    fn only_sounding_guitars_and_basses_are_parts() {
        let guitar = json!({"instrumentId": 30, "tuning": [64, 59, 55, 50, 45, 40]});
        assert!(is_fretted(&guitar));
        assert!(!is_fretted(&json!({"instrumentId": 1024, "tuning": []})));
        assert!(!is_fretted(&json!({"instrumentId": 30, "tuning": [64, 59, 55, 50, 45, 40], "isEmpty": true})));
        assert!(!is_fretted(&json!({"instrumentId": 16, "tuning": [64, 59, 55, 50]})));
    }
}
