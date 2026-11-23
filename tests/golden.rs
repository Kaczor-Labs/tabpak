//! The converter's timing against Songsterr's own player on random well-formed tabs. Each line of
//! `tests/golden/timing.jsonl` holds a made-up tab and the play order, the bar lines and the note times
//! (seconds) Songsterr's player gives it. The numbers are measured, not written: when a line fails, the
//! converter changed.

use serde_json::Value;
use tabpak::chart;

/// Songsterr's times are kept to the millisecond.
const TOLERANCE: f64 = 0.0015;

fn floats(v: &Value) -> Vec<f64> {
    v.as_array().expect("a list").iter().map(|x| x.as_f64().expect("a number")).collect()
}

fn all_near(times: &[f64], pool: &[f64]) -> bool {
    times.iter().all(|t| pool.iter().any(|p| (p - t).abs() <= TOLERANCE))
}

#[test]
fn timing_matches_songsterrs_player_on_the_golden_tabs() {
    let lines: Vec<&str> = include_str!("golden/timing.jsonl").lines().collect();
    let mut wrong = Vec::new();
    for (number, line) in lines.iter().enumerate() {
        let case: Value = serde_json::from_str(line).expect("a golden line is JSON");
        let tab = &case["tab"];
        let order = chart::unroll(tab);
        let want_order: Vec<usize> = case["order"].as_array().expect("a list").iter().map(|x| x.as_u64().expect("a number") as usize).collect();
        if order != want_order {
            wrong.push(format!("line {}: play order", number + 1));
            continue;
        }
        let nominal: Vec<f64> = chart::walk(tab, &order).iter().map(|slot| slot.seconds).collect();
        let (starts, _) = chart::slot_times(&nominal, &[]);
        let (built, _) = chart::build_chart(tab, &starts, "Guitar");
        let times: Vec<f64> = ["notes", "chords"].iter().flat_map(|k| built[*k].as_array().expect("a list")).map(|n| n["t"].as_f64().expect("a time")).collect();
        let (want_starts, want_notes) = (floats(&case["starts"]), floats(&case["notes"]));
        if starts.len() != want_starts.len() || starts.iter().zip(&want_starts).any(|(a, b)| (a - b).abs() > TOLERANCE) {
            wrong.push(format!("line {}: bar lines", number + 1));
        } else if !all_near(&times, &want_notes) || !all_near(&want_notes, &times) {
            wrong.push(format!("line {}: note times", number + 1));
        }
    }
    assert!(wrong.is_empty(), "{} of {} golden tabs differ: {}", wrong.len(), lines.len(), wrong.join("; "));
}
