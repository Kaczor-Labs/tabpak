//! The feedpak file: a zip with a YAML manifest that indexes the charts, the audio and the cover. Also reads
//! back the few manifest lines the song list shows.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

pub const VERSION: &str = "1.19.0";

/// The manifest as YAML: mappings, lists of mappings, scalars and short number lists are all it holds.
pub fn yaml(value: &Value) -> String {
    let mut out = String::new();
    write_yaml(value, 0, &mut out);
    out
}

fn scalar(v: &Value) -> String {
    match v {
        Value::Array(items) => format!("[{}]", items.iter().map(scalar).collect::<Vec<_>>().join(", ")),
        Value::String(s) => Value::String(s.clone()).to_string(),
        other => other.to_string(),
    }
}

fn nested(v: &Value) -> bool {
    v.is_object() || v.as_array().is_some_and(|a| a.first().is_some_and(Value::is_object))
}

fn write_yaml(value: &Value, indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                if nested(v) {
                    out.push_str(&format!("{pad}{key}:\n"));
                    write_yaml(v, indent + 1, out);
                } else {
                    out.push_str(&format!("{pad}{key}: {}\n", scalar(v)));
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                let mut body = String::new();
                write_yaml(item, indent + 1, &mut body);
                let mut lines = body.lines();
                if let Some(first) = lines.next() {
                    out.push_str(&format!("{pad}- {}\n", first.trim_start()));
                }
                for line in lines {
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }
        other => out.push_str(&format!("{pad}{}\n", scalar(other))),
    }
}

/// A file name without the characters Finder and Windows refuse.
pub fn safe_name(text: &str) -> String {
    let kept: String = text.chars().filter(|c| !"\\/:*?\"<>|".contains(*c)).collect();
    let joined = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.is_empty() { "song".into() } else { joined }
}

/// Write the pack. Audio and pictures are stored as they are; text is deflated.
pub fn write_zip(dest: &Path, files: &[(String, Vec<u8>)]) -> Result<(), String> {
    let part = dest.with_extension("feedpak.part");
    let result = (|| -> Result<(), String> {
        let mut zip = zip::ZipWriter::new(File::create(&part).map_err(|e| e.to_string())?);
        for (name, data) in files {
            let packed = name.ends_with(".ogg") || name.ends_with(".jpg");
            let method = if packed { zip::CompressionMethod::Stored } else { zip::CompressionMethod::Deflated };
            let options = zip::write::SimpleFileOptions::default().compression_method(method);
            zip.start_file(name.as_str(), options).map_err(|e| e.to_string())?;
            zip.write_all(data).map_err(|e| e.to_string())?;
        }
        zip.finish().map_err(|e| e.to_string())?;
        Ok(())
    })();
    match result {
        Ok(()) => std::fs::rename(&part, dest).map_err(|e| e.to_string()),
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// A song already in the folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Saved {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub parts: Vec<String>,
    pub seconds: f64,
    pub song_id: Option<u64>,
    /// Made with Songsterr's own sound instead of a recording.
    pub songsterr_sound: bool,
    pub cover: Option<Vec<u8>>,
    pub modified: std::time::SystemTime,
}

fn quoted(rest: &str) -> String {
    let rest = rest.trim();
    serde_json::from_str::<String>(rest).unwrap_or_else(|_| rest.trim_matches(['"', '\'']).to_string())
}

/// The lines of a manifest the list needs. Reads the plain `key: value` form this app and quickRS's converter
/// write; anything else falls back to the file name.
pub fn read_saved(path: &Path) -> Option<Saved> {
    let mut zip = zip::ZipArchive::new(File::open(path).ok()?).ok()?;
    let mut text = String::new();
    zip.by_name("manifest.yaml").ok()?.read_to_string(&mut text).ok()?;
    let mut saved = Saved {
        path: path.to_path_buf(),
        title: path.file_stem()?.to_string_lossy().into_owned(),
        artist: String::new(),
        parts: Vec::new(),
        seconds: 0.0,
        song_id: None,
        songsterr_sound: false,
        cover: None,
        modified: std::fs::metadata(path).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH),
    };
    let (mut block, mut cover_path) = (String::new(), None);
    for line in text.lines() {
        let top = !line.starts_with([' ', '-']);
        let Some((key, rest)) = line.trim_start_matches([' ', '-']).split_once(':') else { continue };
        if top {
            block = key.to_string();
            match key {
                "title" => saved.title = quoted(rest),
                "artist" => saved.artist = quoted(rest),
                "duration" => saved.seconds = rest.trim().parse().unwrap_or(0.0),
                "cover" => cover_path = Some(quoted(rest)),
                _ => {}
            }
        } else if block == "arrangements" && key == "name" {
            saved.parts.push(quoted(rest));
        } else if block == "songsterr" && key == "song_id" {
            saved.song_id = rest.trim().parse().ok();
        } else if block == "songsterr" && key == "audio" {
            saved.songsterr_sound = quoted(rest) == "songsterr";
        }
    }
    if let Some(name) = cover_path
        && let Ok(mut file) = zip.by_name(&name)
    {
        let mut data = Vec::new();
        if file.read_to_end(&mut data).is_ok() {
            saved.cover = Some(data);
        }
    }
    Some(saved)
}

