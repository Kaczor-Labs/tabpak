//! Songsterr's public endpoints: search, a song's tracks, each track's notes, and the per-measure sync points it
//! keeps for YouTube videos of the song.

use std::io::Read;
use std::time::Duration;

use serde_json::Value;

const API: &str = "https://www.songsterr.com/api";
const TAB_CDN: &str = "https://dqsljvtekg760.cloudfront.net";
const MAX_BODY: u64 = 64 * 1024 * 1024;

/// General MIDI programs: guitars 24-31, basses 32-39.
pub fn is_bass_program(program: i64) -> bool {
    (32..40).contains(&program)
}

pub fn is_guitar_program(program: i64) -> bool {
    (24..32).contains(&program)
}

pub fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent("Mozilla/5.0")
        .build()
        .into();
    let resp = agent.get(url).call().map_err(|e| match e {
        ureq::Error::StatusCode(404) => "not found".to_string(),
        e => e.to_string(),
    })?;
    let mut data = Vec::new();
    resp.into_body()
        .into_with_config()
        .limit(MAX_BODY)
        .reader()
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.starts_with(&[0x1f, 0x8b]) {
        let mut plain = Vec::new();
        flate2::read::GzDecoder::new(&data[..])
            .read_to_end(&mut plain)
            .map_err(|e| e.to_string())?;
        return Ok(plain);
    }
    Ok(data)
}

fn get_json(url: &str) -> Result<Value, String> {
    serde_json::from_slice(&http_get(url)?).map_err(|e| e.to_string())
}

/// The song id in a Songsterr link (`...-tab-s403656`, `...-s403656t4`) or a bare number.
pub fn song_id_of(text: &str) -> Option<u64> {
    let t = text.trim();
    if !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) {
        return t.parse().ok();
    }
    if !t.contains("songsterr.com") {
        return None;
    }
    let path = t.split(['?', '#']).next().unwrap_or(t).trim_end_matches('/');
    let tail = &path[path.rfind("-s")? + 2..];
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    let rest = &tail[digits.len()..];
    let track_suffix = rest.is_empty() || (rest.starts_with('t') && rest[1..].bytes().all(|b| b.is_ascii_digit()));
    if digits.is_empty() || !track_suffix {
        return None;
    }
    digits.parse().ok()
}

/// "bass" or "guitar" when the link names the instrument (`...-bass-tab-s403656`).
pub fn instrument_in_link(text: &str) -> Option<&'static str> {
    let t = text.to_lowercase();
    if t.contains("-bass-tab-") {
        Some("bass")
    } else if t.contains("-guitar-tab-") || t.contains("-tab-s") {
        Some("guitar")
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SearchHit {
    pub song_id: u64,
    pub artist: String,
    pub title: String,
    pub has_bass: bool,
    pub has_guitar: bool,
}

pub fn search(pattern: &str) -> Result<Vec<SearchHit>, String> {
    let mut query = String::new();
    for b in pattern.trim().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => query.push(b as char),
            _ => query.push_str(&format!("%{b:02X}")),
        }
    }
    let found = get_json(&format!("{API}/songs?size=12&pattern={query}"))?;
    let empty = Vec::new();
    Ok(found
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|s| {
            let programs: Vec<i64> = s["tracks"]
                .as_array()?
                .iter()
                .map(|t| t["instrumentId"].as_i64().unwrap_or(-1))
                .collect();
            Some(SearchHit {
                song_id: s["songId"].as_u64()?,
                artist: s["artist"].as_str()?.to_string(),
                title: s["title"].as_str()?.to_string(),
                has_bass: programs.iter().any(|p| is_bass_program(*p)),
                has_guitar: programs.iter().any(|p| is_guitar_program(*p)),
            })
        })
        .filter(|h| h.has_bass || h.has_guitar)
        .collect())
}

pub fn meta(song_id: u64) -> Result<Value, String> {
    get_json(&format!("{API}/meta/{song_id}"))
}

pub fn track(meta: &Value, index: usize) -> Result<Value, String> {
    let (song, rev, image) = (&meta["songId"], &meta["revisionId"], meta["image"].as_str().unwrap_or(""));
    get_json(&format!("{TAB_CDN}/{song}/{rev}/{image}/{index}.json"))
}

/// The videos Songsterr has synced to this revision, each with one time per played measure.
pub fn videos(meta: &Value) -> Result<Vec<Value>, String> {
    let (song, rev) = (&meta["songId"], &meta["revisionId"]);
    let list = get_json(&format!("{API}/video-points/{song}/{rev}/list"))?;
    Ok(list.as_array().cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn song_ids_from_links() {
        let link = "https://www.songsterr.com/a/wsa/popek-bollywodzka-produkcja-pakistanskie-disco-bass-tab-s403656";
        assert_eq!(song_id_of(link), Some(403656));
        assert_eq!(song_id_of(&format!("{link}t4")), Some(403656));
        assert_eq!(song_id_of(&format!(" {link}/?utm=1 ")), Some(403656));
        assert_eq!(song_id_of("27"), Some(27));
        assert_eq!(song_id_of("master of puppets"), None);
        assert_eq!(song_id_of("https://example.com/x-s12"), None);
        assert_eq!(song_id_of("https://www.songsterr.com/a/wsa/x-tab-s12abc"), None);
    }

    #[test]
    fn instrument_from_link() {
        assert_eq!(instrument_in_link("https://www.songsterr.com/a/wsa/x-bass-tab-s1"), Some("bass"));
        assert_eq!(instrument_in_link("https://www.songsterr.com/a/wsa/x-tab-s1"), Some("guitar"));
        assert_eq!(instrument_in_link("403656"), None);
    }
}
