//! A Songsterr track to a feedpak chart: measures in playing order, each placed in time by the sync points, then
//! notes, chords, hand positions and the song's beat grid.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::songsterr::is_bass_program;

const GUITAR_OPEN: [i64; 6] = [40, 45, 50, 55, 59, 64]; // E2 A2 D3 G3 B3 E4, lowest string first
const BASS_OPEN: [i64; 6] = [28, 33, 38, 43, 47, 52];
const MAX_FRET: i64 = 24;
/// Notes this long (of a whole note) or longer get a sustain tail.
const SUSTAIN_FROM: Frac = Frac { n: 3, d: 16 };

/// A length in whole notes, kept exact so positions inside a measure don't drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frac {
    n: i64,
    d: i64,
}

impl Frac {
    pub const ZERO: Frac = Frac { n: 0, d: 1 };
    pub const ONE: Frac = Frac { n: 1, d: 1 };

    pub fn new(n: i64, d: i64) -> Frac {
        let d = if d == 0 { 1 } else { d };
        let g = gcd(n.abs(), d.abs()).max(1) * d.signum();
        Frac { n: n / g, d: d / g }
    }

    fn of(pair: &Value) -> Frac {
        Frac::new(pair[0].as_i64().unwrap_or(0), pair[1].as_i64().unwrap_or(1))
    }

    fn plus(self, o: Frac) -> Frac {
        Frac::new(self.n * o.d + o.n * self.d, self.d * o.d)
    }

    fn minus(self, o: Frac) -> Frac {
        Frac::new(self.n * o.d - o.n * self.d, self.d * o.d)
    }

    fn times(self, o: Frac) -> Frac {
        Frac::new(self.n * o.n, self.d * o.d)
    }

    fn float(self) -> f64 {
        self.n as f64 / self.d as f64
    }

    fn less(self, o: Frac) -> bool {
        self.n * o.d < o.n * self.d
    }

}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Python's truth of a JSON value: null, false, 0, "" and empty containers are false.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|x| x != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Round to a number of decimal places by the value's exact decimal expansion, so a time that sits half a
/// millisecond between two others lands the same way everywhere.
fn round_to(x: f64, places: usize) -> f64 {
    format!("{x:.places$}").parse().unwrap_or(x)
}

fn round3(x: f64) -> f64 {
    round_to(x, 3)
}