/// Every feedpak in the folder, newest first.
pub fn list_saved(dir: &Path) -> Vec<Saved> {
    let mut found: Vec<Saved> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "feedpak") && p.is_file())
        .filter_map(|p| read_saved(&p))
        .collect();
    found.sort_by(|a, b| b.modified.cmp(&a.modified));
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tabpak-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_manifest_reads_as_yaml() {
        let manifest = json!({
            "title": "Zażółć \"gęślą\" jaźń",
            "duration": 12.5,
            "arrangements": [{"id": "bass", "name": "Electric Bass", "tuning": [-2, 0, 0, 0], "capo": 0}],
            "stems": [{"id": "full", "file": "stems/full.ogg", "default": true}],
            "songsterr": {"song_id": 403656, "video_id": "abc"},
        });
        let expected = "title: \"Zażółć \\\"gęślą\\\" jaźń\"\nduration: 12.5\narrangements:\n  - id: \"bass\"\n    name: \"Electric Bass\"\n    tuning: [-2, 0, 0, 0]\n    capo: 0\nstems:\n  - id: \"full\"\n    file: \"stems/full.ogg\"\n    default: true\nsongsterr:\n  song_id: 403656\n  video_id: \"abc\"\n";
        assert_eq!(yaml(&manifest), expected);
    }

    #[test]
    fn a_written_pack_reads_back_for_the_list() {
        let dir = scratch("roundtrip");
        let manifest = json!({
            "title": "Song: one",
            "artist": "Artist",
            "duration": 61.0,
            "arrangements": [{"id": "bass", "name": "Electric Bass"}, {"id": "lead", "name": "Lead"}],
            "cover": "cover.jpg",
            "songsterr": {"song_id": 42, "audio": "songsterr"},
        });
        let path = dir.join("Artist - Song one.feedpak");
        write_zip(&path, &[("manifest.yaml".into(), yaml(&manifest).into_bytes()), ("cover.jpg".into(), vec![1, 2, 3])]).unwrap();
        let saved = &list_saved(&dir)[0];
        assert_eq!((saved.title.as_str(), saved.artist.as_str(), saved.seconds), ("Song: one", "Artist", 61.0));
        assert_eq!(saved.parts, vec!["Electric Bass", "Lead"]);
        assert_eq!((saved.song_id, saved.songsterr_sound, saved.cover.clone()), (Some(42), true, Some(vec![1, 2, 3])));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn file_names_lose_what_finder_refuses() {
        assert_eq!(safe_name("AC/DC:  Back  in \"Black\"?"), "ACDC Back in Black");
        assert_eq!(safe_name("///"), "song");
    }
}
