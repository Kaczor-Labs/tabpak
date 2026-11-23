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

    fn over(self, o: Frac) -> f64 {
        let r = Frac::new(self.n * o.d, self.d * o.n);
        r.n as f64 / r.d as f64
    }

    fn float(self) -> f64 {
        self.n as f64 / self.d as f64
    }

    fn less(self, o: Frac) -> bool {
        self.n * o.d < o.n * self.d
    }

    fn is_zero(self) -> bool {
        self.n == 0
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

/// A voice's length without its grace notes, which take no time.
fn voice_length(voice: &Value) -> Frac {
    beats(voice)
        .iter()
        .filter(|b| !truthy(&b["graceNote"]))
        .fold(Frac::ZERO, |sum, b| sum.plus(Frac::of(&b["duration"])))
}

/// Measure indexes in the order they are played: repeats and numbered endings written out.
pub fn unroll(track: &Value) -> Vec<usize> {
    let ms = measures(track);
    let (mut order, mut i, mut start, mut lap) = (Vec::new(), 0usize, 0usize, 1i64);
    while i < ms.len() && order.len() < 20000 {
        let m = &ms[i];
        if truthy(&m["repeatStart"]) && i != start {
            start = i;
            lap = 1;
        }
        let endings: Vec<i64> = match &m["alternateEnding"] {
            Value::Number(n) => n.as_i64().into_iter().collect(),
            Value::Array(a) => a.iter().filter_map(Value::as_i64).collect(),
            _ => Vec::new(),
        };
        if !endings.is_empty() && !endings.contains(&lap) {
            i += 1;
            continue;
        }
        order.push(i);
        let times = m["repeat"].as_i64().unwrap_or(0);
        if times > 1 {
            if lap < times {
                lap += 1;
                i = start;
                continue;
            }
            start = i + 1;
            lap = 1;
        }
        i += 1;
    }
    order
}

/// One played measure: its time signature, its length in whole notes, how long it lasts at the tab's own tempo
/// and the quarter-note bpm at its start.
#[derive(Clone, Debug)]
pub struct Slot {
    pub sig: [i64; 2],
    pub length: Frac,
    pub seconds: f64,
    pub bpm: f64,
}

fn measure_length(measure: &Value, signature: Frac) -> Frac {
    let longest = voices(measure)
        .iter()
        .map(voice_length)
        .fold(Frac::ZERO, |a, b| if a.less(b) { b } else { a });
    if longest.is_zero() { signature } else { longest }
}

pub fn walk(track: &Value, order: &[usize]) -> Vec<Slot> {
    let ms = measures(track);
    let (mut out, mut sig, mut sig_pair, mut bpm) = (Vec::new(), Frac::ONE, [4i64, 4i64], 120.0f64);
    for &idx in order {
        let m = &ms[idx];
        if truthy(&m["signature"]) {
            sig_pair = [m["signature"][0].as_i64().unwrap_or(4), m["signature"][1].as_i64().unwrap_or(4)];
            sig = Frac::new(sig_pair[0], sig_pair[1]);
        }
        let length = measure_length(m, sig);
        let mut main: Option<&Value> = None;
        for v in voices(m) {
            if main.is_none_or(|best| voice_length(best).less(voice_length(v))) {
                main = Some(v);
            }
        }
        let (mut seconds, mut counted, mut start_bpm) = (0.0f64, Frac::ZERO, None);
        for b in main.map(beats).unwrap_or(&[]) {
            if let Some(t) = b["tempo"]["bpm"].as_f64().filter(|x| *x > 0.0) {
                let unit = b["tempo"]["type"].as_f64().filter(|x| *x > 0.0).unwrap_or(4.0);
                bpm = t * 4.0 / unit;
            }
            if start_bpm.is_none() {
                start_bpm = Some(bpm);
            }
            if truthy(&b["graceNote"]) {
                continue;
            }
            let d = Frac::of(&b["duration"]);
            seconds += d.float() * 4.0 * 60.0 / bpm;
            counted = counted.plus(d);
        }
        if counted.less(length) {
            seconds += length.minus(counted).float() * 4.0 * 60.0 / bpm;
        }
        out.push(Slot { sig: sig_pair, length, seconds, bpm: start_bpm.unwrap_or(bpm) });
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
fn raw_notes<'a>(track: &'a Value, order: &[usize], starts: &[f64], capo: i64) -> Vec<RawNote<'a>> {
    let ms = measures(track);
    let strings = track["strings"].as_i64().unwrap_or(6);
    let (mut notes, mut sig) = (Vec::new(), Frac::ONE);
    for (slot, &idx) in order.iter().enumerate() {
        let m = &ms[idx];
        if truthy(&m["signature"]) {
            sig = Frac::of(&m["signature"]);
        }
        let length = measure_length(m, sig);
        let (t0, t1) = (starts[slot], starts[slot + 1]);
        for voice in voices(m) {
            let (mut pos, mut graces): (Frac, Vec<&Value>) = (Frac::ZERO, Vec::new());
            for beat in beats(voice) {
                if truthy(&beat["graceNote"]) {
                    graces.push(beat);
                    continue;
                }
                let d = Frac::of(&beat["duration"]);
                let t = t0 + (t1 - t0) * pos.over(length);
                let dur = (t1 - t0) * d.over(length);
                let lead = f64::min(0.06, dur * 0.25); // grace notes sit just before the beat they decorate
                let count = graces.len();
                let mut todo: Vec<(&Value, f64, f64, Frac)> = graces
                    .drain(..)
                    .enumerate()
                    .map(|(k, g)| (g, t - lead * (count - k) as f64, lead, Frac::new(1, 64)))
                    .collect();
                todo.push((beat, t, dur, d));
                for (b, bt, bdur, blen) in todo {
                    for n in b["notes"].as_array().map(Vec::as_slice).unwrap_or(&[]) {
                        let (Some(s), Some(f)) = (n["string"].as_i64(), n["fret"].as_i64()) else {
                            continue;
                        };
                        if truthy(&n["rest"]) || !n["string"].is_i64() && !n["string"].is_u64() || !(0..strings).contains(&s) {
                            continue;
                        }
                        notes.push(RawNote {
                            t: bt,
                            dur: bdur,
                            len: blen,
                            s: strings - 1 - s,
                            f: (f + capo).min(MAX_FRET + capo),
                            slot,
                            beat: b,
                            note: n,
                            bends: if truthy(&n["bend"]) { vec![(bt, bdur, &n["bend"])] } else { Vec::new() },
                            tied: false,
                            carry: Carry::default(),
                        });
                    }
                }
                pos = pos.plus(d);
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
    let notes = merge_ties(raw_notes(track, &order, starts, capo));
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
        let count = m.sig[0].max(1);
        for k in 0..count {
            let time = round3(t0 + (t1 - t0) * k as f64 / count as f64);
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
            {"signature": [3, 4], "voices": [{"beats": [{"duration": [3, 4], "notes": [{"rest": true}], "tempo": {"type": 4, "bpm": 90}}]}]},
        ]));
        let order = unroll(&track);
        let slots = walk(&track, &order);
        assert_eq!((slots[0].sig, slots[0].seconds, slots[0].bpm), ([3, 4], 2.0, 90.0));
        let grid = timeline(&slots, &[0.0, 3.0], Vec::new());
        assert_eq!(grid["beats"], json!([{"time": 0.0, "measure": 1}, {"time": 1.0, "measure": -1}, {"time": 2.0, "measure": -1}]));
        assert_eq!(grid["tempos"], json!([{"time": 0.0, "bpm": 90.0}]));
    }
}