fn beats(voice: &Value) -> &[Value] {
    voice["beats"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn voices(measure: &Value) -> &[Value] {
    measure["voices"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn measures(track: &Value) -> &[Value] {
    track["measures"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// The time signature of every written measure: its own, else the one before it, else 4/4.
fn signatures(track: &Value) -> Vec<[i64; 2]> {
    let mut sig = [4i64, 4];
    measures(track)
        .iter()
        .map(|m| {
            if let (Some(n), Some(d)) = (m["signature"][0].as_i64(), m["signature"][1].as_i64())
                && n > 0
                && d > 0
            {
                sig = [n, d];
            }
            sig
        })
        .collect()
}

fn endings_of(measure: &Value) -> Vec<i64> {
    match &measure["alternateEnding"] {
        Value::Number(n) => n.as_i64().into_iter().collect(),
        Value::Array(a) => a.iter().filter_map(Value::as_i64).collect(),
        _ => Vec::new(),
    }
}

/// How many times a repeat sign plays its section; Songsterr's player stops counting at 32.
fn plays(measure: &Value) -> i64 {
    measure["repeat"].as_i64().unwrap_or(0).clamp(0, 32)
}

/// A repeated stretch of the tab: where it opens and closes, and the measure each numbered ending starts at.
struct Section {
    start: usize,
    end: usize,
    endings: HashMap<i64, usize>,
}

/// Measure indexes in the order they are played: repeats and numbered endings written out, by the rules of
/// Songsterr's own player (a repeat sign with no opening sign repeats from the first measure).
pub fn unroll(track: &Value) -> Vec<usize> {
    let ms = measures(track);
    let mut sections: Vec<Section> = Vec::new();
    let mut of: Vec<Option<usize>> = vec![None; ms.len()];
    let mut open = false;
    for (i, m) in ms.iter().enumerate() {
        if truthy(&m["repeatStart"]) {
            sections.push(Section { start: i, end: 0, endings: HashMap::new() });
            open = true;
        }
        let endings = endings_of(m);
        if let Some(last) = sections.len().checked_sub(1)
            && (open || !endings.is_empty())
        {
            of[i] = Some(last);
            sections[last].endings.extend(endings.iter().map(|e| (*e, i)));
        }
        if plays(m) > 0 {
            if sections.is_empty() {
                sections.push(Section { start: 0, end: 0, endings: endings.iter().map(|e| (*e, i)).collect() });
                of[..=i].fill(Some(0));
            }
            let last = sections.len() - 1;
            sections[last].end = i;
            open = false;
        }
    }

    let (mut order, mut i) = (Vec::new(), 0usize);
    // the section being played, which pass of it this is, and whether a numbered ending is under way
    let (mut current, mut pass, mut in_ending) = (None::<usize>, 0i64, false);
    while i < ms.len() && order.len() < 100_000 {
        order.push(i);
        if let Some(c) = of[i]
            && sections[c].start == i
            && current.is_none_or(|r| sections[r].start != i)
            && sections[c].end >= sections[c].start
        {
            current = Some(c);
            pass = 1;
        }
        let Some(section) = current.map(|r| &sections[r]) else {
            i += 1;
            continue;
        };
        if pass < plays(&ms[i]) {
            i = section.start;
            pass += 1;
            in_ending = false;
            continue;
        }
        i += 1;
        if i < ms.len() {
            let endings = endings_of(&ms[i]);
            if endings.contains(&pass) {
                in_ending = true;
            } else if !endings.is_empty() && in_ending {
                i = section.start;
                pass += 1;
                in_ending = false;
            } else if !endings.is_empty() {
                let own = section.endings.get(&pass).copied().filter(|at| *at != 0);
                i = own.unwrap_or(if pass < plays(&ms[section.end]) { section.end } else { section.end + 1 });
                in_ending = true;
            }
            if i > section.end {
                current = None;
                pass = 0;
            }
        }
    }
    order
}

/// One played measure: its time signature, its length in whole notes, how long it lasts at the tab's own tempo
/// and the quarter-note bpm at its start.
#[derive(Clone, Debug)]
pub struct Slot {
    pub sig: [i64; 2],
    pub length: f64,
    pub seconds: f64,
    pub bpm: f64,
    /// Where the tempo changes inside the measure: (how far into the measure, how far into its time), both
    /// 0 to 1, from (0, 0) to (1, 1). Empty when the measure runs at one tempo.
    pub warp: Vec<(f64, f64)>,
}

impl Slot {
    /// How far into the measure's time a place in the measure is, both 0 to 1. A place before the measure or
    /// past its end runs on at the tempo of the measure's nearest part.
    pub fn time_at(&self, place: f64) -> f64 {
        if self.warp.len() < 2 {
            return place;
        }
        let last = self.warp.len() - 1;
        let k = self.warp.iter().position(|(x, _)| *x >= place).unwrap_or(last).clamp(1, last);
        let ((x0, y0), (x1, y1)) = (self.warp[k - 1], self.warp[k]);
        if x1 > x0 { y0 + (y1 - y0) * (place - x0) / (x1 - x0) } else { y1 }
    }
}

/// One tempo entry of the tab, as Songsterr's player carries it through its steps: `position` counts 960 to
/// a quarter note from the start of `measure`, `bpm` counts notes of `unit` (4 a quarter, 2 a half, 8 an eighth).
#[derive(Clone, Debug, PartialEq)]
struct Tempo {
    measure: i64,
    position: f64,
    bpm: f64,
    unit: i64,
    dotted: bool,
    linear: bool,
    /// For a fermata's slow stretch: how long it holds, in the units of `position`.
    span: f64,
}

/// Position units (960 to a quarter note) are cut to this many steps a quarter note, as the player does.
const STEPS_PER_QUARTER: f64 = 16384.0;

/// Places in a measure are sums of thirds and fifths of notes: two that differ by less than this are the same place.
const EDGE: f64 = 1e-9;

/// JavaScript's rounding: a half goes up.
fn round_half_up(x: f64) -> f64 {
    (x + 0.5).floor()
}

impl Tempo {
    fn of(v: &Value) -> Option<Tempo> {
        Some(Tempo {
            measure: v["measure"].as_i64().unwrap_or(0),
            position: v["position"].as_f64().unwrap_or(0.0),
            bpm: v["bpm"].as_f64()?,
            unit: v["type"].as_i64().unwrap_or(4),
            dotted: truthy(&v["dotted"]),
            linear: truthy(&v["linear"]),
            span: 0.0,
        })
    }

    fn plain(measure: i64, position: f64, bpm: f64, unit: i64) -> Tempo {
        Tempo { measure, position, bpm, unit, dotted: false, linear: false, span: 0.0 }
    }

    /// Quarter notes a minute, cut to a whole number.
    fn quarter_bpm(&self) -> f64 {
        let bpm = match self.unit {
            8 => self.bpm / 2.0,
            2 => self.bpm * 2.0,
            _ => self.bpm,
        };
        (if self.dotted { bpm * 1.5 } else { bpm }).floor()
    }
}

/// A measure's length in tempo position units.
fn position_length(sig: [i64; 2]) -> f64 {
    3840.0 * sig[0] as f64 / sig[1] as f64
}

fn by_place(a: &Tempo, b: &Tempo) -> std::cmp::Ordering {
    a.measure.cmp(&b.measure).then(a.position.total_cmp(&b.position))
}

/// How long a fermata holds: the beat it stands on, or the smallest halving of the beat that it starts on.
fn fermata_span(position: f64, sig: [i64; 2]) -> f64 {
    let beat = position_length(sig) / sig[0] as f64;
    let part = (position / beat).rem_euclid(1.0);
    if part.abs() < 1e-9 {
        return beat;
    }
    for halvings in 1..=7 {
        let pieces = 2f64.powi(halvings);
        if (part * pieces - (part * pieces).round()).abs() < 1e-9 {
            return beat / pieces;
        }
    }
    beat / 128.0
}

/// The tempo list with every fermata written in as a slow stretch and a return to the tempo after it.
fn with_fermatas(tempo: Vec<Tempo>, track: &Value, sigs: &[[i64; 2]]) -> Vec<Tempo> {
    // (measure, position, length 0 to 1)
    let mut fermatas: Vec<(i64, f64, f64)> = track["automations"]["fermata"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .map(|f| (f["measure"].as_i64().unwrap_or(0), f["position"].as_f64().unwrap_or(0.0), f["length"].as_f64().unwrap_or(0.0)))
        .collect();
    if fermatas.is_empty() || tempo.is_empty() {
        return tempo;
    }
    fermatas.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    fn put(out: &mut Vec<Tempo>, entry: Tempo) {
        match out.iter_mut().find(|e| e.measure == entry.measure && e.position == entry.position) {
            Some(same) => *same = entry,
            None => out.push(entry),
        }
    }
    let mut carried = tempo[0].clone();
    let mut out: Vec<Tempo> = Vec::new();
    for (t, &sig) in sigs.iter().enumerate() {
        let t = t as i64;
        let mut here: Vec<Tempo> = tempo.iter().filter(|e| e.measure == t).cloned().collect();
        let first_here = here.first().cloned().unwrap_or_else(|| carried.clone());
        here.sort_by(|a, b| a.position.total_cmp(&b.position));
        if t == 0 && !here.iter().any(|e| e.position == 0.0) {
            out.push(Tempo { measure: 0, position: 0.0, ..first_here.clone() });
        }
        out.extend(here.iter().cloned());
        for &(_, at, length) in fermatas.iter().filter(|f| f.0 == t) {
            let span = fermata_span(at, sig);
            let end = at + span;
            let before = here.iter().rev().find(|e| e.position <= at).unwrap_or(&carried);
            let after = here.iter().rev().find(|e| e.position <= end).unwrap_or(&carried);
            let slow = round_half_up(before.bpm * (-7.0 / 12.0 * (0.2 + length * 0.8) + 11.0 / 12.0));
            put(&mut out, Tempo { span, ..Tempo::plain(t, at, slow, before.unit) });
            let back = Tempo::plain(t, end, after.bpm, after.unit);
            if end >= position_length(sig) {
                let next = t + 1;
                let taken = tempo.iter().any(|e| e.measure == next && e.position == 0.0) || fermatas.iter().any(|f| f.0 == next && f.1 == 0.0);
                if (next as usize) < sigs.len() && !taken {
                    put(&mut out, Tempo { measure: next, position: 0.0, ..back });
                }
            } else if !out.iter().any(|e| e.measure == t && e.position == end) && !fermatas.iter().any(|f| f.0 == t && f.1 == end) {
                put(&mut out, back);
            }
        }
        carried = here.last().cloned().unwrap_or(first_here);
    }
    // in written order, without doubles and without anything that starts inside a fermata's slow stretch
    out.sort_by(by_place);
    let mut kept: Vec<Tempo> = Vec::new();
    let mut held_until: HashMap<i64, f64> = HashMap::new();
    for e in out {
        if kept.iter().any(|k| k.measure == e.measure && k.position == e.position && k.bpm == e.bpm) {
            continue;
        }
        let held = held_until.get(&e.measure).copied();
        if held.is_some_and(|until| e.position > 0.0 && e.position < until) {
            continue;
        }
        if e.span > 0.0 && held.is_none_or(|until| until == 0.0 || e.position + e.span > until) {
            held_until.insert(e.measure, e.position + e.span);
        }
        kept.push(e);
    }
    kept
}

/// The tempo list with every gradual change written in as steps: one step a quarter note, 64 at most, from
/// the entry before a `linear` entry up to it.
fn with_ramps(tempo: Vec<Tempo>, sigs: &[[i64; 2]]) -> Vec<Tempo> {
    if tempo.len() < 2 || !tempo.iter().any(|e| e.linear) {
        return tempo;
    }
    let lengths: Vec<f64> = sigs.iter().map(|s| position_length(*s)).collect();
    let mut offsets = Vec::with_capacity(lengths.len());
    let mut sum = 0.0;
    for len in &lengths {
        offsets.push(sum);
        sum += len;
    }
    let spot = |e: &Tempo| usize::try_from(e.measure).ok().and_then(|m| offsets.get(m)).copied().unwrap_or(0.0) + e.position;
    let mut sorted = tempo.clone();
    sorted.sort_by(|a, b| spot(a).total_cmp(&spot(b)));
    let mut taken: Vec<(i64, f64)> = sorted.iter().map(|e| (e.measure, e.position)).collect();
    let mut steps: Vec<Tempo> = Vec::new();
    for pair in sorted.windows(2) {
        let (from, to) = (&pair[0], &pair[1]);
        let (slow, fast) = (from.quarter_bpm(), to.quarter_bpm());
        let (start, stretch) = (spot(from), spot(to) - spot(from));
        if !to.linear || slow == fast || stretch <= 0.0 {
            continue;
        }
        let count = round_half_up(stretch / 960.0).clamp(1.0, 64.0) as usize;
        for k in 1..count {
            let share = k as f64 / count as f64;
            let at = start + stretch * share;
            let m = (0..offsets.len()).find(|&m| at < offsets[m] + lengths[m]).unwrap_or(offsets.len().saturating_sub(1));
            let position = round_half_up(at - offsets.get(m).copied().unwrap_or(0.0)).clamp(0.0, (lengths.get(m).copied().unwrap_or(1.0) - 1.0).max(0.0));
            if !taken.contains(&(m as i64, position)) {
                taken.push((m as i64, position));
                steps.push(Tempo::plain(m as i64, position, round_half_up(slow + (fast - slow) * share), 4));
            }
        }
    }
    if steps.is_empty() {
        return tempo;
    }
    let mut all = tempo;
    all.extend(steps);
    all.sort_by(by_place);
    all
}

/// Every tempo entry of the tab the way Songsterr's player ends up using it. The tempo lives in the track's
/// `automations`; measures count from the first entry's own measure; no entry at all means 100 bpm.
fn tempo_list(track: &Value, sigs: &[[i64; 2]]) -> Vec<Tempo> {
    let auto = &track["automations"];
    let mut read: Vec<Tempo> = auto["tempo"].as_array().map(Vec::as_slice).unwrap_or(&[]).iter().filter_map(Tempo::of).collect();
    if read.is_empty() {
        read.push(Tempo::plain(0, 0.0, 100.0, 4));
    }
    let first = read[0].measure;
    let mut list: Vec<Tempo> = Vec::new();
    for mut e in read {
        e.measure -= first;
        match list.iter_mut().find(|k| k.measure == e.measure && k.position == e.position) {
            Some(same) => *same = e,
            None => list.push(e),
        }
    }
    let list = with_fermatas(list, track, sigs);
    if truthy(&auto["gradualTempo"]) { with_ramps(list, sigs) } else { list }
}

/// The tempo changes of one written measure: (whole notes from its start, quarter-note bpm). Two entries on
/// one step: the first listed stands.
fn tempo_changes(list: &[Tempo], measure: usize) -> Vec<(f64, f64)> {
    let mut steps: Vec<(f64, f64)> = Vec::new();
    for e in list.iter().filter(|e| e.measure == measure as i64) {
        let step = (e.position * STEPS_PER_QUARTER / 960.0).floor();
        if e.quarter_bpm() > 0.0 && !steps.iter().any(|s| s.0 == step) {
            steps.push((step, e.quarter_bpm()));
        }
    }
    steps.into_iter().map(|(step, bpm)| (step / (4.0 * STEPS_PER_QUARTER), bpm)).collect()
}

/// A swing ("triplet feel"): pairs of `unit` notes played long-short (or short-long) instead of evenly.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Feel {
    /// 8 for eighth-note pairs, 16 for sixteenths.
    unit: i64,
    /// How much of its written length the first and the second note of a pair get.
    first: Frac,
    second: Frac,
    /// The plain triplet feel leaves written duplets alone.
    plain: bool,
}

/// The swing of every written measure: a measure without its own mark keeps the one before it.
fn feels(track: &Value) -> Vec<Option<Feel>> {
    let mut current = None;
    measures(track)
        .iter()
        .map(|m| {
            if let Some(name) = m["tripletFeel"].as_str().filter(|n| !n.is_empty()) {
                let unit = if name.ends_with("8th") { 8 } else { 16 };
                let kind = name.split(|c: char| c.is_ascii_digit()).next().unwrap_or("");
                let shares = match kind {
                    "" => Some(((4, 3), (2, 3))),
                    "dotted" => Some(((3, 2), (1, 2))),
                    "scottish" => Some(((1, 2), (3, 2))),
                    _ => None,
                };
                current = shares.filter(|_| name != "off").map(|(a, b)| Feel { unit, first: Frac::new(a.0, a.1), second: Frac::new(b.0, b.1), plain: kind.is_empty() });
            }
            current
        })
        .collect()
}

/// The sounding length of every beat of a measure under a swing, voice by voice. A pair is left even when a
/// note that starts in it does not fit the swing's grid, or when the pair runs past the end of the voice.
fn swung(measure: &Value, feel: Feel) -> Vec<Vec<Frac>> {
    let pair_of = |at: Frac| (at.n * feel.unit).div_euclid(at.d * 2);
    let plays = |v: &Value| !truthy(&v["rest"]) && !beats(v).is_empty();
    let lengths = |v: &Value| beats(v).iter().map(|b| Frac::of(&b["duration"])).collect::<Vec<_>>();
    let starts = |v: &Value| {
        let mut at = Frac::ZERO;
        let places: Vec<Frac> = beats(v)
            .iter()
            .map(|b| {
                let here = at;
                if !truthy(&b["graceNote"]) {
                    at = at.plus(Frac::of(&b["duration"]));
                }
                here
            })
            .collect();
        (places, at)
    };
    let mut even: Vec<i64> = Vec::new();
    for v in voices(measure).iter().filter(|v| plays(v)) {
        for (b, at) in beats(v).iter().zip(starts(v).0) {
            let d = Frac::of(&b["duration"]);
            let off_grid = (d.n * feel.unit) % d.d != 0;
            if !truthy(&b["graceNote"]) && (off_grid || feel.plain && b["tuplet"].as_i64() == Some(2)) {
                even.push(pair_of(at));
            }
        }
    }
    let unit = Frac::new(1, feel.unit);
    voices(measure)
        .iter()
        .map(|v| {
            let written = lengths(v);
            if !plays(v) {
                return written;
            }
            let (places, total) = starts(v);
            let sounding = |at: Frac| {
                let pair = pair_of(at);
                if even.contains(&pair) || total.less(Frac::new((pair + 1) * 2, feel.unit)) {
                    return at;
                }
                let begin = Frac::new(pair * 2, feel.unit);
                let into = at.minus(begin);
                if unit.less(into) { begin.plus(unit.times(feel.first)).plus(into.minus(unit).times(feel.second)) } else { begin.plus(into.times(feel.first)) }
            };
            beats(v)
                .iter()
                .zip(written.iter().zip(places))
                .map(|(b, (d, at))| if truthy(&b["graceNote"]) { *d } else { sounding(at.plus(*d)).minus(sounding(at)) })
                .collect()
        })
        .collect()
}

/// Where a beat sounds in its measure: its start and its length, in whole notes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Place {
    start: f64,
    len: f64,
}

/// The sounding length of every beat of a measure in whole notes, voice by voice: swung where the measure
/// swings, and never longer than the measure's time signature (`total`).
fn sounding_lengths(measure: &Value, total: f64, feel: Option<Feel>) -> Vec<Vec<f64>> {
    let exact: Vec<Vec<Frac>> = match feel {
        Some(feel) => swung(measure, feel),
        None => voices(measure).iter().map(|v| beats(v).iter().map(|b| Frac::of(&b["duration"])).collect()).collect(),
    };
    exact.iter().map(|v| v.iter().map(|d| d.float().min(total)).collect()).collect()
}

/// How much time the last note of a voice before measure `idx` can lend to grace notes that open the measure.
fn room_before(track: &Value, sigs: &[[i64; 2]], feels: &[Option<Feel>], idx: usize, voice: usize) -> Option<f64> {
    let ms = measures(track);
    (0..idx).rev().find_map(|k| {
        let bs = beats(voices(&ms[k]).get(voice)?);
        let last = bs.iter().rposition(|b| !truthy(&b["graceNote"]))?;
        let total = sigs[k][0] as f64 / sigs[k][1] as f64;
        let lengths = sounding_lengths(&ms[k], total, feels[k]).swap_remove(voice);
        let start: f64 = (0..last).filter(|&b| !truthy(&bs[b]["graceNote"])).map(|b| lengths[b]).sum();
        Some(if last + 1 == bs.len() && start < total - EDGE { total - start } else { lengths[last] })
    })
}

/// Where every beat of a measure sounds, voice by voice, the way Songsterr's player places them: beats follow
/// each other by their own lengths (after swing), grace notes take their time from a neighbour, and the last
/// beat is cut at the bar line. `total` is the measure's time signature in whole notes; `lent` tells how much
/// time the note before the measure can lend, by voice.
fn beat_places(measure: &Value, total: f64, feel: Option<Feel>, first_measure: bool, lent: &dyn Fn(usize) -> Option<f64>) -> Vec<Vec<Place>> {
    voices(measure)
        .iter()
        .zip(sounding_lengths(measure, total, feel))
        .enumerate()
        .map(|(voice, (v, lengths))| {
            let bs = beats(v);
            let grace: Vec<bool> = bs.iter().map(|b| truthy(&b["graceNote"])).collect();
            let mut out: Vec<Place> = lengths.iter().map(|len| Place { start: 0.0, len: *len }).collect();
            let mut end = 0.0;
            for (place, _) in out.iter_mut().zip(&grace).filter(|(_, g)| !**g) {
                place.start = end;
                end += place.len;
            }
            // groups of grace notes: (first, one past the last, played on the beat)
            let mut groups: Vec<(usize, usize, bool)> = Vec::new();
            let mut on_beat: Vec<bool> = bs.iter().map(|b| b["graceNote"].as_str() == Some("onBeat")).collect();
            let mut i = 0;
            while i < bs.len() {
                let j = (i..bs.len()).find(|&k| !grace[k]).unwrap_or(bs.len());
                if j > i {
                    // before-the-beat grace notes on the very first beat of the tab have nothing to take time from
                    if first_measure && voice == 0 && i == 0 && !on_beat[i] {
                        on_beat[i..j].fill(true);
                    }
                    groups.push((i, j, on_beat[i]));
                }
                i = j + 1;
            }
            // a group is squeezed into half of the note it takes its time from, and into one grace note's length
            for &(i, j, on) in &groups {
                let room = match (on, i.checked_sub(1)) {
                    (true, _) if j < bs.len() => Some(if j + 1 == bs.len() && out[j].start < total - EDGE { total - out[j].start } else { out[j].len }),
                    (true, _) => None,
                    (false, Some(p)) => Some(if p + 1 == bs.len() && out[p].start < total - EDGE { total - out[p].start } else { out[p].len }),
                    (false, None) => lent(voice),
                };
                if let Some(room) = room {
                    let dots = (i..j).map(|k| bs[k]["dots"].as_i64().unwrap_or(0)).max().unwrap_or(0);
                    let share = [0.5, 0.25, 0.125][dots.clamp(0, 2) as usize];
                    let sum: f64 = out[i..j].iter().map(|g| g.len).sum();
                    let most = f64::min(room * (1.0 - share), sum / (j - i) as f64);
                    if sum > most {
                        out[i..j].iter_mut().for_each(|g| g.len = most / (j - i) as f64);
                    }
                }
            }
            // each grace note starts from the next plain note: one played on the beat pushes that note and
            // the on-beat grace notes after it later; one played before the beat moves itself and the
            // before-beat grace notes ahead of it earlier, and shortens the note before
            let mut shift = vec![0.0f64; bs.len()];
            for &(i, j, _) in &groups {
                let after = (j < bs.len()).then_some(j);
                for k in i..j {
                    let len = out[k].len;
                    out[k].start = after.map_or(total, |n| out[n].start);
                    if on_beat[k] {
                        (k + 1..j).take_while(|&n| on_beat[n]).for_each(|n| shift[n] += len);
                        if let Some(n) = after {
                            out[n].len -= len;
                            shift[n] += len;
                        }
                    } else {
                        shift[k] -= len;
                        (i..k).rev().take_while(|&n| !on_beat[n]).for_each(|n| shift[n] -= len);
                        if let Some(p) = i.checked_sub(1) {
                            out[p].len = (out[p].len - len).max(0.0);
                        }
                    }
                }
            }
            out.iter_mut().zip(&shift).for_each(|(place, by)| place.start += by);
            for (place, g) in out.iter_mut().zip(&grace) {
                if place.start + place.len > total + EDGE && (*g || place.start < total - EDGE) {
                    place.len = (total - place.start).max(0.0);
                }
            }
            out
        })
        .collect()
}

/// The length of every written measure in whole notes: its time signature, whatever its beats add up to.
/// Only a tab marked as starting with a pickup has a first measure as long as its longest voice, less the
/// time its last note lends to grace notes that open the next measure.
fn measure_lengths(track: &Value, sigs: &[[i64; 2]], feels: &[Option<Feel>]) -> Vec<f64> {
    let (ms, pickup) = (measures(track), truthy(&track["anacrusis"]));
    let total = |i: usize| sigs[i][0] as f64 / sigs[i][1] as f64;
    let is_grace = |b: &Value| truthy(&b["graceNote"]);
    (0..ms.len())
        .map(|i| {
            if !pickup || i != 0 && !truthy(&ms[i]["anacrusis"]) {
                return total(i);
            }
            let mut places = beat_places(&ms[i], total(i), feels[i], i == 0, &|_| None);
            for (v, voice) in places.iter_mut().enumerate() {
                let bs = beats(&voices(&ms[i])[v]);
                // the next measure where this voice plays: its opening grace notes take their time from here
                let (Some(last), Some(k)) = (bs.iter().rposition(|b| !is_grace(b)), (i + 1..ms.len()).find(|&k| voices(&ms[k]).get(v).is_some_and(|x| !beats(x).is_empty()))) else {
                    continue;
                };
                let lens = beat_places(&ms[k], total(k), feels[k], false, &|v| room_before(track, sigs, feels, k, v)).swap_remove(v);
                let mut left = total(i) - voice[last].start;
                for (_, grace) in beats(&voices(&ms[k])[v]).iter().zip(lens).take_while(|(b, _)| is_grace(b)).filter(|(b, _)| b["graceNote"].as_str() != Some("onBeat")) {
                    if last + 1 == bs.len() {
                        left -= grace.len;
                        voice[last].len = voice[last].len.min(left);
                    } else {
                        voice[last].len -= grace.len;
                    }
                }
            }
            places.iter().map(|v| v.iter().map(|p| p.len).sum::<f64>()).fold(0.0, f64::max)
        })
        .collect()
}

/// Every played measure at the tab's own tempo. The tempo at any moment is the last change the playing has
/// passed, so a repeat that jumps back keeps the tempo it left with; before the first change it is 100 bpm.
pub fn walk(track: &Value, order: &[usize]) -> Vec<Slot> {
    let sigs = signatures(track);
    let lengths = measure_lengths(track, &sigs, &feels(track));
    let list = tempo_list(track, &sigs);
    // every change by where the playing meets it, in whole notes from the start
    let (mut changes, mut begins, mut at) = (Vec::new(), Vec::with_capacity(order.len()), 0.0f64);
    for &idx in order {
        begins.push(at);
        changes.extend(tempo_changes(&list, idx).into_iter().map(|(place, bpm)| (at + place, bpm)));
        at += lengths[idx];
    }
    changes.sort_by(|a, b| a.0.total_cmp(&b.0));

    let (mut out, mut next, mut bpm) = (Vec::with_capacity(order.len()), 0usize, 100.0f64);
    for (slot, &idx) in order.iter().enumerate() {
        let (from, to) = (begins[slot], begins[slot] + lengths[idx]);
        while next < changes.len() && changes[next].0 <= from {
            bpm = changes[next].1;
            next += 1;
        }
        let (start_bpm, mut x, mut seconds, mut curve) = (bpm, from, 0.0f64, vec![(0.0, 0.0)]);
        while next < changes.len() && changes[next].0 < to {
            seconds += (changes[next].0 - x) * 240.0 / bpm;
            x = changes[next].0;
            bpm = changes[next].1;
            next += 1;
            curve.push(((x - from) / lengths[idx], seconds));
        }
        seconds += (to - x) * 240.0 / bpm;
        curve.push((1.0, seconds));
        let warp = if curve.len() == 2 || seconds <= 0.0 { Vec::new() } else { curve.iter().map(|(x, t)| (*x, t / seconds)).collect() };
        out.push(Slot { sig: sigs[idx], length: lengths[idx], seconds, bpm: start_bpm, warp });
    }
    out
}

/// Start time of every played measure plus the end of the last one, and how many had no sync point.
///
/// Songsterr keeps one sync point per played measure. Where it keeps fewer (seen: a repeated first measure
/// under one point), a gap that is two or more measures long is shared out between them. Measures past the
/// last point run at the tab's tempo, scaled by how the synced ones ran.
pub fn slot_times(nominal: &[f64], points: &[f64]) -> (Vec<f64>, usize) {
    let n = nominal.len();
    if n == 0 {
        return (vec![0.0], 0);
    }
    if points.is_empty() {
        let (mut starts, mut t) = (Vec::with_capacity(n + 1), 0.0);
        for d in nominal {
            starts.push(t);
            t += d;
        }
        starts.push(t);
        return (starts, 0);
    }
    let mut starts: Vec<Option<f64>> = vec![None; n];
    let mut deficit = n.saturating_sub(points.len()) as i64;
    let (mut i, mut j) = (0usize, 0usize);
    let mut ratios = Vec::new();
    while i < n && j < points.len() {
        starts[i] = Some(points[j]);
        let mut k = 1usize;
        let gap = points.get(j + 1).map(|next| next - points[j]);
        if let Some(gap) = gap {
            if deficit > 0 && nominal[i] > 0.0 {
                let measures_in_gap = (gap / nominal[i]).round_ties_even() as i64;
                k = measures_in_gap.min(deficit + 1).min((n - i) as i64).max(1) as usize;
            }
            let total: f64 = nominal[i..i + k].iter().sum();
            if total > 0.0 {
                ratios.push(gap / total);
                let mut t = points[j];
                for q in 1..k {
                    t += gap * nominal[i + q - 1] / total;
                    starts[i + q] = Some(t);
                }
            }
        }
        deficit -= k as i64 - 1;
        i += k;
        j += 1;
    }
    ratios.sort_by(f64::total_cmp);
    let ratio = ratios.get(ratios.len() / 2).copied().unwrap_or(1.0);
    let unsynced = starts.iter().filter(|s| s.is_none()).count();
    let mut out: Vec<f64> = Vec::with_capacity(n + 1);
    for q in 0..n {
        let t = starts[q].unwrap_or_else(|| out[q - 1] + nominal[q - 1] * ratio);
        out.push(t);
    }
    out.push(out[n - 1] + nominal[n - 1] * ratio);
    (out, unsynced)
}

/// What a tied note hands to the note it lengthens.
#[derive(Default)]
struct Carry<'a> {
    slide: Option<&'a str>,
    hp: bool,
    vibrato: bool,
    let_ring: bool,
}

struct RawNote<'a> {
    t: f64,
    dur: f64,
    len: Frac,
    s: i64,
    f: i64,
    slot: usize,
    beat: &'a Value,
    note: &'a Value,
    bends: Vec<(f64, f64, &'a Value)>,
    tied: bool,
    carry: Carry<'a>,
}

/// Every struck or tied note with its time, length and the Songsterr beat and note it came from.
fn raw_notes<'a>(track: &'a Value, order: &[usize], slots: &[Slot], starts: &[f64], capo: i64) -> Vec<RawNote<'a>> {
    let ms = measures(track);
    let strings = track["strings"].as_i64().unwrap_or(6);
    let (sigs, feels) = (signatures(track), feels(track));
    let mut notes = Vec::new();
    for (slot, &idx) in order.iter().enumerate() {
        let m = &ms[idx];
        // a place in whole notes from the measure's start, as a time. A grace note before the bar line sits in
        // the measure before, a beat past the end of an over-full measure in the one after, at their tempo.
        let time = |place: f64| {
            let (mut k, mut place) = (slot, place);
            while place < 0.0 && k > 0 {
                k -= 1;
                place += slots[k].length;
            }
            while place >= slots[k].length && k + 1 < slots.len() {
                place -= slots[k].length;
                k += 1;
            }
            let length = slots[k].length;
            // nothing sounds before the tab starts
            starts[k] + (starts[k + 1] - starts[k]) * if length > 0.0 { slots[k].time_at(place.max(0.0) / length) } else { 0.0 }
        };
        let places = beat_places(m, sigs[idx][0] as f64 / sigs[idx][1] as f64, feels[idx], idx == 0, &|voice| room_before(track, &sigs, &feels, idx, voice));
        for (voice, places) in voices(m).iter().zip(places) {
            for (beat, place) in beats(voice).iter().zip(places) {
                let t = time(place.start);
                let dur = time(place.start + place.len) - t;
                let len = if truthy(&beat["graceNote"]) { Frac::new(1, 64) } else { Frac::of(&beat["duration"]) };
                for n in beat["notes"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
                    let (Some(s), Some(f)) = (n["string"].as_i64(), n["fret"].as_i64()) else {
                        continue;
                    };
                    if truthy(&n["rest"]) || !n["string"].is_i64() && !n["string"].is_u64() || !(0..strings).contains(&s) {
                        continue;
                    }
                    notes.push(RawNote {
                        t,
                        dur,
                        len,
                        s: strings - 1 - s,
                        f: (f + capo).min(MAX_FRET + capo),
                        slot,
                        beat,
                        note: n,
                        bends: if truthy(&n["bend"]) { vec![(t, dur, &n["bend"])] } else { Vec::new() },
                        tied: false,
                        carry: Carry::default(),
                    });
                }
            }
        }
    }
    notes.sort_by(|a, b| a.t.total_cmp(&b.t).then(a.s.cmp(&b.s)));
    notes
}

/// A tied note lengthens the note before it on its string and passes on what it carries.
fn merge_ties(notes: Vec<RawNote>) -> Vec<RawNote> {
    let mut last: HashMap<i64, usize> = HashMap::new();
    let mut out: Vec<RawNote> = Vec::with_capacity(notes.len());
    for n in notes {
        if truthy(&n.note["tie"])
            && let Some(&p) = last.get(&n.s)
        {
            let prev = &mut out[p];
            prev.dur = n.t + n.dur - prev.t;
            prev.len = prev.len.plus(n.len);
            prev.tied = true;
            prev.bends.extend(n.bends);
            if let Some(slide) = n.note["slide"].as_str().filter(|s| !s.is_empty()) {
                prev.carry.slide = Some(slide);
            }
            prev.carry.hp |= truthy(&n.note["hp"]);
            prev.carry.vibrato |= truthy(&n.note["leftHandVibrato"]);
            prev.carry.let_ring |= truthy(&n.beat["letRing"]);
            continue;
        }
        last.insert(n.s, out.len());
        out.push(n);
    }
    out
}

/// (peak in semitones, intent, curve) from Guitar Pro bend points (tone 100 = a whole step).
fn bend_fields(n: &RawNote) -> (f64, i64, Vec<Value>) {
    let mut curve: Vec<(f64, f64)> = Vec::new();
    for (start, dur, bend) in &n.bends {
        for p in bend["points"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
            let at = start - n.t + dur * p["position"].as_f64().unwrap_or(0.0) / 60.0;
            let v = round_to(p["tone"].as_f64().unwrap_or(0.0) / 50.0, 2);
            curve.push((round3(at.max(0.0)), v));
        }
    }
    if curve.is_empty() {
        return (0.0, 0, Vec::new());
    }
    curve.sort_by(|a, b| a.0.total_cmp(&b.0));
    let peak = curve.iter().map(|p| p.1).fold(f64::MIN, f64::max);
    let (first, last) = (curve[0].1, curve[curve.len() - 1].1);
    let intent = if first > 0.0 {
        if last < first { 3 } else { 2 }
    } else if last == 0.0 && peak > 0.0 {
        4
    } else {
        0
    };
    (peak, intent, curve.iter().map(|(t, v)| json!({"t": t, "v": v})).collect())
}

/// Songsterr's marks to feedpak note fields, one map per note in the same order.
fn chart_notes(notes: &[RawNote]) -> Vec<Map<String, Value>> {
    let mut last_on_string: HashMap<i64, usize> = HashMap::new();
    let mut following: Vec<Option<usize>> = vec![None; notes.len()];
    for (i, n) in notes.iter().enumerate() {
        if let Some(prev) = last_on_string.insert(n.s, i) {
            following[prev] = Some(i);
        }
    }
    let mut legato: HashMap<usize, &str> = HashMap::new(); // set by the note before, on the same string
    let mut out = Vec::with_capacity(notes.len());
    for (i, n) in notes.iter().enumerate() {
        let (src, beat) = (n.note, n.beat);
        let nxt = following[i].map(|k| &notes[k]);
        let mut f = Map::new();
        f.insert("t".into(), json!(round3(n.t)));
        f.insert("s".into(), json!(n.s));
        f.insert("f".into(), json!(n.f));
        let room = nxt.map(|x| x.t - n.t).unwrap_or(n.dur);
        let dead = truthy(&src["dead"]);
        let slide = n.carry.slide.or(src["slide"].as_str()).unwrap_or("");
        let vibrato = n.carry.vibrato || truthy(&src["leftHandVibrato"]) || truthy(&beat["vibratoWithTremoloBar"]);
        let let_ring = n.carry.let_ring || truthy(&beat["letRing"]);
        let tremolo = truthy(&beat["tremolo"]);
        let (peak, intent, curve) = bend_fields(n);

        let held = !n.len.less(SUSTAIN_FROM)
            || n.tied
            || vibrato
            || let_ring
            || peak > 0.0
            || tremolo
            || matches!(slide, "legato" | "shift" | "downwards" | "upwards");
        let mut sus = 0.0;
        if held && !dead && !truthy(&src["staccato"]) {
            sus = n.dur - f64::min(0.06, n.dur * 0.15);
            if !let_ring {
                sus = f64::min(sus, f64::max(0.0, room - 0.02));
            }
        }

        match (slide, nxt) {
            ("legato" | "shift", Some(x)) if x.f != n.f && n.f > 0 && x.f > 0 => {
                f.insert("sl".into(), json!(x.f));
                sus = room.max(0.0);
                if slide == "legato" {
                    f.insert("ln".into(), json!(true));
                }
            }
            ("downwards", _) if n.f > 1 => {
                f.insert("slu".into(), json!((n.f - 4).max(1)));
            }
            ("upwards", _) if n.f > 0 => {
                f.insert("slu".into(), json!((n.f + 4).min(MAX_FRET)));
            }
            _ => {}
        }

        if (n.carry.hp || truthy(&src["hp"]))
            && let Some(k) = following[i]
            && notes[k].f != n.f
        {
            legato.insert(k, if notes[k].f > n.f { "ho" } else { "po" });
        }
        if let Some(mark) = legato.get(&i) {
            f.insert((*mark).into(), json!(true));
        }

        if sus > 0.0 {
            f.insert("sus".into(), json!(round3(sus)));
        }
        if peak > 0.0 {
            f.insert("bn".into(), json!(peak));
            if intent != 0 {
                f.insert("bt".into(), json!(intent));
            }
            f.insert("bnv".into(), Value::Array(curve));
        }
        let marks = [
            ("fhm", dead),
            ("pm", truthy(&beat["palmMute"])),
            ("ac", truthy(&src["accentuated"])),
            ("vb", vibrato),
            ("tr", tremolo),
            ("tp", truthy(&beat["tapping"])),
            ("slp", truthy(&beat["slapping"])),
            ("plk", truthy(&beat["popping"])),
        ];
        for (key, on) in marks {
            if on {
                f.insert(key.into(), json!(true));
            }
        }
        match src["harmonic"].as_str() {
            Some("natural" | "tapped") => {
                f.insert("hm".into(), json!(true));
            }
            Some(h) if !h.is_empty() => {
                f.insert("hp".into(), json!(true));
            }
            _ => {}
        }
        match beat["pickStroke"].as_str() {
            Some("down") => {
                f.insert("pkd".into(), json!(0));
            }
            Some("up") => {
                f.insert("pkd".into(), json!(1));
            }
            _ => {}
        }
        out.push(f);
    }
    out
}

fn int_of(f: &Map<String, Value>, key: &str) -> i64 {
    f.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn time_of(f: &Map<String, Value>) -> f64 {
    f.get("t").and_then(Value::as_f64).unwrap_or(0.0)
}

/// Where the fretting hand sits: a four-fret box that moves only when a note falls outside it.
fn anchors_for(notes: &[RawNote], fields: &[Map<String, Value>], starts: &[f64]) -> Vec<Value> {
    let mut per_slot: Vec<(usize, Vec<(f64, i64, i64)>)> = Vec::new();
    let mut slot_index: HashMap<usize, usize> = HashMap::new();
    for (n, f) in notes.iter().zip(fields) {
        let frets: Vec<i64> = [int_of(f, "f"), int_of(f, "sl"), int_of(f, "slu")]
            .into_iter()
            .filter(|x| *x > 0)
            .collect();
        if let (Some(lo), Some(hi)) = (frets.iter().min(), frets.iter().max()) {
            let at = *slot_index.entry(n.slot).or_insert_with(|| {
                per_slot.push((n.slot, Vec::new()));
                per_slot.len() - 1
            });
            per_slot[at].1.push((time_of(f), *lo, *hi));
        }
    }
    per_slot.sort_by_key(|(slot, _)| *slot);
    let mut anchors: Vec<(f64, i64, i64)> = Vec::new();
    let mut hand: Option<(i64, i64)> = None; // (fret, width)
    let inside = |hand: Option<(i64, i64)>, lo: i64, hi: i64| hand.is_some_and(|(fret, width)| fret <= lo && hi <= fret + width - 1);
    for (slot, events) in &per_slot {
        let lo = events.iter().map(|e| e.1).min().unwrap_or(1);
        let hi = events.iter().map(|e| e.2).max().unwrap_or(1);
        if inside(hand, lo, hi) {
            continue;
        }
        if hi - lo <= 3 {
            hand = Some((lo, 4));
            let t = if anchors.is_empty() { 0.0 } else { starts[*slot] };
            anchors.push((round3(t), lo, 4));
            continue;
        }
        for &(t, elo, ehi) in events {
            // the measure spans more than one box: move note by note
            if inside(hand, elo, ehi) {
                continue;
            }
            let width = (ehi - elo + 1).max(4);
            hand = Some((elo, width));
            anchors.push((round3(if anchors.is_empty() { 0.0 } else { t }), elo, width));
        }
    }
    if anchors.is_empty() {
        anchors.push((0.0, 1, 4));
    }
    // two moves at one instant: the last one stands
    let mut latest: Vec<(f64, i64, i64)> = Vec::new();
    for a in anchors {
        match latest.iter_mut().find(|x| x.0 == a.0) {
            Some(same) => *same = a,
            None => latest.push(a),
        }
    }
    latest.sort_by(|a, b| a.0.total_cmp(&b.0));
    latest
        .into_iter()
        .map(|(time, fret, width)| json!({"time": time, "fret": fret, "width": width}))
        .collect()
}

/// Semitones from standard tuning, lowest string first (Songsterr lists the highest first).
pub fn tuning_offsets(track: &Value) -> Vec<i64> {
    let base = if is_bass_program(track["instrumentId"].as_i64().unwrap_or(-1)) { BASS_OPEN } else { GUITAR_OPEN };
    let empty = Vec::new();
    track["tuning"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .rev()
        .enumerate()
        .map(|(i, m)| m.as_i64().unwrap_or(0) - base[i.min(5)])
        .collect()
}

/// The chart of one track and how many notes and chords it holds.
pub fn build_chart(track: &Value, starts: &[f64], name: &str) -> (Value, usize) {
    let strings = track["strings"].as_i64().unwrap_or(6).max(1) as usize;
    let capo = track["capo"].as_i64().unwrap_or(0);
    let order = unroll(track);
    let notes = merge_ties(raw_notes(track, &order, &walk(track, &order), starts, capo));
    let fields = chart_notes(&notes);

    let (mut singles, mut chords, mut templates, mut shapes) = (Vec::new(), Vec::new(), Vec::<Value>::new(), Vec::<Value>::new());
    let mut template_ids: HashMap<(Vec<i64>, String), usize> = HashMap::new();
    let mut prev_chord: Option<(usize, f64)> = None; // (template, end time) when the event before was a chord
    let mut i = 0;
    while i < fields.len() {
        let t = time_of(&fields[i]);
        let mut j = i + 1;
        while j < fields.len() && time_of(&fields[j]) == t {
            j += 1;
        }
        let group = i..j;
        i = j;
        let mut used: Vec<i64> = fields[group.clone()].iter().map(|f| int_of(f, "s")).collect();
        used.sort_unstable();
        used.dedup();
        if used.len() < 2 {
            singles.push(Value::Object(fields[group.start].clone()));
            prev_chord = None;
            continue;
        }
        let mut frets = vec![-1i64; strings];
        for f in &fields[group.clone()] {
            if let Some(slot) = frets.get_mut(int_of(f, "s") as usize) {
                *slot = int_of(f, "f");
            }
        }
        let label = notes[group.start].beat["chord"]["text"].as_str().unwrap_or("").trim().to_string();
        let next_id = templates.len();
        let tid = *template_ids.entry((frets.clone(), label.clone())).or_insert_with(|| {
            templates.push(json!({"name": label, "displayName": label, "frets": frets, "fingers": vec![-1; strings]}));
            next_id
        });
        let end = t + notes[group.clone()].iter().map(|n| n.dur).fold(0.0, f64::max);
        let repeat = prev_chord.is_some_and(|(id, prev_end)| id == tid && t - prev_end < 0.05);
        let chord_notes: Vec<Value> = fields[group]
            .iter()
            .map(|f| Value::Object(f.iter().filter(|(k, _)| *k != "t").map(|(k, v)| (k.clone(), v.clone())).collect()))
            .collect();
        let mut chord = Map::new();
        chord.insert("t".into(), json!(t));
        chord.insert("id".into(), json!(tid));
        chord.insert("notes".into(), Value::Array(chord_notes));
        if repeat {
            chord.insert("hd".into(), json!(true));
            if let Some(shape) = shapes.last_mut() {
                shape["end_time"] = json!(round3(end));
            }
        } else {
            shapes.push(json!({"chord_id": tid, "start_time": t, "end_time": round3(end)}));
        }
        chords.push(Value::Object(chord));
        prev_chord = Some((tid, end));
    }

    let count = singles.len() + chords.len();
    let chart = json!({
        "name": name,
        "tuning": tuning_offsets(track),
        "capo": capo,
        "notes": singles,
        "chords": chords,
        "anchors": anchors_for(&notes, &fields, starts),
        "handshapes": shapes,
        "templates": templates,
    });
    (chart, count)
}

/// Sections from Songsterr's measure markers; when it has none, from the few text notes on beats.
pub fn section_marks(tracks: &[&Value], order: &[usize], starts: &[f64]) -> Vec<Value> {
    let mut marks: Vec<Option<String>> = vec![None; order.len()];
    let text_of = |v: &Value| v["text"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    for track in tracks {
        let ms = measures(track);
        for (slot, &idx) in order.iter().enumerate() {
            if marks[slot].is_none()
                && let Some(m) = ms.get(idx)
            {
                marks[slot] = text_of(&m["marker"]);
            }
        }
    }
    if marks.iter().all(Option::is_none) {
        for track in tracks {
            let ms = measures(track);
            for (slot, &idx) in order.iter().enumerate() {
                if marks[slot].is_some() {
                    continue;
                }
                let Some(m) = ms.get(idx) else { continue };
                marks[slot] = voices(m).iter().flat_map(beats).find_map(|b| text_of(&b["text"]));
            }
        }
        let mut distinct: Vec<&String> = marks.iter().flatten().collect();
        distinct.sort();
        distinct.dedup();
        if distinct.len() > 12 {
            marks.fill(None); // that many are lyrics or playing hints, not sections
        }
    }
    let mut seen: HashMap<String, i64> = HashMap::new();
    let mut out = Vec::new();
    for (slot, name) in marks.into_iter().enumerate() {
        if let Some(name) = name {
            let number = seen.entry(name.clone()).or_insert(0);
            *number += 1;
            out.push(json!({"name": name, "number": *number, "time": round3(starts[slot])}));
        }
    }
    out
}

/// The song's grid: tempo and time-signature changes, every beat, the sections.
pub fn timeline(slots: &[Slot], starts: &[f64], sections: Vec<Value>) -> Value {
    let (mut beats, mut tempos, mut signatures): (Vec<Value>, Vec<(f64, f64)>, Vec<(f64, [i64; 2])>) = (Vec::new(), Vec::new(), Vec::new());
    for (slot, m) in slots.iter().enumerate() {
        let (t0, t1) = (starts[slot], starts[slot + 1]);
        // a pickup measure is shorter than its signature: it gets only the beats that start inside it
        let unit = 1.0 / m.sig[1].max(1) as f64;
        let count = if m.length > 0.0 { ((m.length / unit - 1e-9).ceil() as i64).clamp(1, m.sig[0].max(1)) } else { 1 };
        for k in 0..count {
            let place = if m.length > 0.0 { k as f64 * unit / m.length } else { 0.0 };
            let time = round3(t0 + (t1 - t0) * m.time_at(place));
            beats.push(json!({"time": time, "measure": if k == 0 { slot as i64 + 1 } else { -1 }}));
        }
        if tempos.last().is_none_or(|last| (last.1 - m.bpm).abs() > 0.01) {
            tempos.push((if tempos.is_empty() { 0.0 } else { round3(t0) }, round_to(m.bpm, 2)));
        }
        if signatures.last().is_none_or(|last| last.1 != m.sig) {
            signatures.push((if signatures.is_empty() { 0.0 } else { round3(t0) }, m.sig));
        }
    }
    json!({
        "version": 1,
        "tempos": tempos.iter().map(|(time, bpm)| json!({"time": time, "bpm": bpm})).collect::<Vec<_>>(),
        "time_signatures": signatures.iter().map(|(time, ts)| json!({"time": time, "ts": ts})).collect::<Vec<_>>(),
        "beats": beats,
        "sections": sections,
    })
}

/// How busy a track is in each played measure, 0 to 1: the strip that shows where an instrument plays.
pub fn density(track: &Value, order: &[usize]) -> Vec<f32> {
    let ms = measures(track);
    let counts: Vec<usize> = order
        .iter()
        .map(|&idx| {
            voices(&ms[idx])
                .iter()
                .flat_map(beats)
                .filter(|b| b["notes"].as_array().is_some_and(|ns| ns.iter().any(|n| !truthy(&n["rest"]) && !truthy(&n["tie"]))))
                .count()
        })
        .collect();
    let top = counts.iter().copied().max().unwrap_or(0).max(1) as f32;
    counts.iter().map(|c| *c as f32 / top).collect()
}

/// "Drop D", "E standard", "Eb standard", or the open strings low to high.
pub fn tuning_name(offsets: &[i64], bass: bool) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];
    let base = if bass { BASS_OPEN } else { GUITAR_OPEN };
    let note = |i: usize| NAMES[(base[i.min(5)] + offsets[i]).rem_euclid(12) as usize];
    let Some(&first) = offsets.first() else { return String::new() };
    if offsets.iter().all(|o| *o == first) {
        return format!("{} standard", note(0));
    }
    if offsets[1..].iter().all(|o| *o == offsets[1]) && first == offsets[1] - 2 {
        return format!("Drop {}", note(0));
    }
    (0..offsets.len()).map(note).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beat(den: i64, string: i64, fret: i64) -> Value {
        json!({"duration": [1, den], "notes": [{"string": string, "fret": fret}]})
    }

    fn bass(measures: Value) -> Value {
        json!({"strings": 4, "tuning": [43, 38, 33, 26], "instrumentId": 33, "measures": measures})
    }

    /// A measure of `beats` with more fields of its own.
    fn bar(beats: Value, extra: Value) -> Value {
        let mut measure = json!({"voices": [{"beats": beats}]});
        measure.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        measure
    }

    fn quarters() -> Value {
        json!([beat(4, 3, 5), beat(4, 3, 5), beat(4, 3, 5), beat(4, 3, 5)])
    }

    fn eighths() -> Value {
        Value::Array(vec![beat(8, 3, 5); 8])
    }

    fn grace(kind: &str) -> Value {
        json!({"duration": [1, 32], "graceNote": kind, "notes": [{"string": 3, "fret": 5}]})
    }

    /// A bass tab in 4/4 with this tempo list.
    fn tab(measures: Vec<Value>, tempo: Value) -> Value {
        let mut track = bass(Value::Array(measures));
        track["measures"][0]["signature"] = json!([4, 4]);
        track["automations"] = json!({"tempo": tempo});
        track
    }

    fn at_120() -> Value {
        json!([{"measure": 0, "position": 0, "bpm": 120, "type": 4}])
    }

    /// The play order, the bar lines and the note times of a tab at its own tempo: what a pack with
    /// Songsterr's sound gets.
    fn played(track: &Value) -> (Vec<usize>, Vec<f64>, Vec<f64>) {
        let order = unroll(track);
        let nominal: Vec<f64> = walk(track, &order).iter().map(|s| s.seconds).collect();
        let (starts, _) = slot_times(&nominal, &[]);
        let (chart, _) = build_chart(track, &starts, "Bass");
        let mut times: Vec<f64> = ["notes", "chords"].iter().flat_map(|k| chart[*k].as_array().unwrap()).map(|n| n["t"].as_f64().unwrap()).collect();
        times.sort_by(f64::total_cmp);
        (order, starts, times)
    }

    /// The times in the tests below are the ones Songsterr's own player gives the same tab, to the millisecond.
    #[track_caller]
    fn assert_times(got: &[f64], want: &[f64]) {
        assert!(got.len() == want.len() && got.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.0015), "got {got:?}, want {want:?}");
    }

    #[test]
    fn the_tempo_is_the_tabs_tempo_list_not_a_beats_own() {
        let mut second = quarters();
        second[0]["tempo"] = json!({"bpm": 60, "type": 4});
        let track = tab(vec![bar(quarters(), json!({})), bar(second, json!({}))], json!([{"measure": 0, "position": 0, "bpm": 135, "type": 4}]));
        assert_times(&played(&track).1, &[0.0, 1.778, 3.556]);
        // a tab without a tempo list plays at 100
        let mut bare = track;
        bare.as_object_mut().unwrap().remove("automations");
        assert_times(&played(&bare).1, &[0.0, 2.4, 4.8]);
    }

    #[test]
    fn tempo_measures_count_from_the_first_entrys_measure() {
        let tempo = json!([{"measure": 1, "position": 0, "bpm": 120, "type": 4}, {"measure": 2, "position": 0, "bpm": 60, "type": 4}]);
        let track = tab(vec![bar(quarters(), json!({})); 3], tempo);
        assert_times(&played(&track).1, &[0.0, 2.0, 6.0, 10.0]);
    }

    #[test]
    fn tempo_counts_eighths_halves_and_dotted_notes_and_drops_the_fraction() {
        // 200 eighths, 40 halves and 100 dotted quarters a minute; 181 eighths a minute is 90 quarters, not 90.5
        let tempo = json!([
            {"measure": 0, "position": 0, "bpm": 200, "type": 8},
            {"measure": 1, "position": 0, "bpm": 40, "type": 2},
            {"measure": 2, "position": 0, "bpm": 100, "type": 4, "dotted": true},
            {"measure": 3, "position": 0, "bpm": 181, "type": 8},
        ]);
        let track = tab(vec![bar(quarters(), json!({})); 4], tempo);
        assert_times(&played(&track).1, &[0.0, 2.4, 5.4, 7.0, 9.667]);
    }

    #[test]
    fn a_tempo_change_inside_a_measure_moves_the_notes_after_it() {
        let tempo = json!([{"measure": 0, "position": 0, "bpm": 120, "type": 4}, {"measure": 0, "position": 1920, "bpm": 60, "type": 4}]);
        let track = tab(vec![bar(quarters(), json!({})); 2], tempo);
        assert_times(&played(&track).2, &[0.0, 0.5, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn a_gradual_tempo_change_goes_in_steps_of_a_quarter_note() {
        let tempo = json!([{"measure": 0, "position": 0, "bpm": 100, "type": 4}, {"measure": 1, "position": 0, "bpm": 140, "type": 4, "linear": true}]);
        let mut track = tab(vec![bar(quarters(), json!({})); 2], tempo);
        track["automations"]["gradualTempo"] = json!(true);
        assert_times(&played(&track).2, &[0.0, 0.6, 1.145, 1.645, 2.107, 2.536, 2.964, 3.393]);
    }

    #[test]
    fn a_fermata_slows_its_beat() {
        let mut track = tab(vec![bar(quarters(), json!({})); 2], at_120());
        track["automations"]["fermata"] = json!([{"measure": 0, "position": 2880, "length": 0.5}]);
        assert_times(&played(&track).2, &[0.0, 0.5, 1.0, 1.5, 2.382, 2.882, 3.382, 3.882]);
    }

    #[test]
    fn swing_plays_pairs_long_short_until_it_is_switched_off() {
        let mixed = json!([beat(8, 3, 5), beat(16, 3, 5), beat(16, 3, 5), beat(4, 3, 5), beat(8, 3, 5), beat(8, 3, 5), beat(4, 3, 5)]);
        let track = tab(vec![bar(eighths(), json!({"tripletFeel": "8th"})), bar(mixed, json!({})), bar(eighths(), json!({"tripletFeel": "off"}))], at_120());
        let want = [0.0, 0.333, 0.5, 0.833, 1.0, 1.333, 1.5, 1.833, 2.0, 2.25, 2.375, 2.5, 3.0, 3.333, 3.5, 4.0, 4.25, 4.5, 4.75, 5.0, 5.25, 5.5, 5.75];
        assert_times(&played(&track).2, &want);
    }

    #[test]
    fn a_pickup_measure_is_as_long_as_its_notes_only_when_the_tab_says_so() {
        let mut track = tab(vec![bar(json!([beat(4, 3, 5)]), json!({})), bar(quarters(), json!({}))], at_120());
        assert_times(&played(&track).1, &[0.0, 2.0, 4.0]);
        track["anacrusis"] = json!(true);
        let (_, starts, times) = played(&track);
        assert_times(&starts, &[0.0, 0.5, 2.5]);
        assert_times(&times, &[0.0, 0.5, 1.0, 1.5, 2.0]);
    }

    #[test]
    fn a_note_written_past_the_bar_line_does_not_stretch_the_measure() {
        let mut five = quarters();
        five.as_array_mut().unwrap().push(beat(4, 1, 2));
        let track = tab(vec![bar(five, json!({})), bar(quarters(), json!({}))], at_120());
        let (_, starts, times) = played(&track);
        assert_times(&starts, &[0.0, 2.0, 4.0]);
        assert_times(&times, &[0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5]);
    }

    #[test]
    fn grace_notes_take_their_time_from_the_note_they_lean_on() {
        // on the beat: the grace note sits on the beat and pushes its note later; before the beat: it ends
        // on the beat, also across a bar line
        let first = json!([beat(4, 3, 5), grace("onBeat"), beat(4, 3, 5), grace("beforeBeat"), beat(4, 3, 5), beat(4, 3, 5)]);
        let mut second = quarters();
        second.as_array_mut().unwrap().insert(0, grace("beforeBeat"));
        let track = tab(vec![bar(first, json!({})), bar(second, json!({}))], at_120());
        assert_times(&played(&track).2, &[0.0, 0.5, 0.562, 0.938, 1.0, 1.5, 1.938, 2.0, 2.5, 3.0, 3.5]);
    }

    #[test]
    fn the_tempo_follows_the_playing_through_repeats() {
        let tempo = json!([{"measure": 0, "position": 0, "bpm": 120, "type": 4}, {"measure": 1, "position": 0, "bpm": 240, "type": 4}, {"measure": 2, "position": 0, "bpm": 60, "type": 4}]);
        // a repeat sign with no opening sign goes back to the start
        let track = tab(vec![bar(quarters(), json!({})), bar(quarters(), json!({"repeat": 2})), bar(quarters(), json!({}))], tempo.clone());
        let (order, starts, _) = played(&track);
        assert_eq!(order, vec![0, 1, 0, 1, 2]);
        assert_times(&starts, &[0.0, 2.0, 3.0, 5.0, 6.0, 10.0]);
        // first and second endings
        let track = tab(
            vec![bar(quarters(), json!({"repeatStart": true})), bar(quarters(), json!({"alternateEnding": [1], "repeat": 2})), bar(quarters(), json!({"alternateEnding": [2]}))],
            tempo,
        );
        let (order, starts, _) = played(&track);
        assert_eq!(order, vec![0, 1, 0, 2]);
        assert_times(&starts, &[0.0, 2.0, 3.0, 5.0, 9.0]);
    }

    #[test]
    fn repeats_and_endings_are_written_out() {
        let m = |extra: Value| {
            let mut v = json!({"voices": []});
            v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            v
        };
        let track = json!({"measures": [
            m(json!({"repeatStart": true})),
            m(json!({"alternateEnding": [1], "repeat": 2})),
            m(json!({"alternateEnding": [2]})),
            m(json!({"repeatStart": true, "repeat": 3})),
            m(json!({})),
        ]});
        assert_eq!(unroll(&track), vec![0, 1, 0, 2, 3, 3, 3, 4]);
    }

    #[test]
    fn sync_points_place_measures_and_share_out_a_merged_gap() {
        // one point per measure
        let (starts, unsynced) = slot_times(&[2.0, 2.0, 2.0], &[1.0, 3.1, 5.0]);
        assert_eq!(unsynced, 0);
        assert_eq!(&starts[..3], &[1.0, 3.1, 5.0]);
        assert!((starts[3] - 7.0).abs() < 0.11);
        // three plays of the first measure under one point
        let (starts, unsynced) = slot_times(&[2.0; 5], &[0.0, 6.3, 8.3]);
        assert_eq!(unsynced, 0);
        assert!((starts[1] - 2.1).abs() < 1e-9 && (starts[2] - 4.2).abs() < 1e-9);
        assert_eq!((starts[3], starts[4]), (6.3, 8.3));
        // measures past the last point follow the tab's tempo
        let (starts, unsynced) = slot_times(&[2.0; 4], &[0.0, 2.0]);
        assert_eq!((unsynced, starts), (2, vec![0.0, 2.0, 4.0, 6.0, 8.0]));
        // no points: the tab's own tempo
        assert_eq!(slot_times(&[2.0, 1.0], &[]).0, vec![0.0, 2.0, 3.0]);
    }

    #[test]
    fn notes_land_on_their_strings_and_ties_lengthen() {
        let track = bass(json!([{"signature": [4, 4], "voices": [{"beats": [
            beat(4, 3, 8),
            {"duration": [1, 4], "notes": [{"string": 3, "fret": 8, "tie": true}]},
            beat(4, 1, 5),
            {"duration": [1, 4], "notes": [{"rest": true}], "rest": true},
        ]}]}]));
        let (chart, count) = build_chart(&track, &[10.0, 14.0], "Bass");
        assert_eq!(count, 2);
        assert_eq!(chart["tuning"], json!([-2, 0, 0, 0]));
        let notes = chart["notes"].as_array().unwrap();
        // Songsterr's string 3 of 4 is the lowest: feedpak string 0. Two tied quarters ring for two beats.
        assert_eq!((notes[0]["t"].as_f64(), notes[0]["s"].as_i64(), notes[0]["f"].as_i64()), (Some(10.0), Some(0), Some(8)));
        assert_eq!(notes[0]["sus"], json!(1.94));
        assert_eq!((notes[1]["t"].as_f64(), notes[1]["s"].as_i64()), (Some(12.0), Some(2)));
        assert_eq!(chart["anchors"], json!([{"time": 0.0, "fret": 5, "width": 4}]));
    }

    #[test]
    fn hammer_ons_slides_and_chords() {
        let track = json!({"strings": 6, "tuning": [64, 59, 55, 50, 45, 40], "instrumentId": 30, "measures": [
            {"signature": [4, 4], "voices": [{"beats": [
                {"duration": [1, 4], "notes": [{"string": 5, "fret": 3, "hp": true}]},
                {"duration": [1, 4], "notes": [{"string": 5, "fret": 5, "slide": "shift"}]},
                {"duration": [1, 4], "notes": [{"string": 5, "fret": 7}], "palmMute": true},
                {"duration": [1, 8], "chord": {"text": "E5"}, "notes": [{"string": 5, "fret": 0}, {"string": 4, "fret": 2}]},
                {"duration": [1, 8], "chord": {"text": "E5"}, "notes": [{"string": 5, "fret": 0}, {"string": 4, "fret": 2}]},
            ]}]},
        ]});
        let (chart, count) = build_chart(&track, &[0.0, 4.0], "Lead");
        assert_eq!(count, 5);
        let notes = chart["notes"].as_array().unwrap();
        assert_eq!(notes[1]["ho"], json!(true));
        assert_eq!((notes[1]["sl"].as_i64(), notes[1]["sus"].as_f64()), (Some(7), Some(1.0)));
        assert_eq!(notes[2]["pm"], json!(true));
        let chords = chart["chords"].as_array().unwrap();
        assert_eq!(chart["templates"][0]["frets"], json!([0, 2, -1, -1, -1, -1]));
        assert_eq!(chart["templates"][0]["name"], json!("E5"));
        assert_eq!((chords[0].get("hd"), chords[1]["hd"].as_bool()), (None, Some(true)));
        assert_eq!(chart["handshapes"], json!([{"chord_id": 0, "start_time": 3.0, "end_time": 4.0}]));
    }

    #[test]
    fn tunings_have_names() {
        assert_eq!(tuning_name(&[0, 0, 0, 0], true), "E standard");
        assert_eq!(tuning_name(&[-2, 0, 0, 0], true), "Drop D");
        assert_eq!(tuning_name(&[-1; 6], false), "Eb standard");
        assert_eq!(tuning_name(&[-2, -2, -2, -2, -2, -2], false), "D standard");
        assert_eq!(tuning_name(&[-2, 0, 0, 0, -2, -2], false), "D A D G A D");
    }

    #[test]
    fn the_grid_counts_beats_per_measure() {
        let track = bass(json!([
            {"signature": [3, 4], "voices": [{"beats": [{"duration": [3, 4], "notes": [{"rest": true}]}]}]},
        ]));
        let mut track = track;
        track["automations"] = json!({"tempo": [{"measure": 0, "position": 0, "bpm": 90, "type": 4}]});
        let order = unroll(&track);
        let slots = walk(&track, &order);
        assert_eq!((slots[0].sig, slots[0].seconds, slots[0].bpm), ([3, 4], 2.0, 90.0));
        let grid = timeline(&slots, &[0.0, 3.0], Vec::new());
        assert_eq!(grid["beats"], json!([{"time": 0.0, "measure": 1}, {"time": 1.0, "measure": -1}, {"time": 2.0, "measure": -1}]));
        assert_eq!(grid["tempos"], json!([{"time": 0.0, "bpm": 90.0}]));
    }
}
