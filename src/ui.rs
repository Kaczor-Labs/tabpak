//! The window, drawn with quickRS's own look (qrs-ui: its theme, cards, keys and type) so the two sit side by
//! side as one program: a sidebar with the songs already made, a field for the link, the song's card.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

use eframe::egui::{self, Align2, Color32, Pos2, Rect, Sense, pos2, vec2};
use qrs_ui::icons::{self, Icon};
use qrs_ui::paint::{self, Fill};
use qrs_ui::text::{self, Style};
use qrs_ui::theme::{self, ThemeName, t};
use qrs_ui::widgets::{self, ButtonStyle, Segmented};
use serde_json::{Value, json};

use crate::convert::{self, Done, Kind, Request, Song, Source, Step};
use crate::feedpak::{self, Saved};
use crate::songsterr::{self, SearchHit};

pub const SIZE: [f32; 2] = [1180.0, 760.0];
pub const MIN_SIZE: [f32; 2] = [1040.0, 700.0];
const PAD: f32 = 28.0;
const SIDE: f32 = 270.0;
const SOUNDS: [&str; 3] = ["Recording", "Songsterr sound", "My file"];

/// What the window remembers between runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub songs_dir: PathBuf,
    /// Index into the sound choices.
    pub sound: usize,
}

impl Default for Prefs {
    fn default() -> Prefs {
        Prefs { songs_dir: convert::default_songs_dir(), sound: 0 }
    }
}

fn prefs_file() -> Option<PathBuf> {
    Some(fastframe_dirs::Dirs::platform("", "", "Tabpak")?.config.join("prefs.json"))
}

impl Prefs {
    pub fn load() -> Prefs {
        let saved: Option<Value> = prefs_file().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok());
        let mut prefs = Prefs::default();
        if let Some(saved) = saved {
            if let Some(dir) = saved["songs_dir"].as_str().filter(|d| !d.is_empty()) {
                prefs.songs_dir = PathBuf::from(dir);
            }
            prefs.sound = (saved["sound"].as_u64().unwrap_or(0) as usize).min(SOUNDS.len() - 1);
        }
        prefs
    }

    fn save(&self) {
        if let Some(file) = prefs_file() {
            let text = json!({"songs_dir": self.songs_dir.to_string_lossy(), "sound": self.sound});
            let _ = fastframe_file::write(&file, serde_json::to_vec_pretty(&text).unwrap_or_default());
        }
    }
}

/// The theme quickRS is set to on this computer, so the two windows match.
pub fn quickrs_theme() -> ThemeName {
    convert::quickrs_config()
        .and_then(|f| std::fs::read(f).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|c| c["theme"].as_str().map(ThemeName::parse))
        .unwrap_or(ThemeName::Midnight)
}

enum Msg {
    Hits(u64, Result<Vec<SearchHit>, String>),
    Song(u64, Result<(Song, Option<Vec<u8>>), String>),
    Step(Step, f32),
    Done(Box<Result<Done, String>>),
    Saved(Vec<Saved>),
}

struct Loaded {
    song: Song,
    picked: Vec<bool>,
    cover_jpeg: Option<Vec<u8>>,
    cover: Option<egui::TextureHandle>,
}

enum Stage {
    Empty,
    Searching,
    Hits(Vec<SearchHit>),
    Loading,
    Song(Box<Loaded>),
    Failed(String),
}

enum Job {
    Idle,
    Running(Step, f32),
    Done(Box<Done>),
    Failed(String),
}

pub struct App {
    prefs: Prefs,
    query: String,
    /// The text the stage below the field answers.
    asked: String,
    /// When the field last changed, for searching once the typing pauses.
    typed_at: f64,
    stage: Stage,
    job: Job,
    file: Option<PathBuf>,
    offset_ms: String,
    /// How far a long parts list is scrolled.
    parts_scroll: f32,
    /// Whether yt-dlp is there to fetch recordings.
    has_ytdlp: bool,
    logo: Option<egui::TextureHandle>,
    saved: Vec<Saved>,
    covers: HashMap<PathBuf, egui::TextureHandle>,
    quickrs_knows: Option<bool>,
    /// Only the newest search or load may answer.
    ticket: u64,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
}

fn clock(seconds: f64) -> String {
    format!("{}:{:02}", seconds as u64 / 60, seconds as u64 % 60)
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn home_short(path: &Path) -> String {
    match path.strip_prefix(convert::home()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn open_path(path: &Path, reveal: bool) {
    let mut cmd = if cfg!(windows) {
        std::process::Command::new("explorer")
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        if reveal {
            c.arg("-R");
        }
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    let target = if reveal && !cfg!(target_os = "macos") { path.parent().unwrap_or(path) } else { path };
    let _ = cmd.arg(target).spawn();
}

fn open_quickrs() {
    let _ = if cfg!(target_os = "macos") {
        std::process::Command::new("open").args(["-a", "quickRS"]).spawn()
    } else {
        std::process::Command::new("quickrs").spawn()
    };
}

fn texture(ctx: &egui::Context, name: &str, jpeg: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(jpeg).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
    Some(ctx.load_texture(name, color, egui::TextureOptions::LINEAR))
}

/// A section's label, in quickRS's small capitals.
fn caption(p: &egui::Painter, s: &str, at: Pos2) {
    text::caps(p, &s.to_uppercase(), at, 11.0, t().caption, Align2::LEFT_TOP, None);
}

fn side_caption(p: &egui::Painter, s: &str, at: Pos2) {
    text::caps(p, &s.to_uppercase(), at, 10.0, t().caption, Align2::LEFT_TOP, None);
}

/// `s` on one line of `max_w`: what doesn't fit is cut from the middle, so names that differ at the end stay apart.
fn line_mid(p: &egui::Painter, s: &str, at: Pos2, size: f32, weight: u16, color: Color32, max_w: f32) -> Rect {
    let st = Style::new(size, weight, color);
    let chars: Vec<char> = s.chars().collect();
    let mut shown = s.to_string();
    let mut cut = 0;
    while text::width(p.ctx(), &shown, st) > max_w && cut + 4 < chars.len() {
        cut += 1;
        let keep = chars.len() - cut;
        let head = keep * 3 / 5;
        shown = format!("{}\u{2026}{}", chars[..head].iter().collect::<String>().trim_end(), chars[chars.len() - (keep - head)..].iter().collect::<String>().trim_start());
    }
    text::draw(p, &shown, at, Align2::LEFT_TOP, st, None)
}

/// Songsterr's part names divide player and gear with a bar, which reads as a letter here.
fn part_label(name: &str) -> String {
    name.replace(" | ", " / ")
}

fn line(p: &egui::Painter, s: &str, at: Pos2, size: f32, weight: u16, color: Color32, max_w: f32) -> Rect {
    let st = Style::new(size, weight, color);
    text::draw(p, &text::fit(p.ctx(), s, st, max_w), at, Align2::LEFT_TOP, st, None)
}

/// Lines of `s` wrapped to `max_w`, drawn from `at`; returns the y under them.
fn para(p: &egui::Painter, s: &str, at: Pos2, size: f32, color: Color32, max_w: f32) -> f32 {
    let st = Style::new(size, 500, color);
    let mut y = at.y;
    for l in text::wrap(p.ctx(), s, st, max_w) {
        text::draw(p, &l, pos2(at.x, y), Align2::LEFT_TOP, st, None);
        y += text::line_height(p.ctx(), st) + 3.0;
    }
    y
}

/// One message in the middle of the stage: a mark, what happened, and what to do about it.
fn notice(p: &egui::Painter, r: Rect, icon: Icon, tint: Color32, title: &str, about: &str) {
    let th = t();
    let c = pos2(r.center().x, r.min.y + r.height() * 0.36);
    let disc = Rect::from_center_size(c, vec2(72.0, 72.0));
    paint::inset(p, disc, Some(36.0), 4.0, Fill::Base);
    icons::icon(p, icon, c, 28.0, tint);
    let w = r.width().min(520.0);
    let st = Style::new(18.0, 800, th.text);
    let mut y = c.y + 58.0;
    for l in text::wrap(p.ctx(), title, st, w) {
        text::draw(p, &l, pos2(c.x, y), Align2::CENTER_TOP, st, None);
        y += text::line_height(p.ctx(), st) + 3.0;
    }
    let st = Style::new(13.0, 500, th.caption);
    y += 6.0;
    for l in text::wrap(p.ctx(), about, st, w) {
        text::draw(p, &l, pos2(c.x, y), Align2::CENTER_TOP, st, None);
        y += text::line_height(p.ctx(), st) + 3.0;
    }
}

/// A song's measures as a strip of bars: tall where the part is busy, flat where it rests.
fn density_strip(p: &egui::Painter, rect: Rect, density: &[f32], lit: bool) {
    let th = t();
    let bars = ((rect.width() / 3.0) as usize).max(1);
    let color = if lit { th.accent } else { th.text_3 };
    for b in 0..bars {
        let (from, to) = (b * density.len() / bars, ((b + 1) * density.len() / bars).max(b * density.len() / bars + 1));
        let level = density.get(from..to.min(density.len())).unwrap_or(&[]).iter().copied().fold(0.0f32, f32::max);
        let x = rect.min.x + b as f32 * 3.0;
        if level <= 0.0 {
            p.rect_filled(Rect::from_min_size(pos2(x, rect.max.y - 1.0), vec2(2.0, 1.0)), 0.0, th.unlit);
        } else {
            let h = (rect.height() * (0.25 + 0.75 * level)).round();
            p.rect_filled(Rect::from_min_size(pos2(x, rect.max.y - h), vec2(2.0, h)), 1.0, color);
        }
    }
}

impl App {
    pub fn new(prefs: Prefs) -> App {
        let (tx, rx) = mpsc::channel();
        App {
            quickrs_knows: convert::quickrs_knows(&prefs.songs_dir),
            prefs,
            query: String::new(),
            asked: String::new(),
            typed_at: 0.0,
            stage: Stage::Empty,
            job: Job::Idle,
            file: None,
            offset_ms: String::new(),
            parts_scroll: 0.0,
            has_ytdlp: crate::audio::find_tool("yt-dlp").is_some(),
            logo: None,
            saved: Vec::new(),
            covers: HashMap::new(),
            ticket: 0,
            tx,
            rx,
        }
    }

    /// Read the songs folder again, off the window's thread.
    pub fn refresh_saved(&self, ctx: &egui::Context) {
        let (tx, dir, ctx) = (self.tx.clone(), self.prefs.songs_dir.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Saved(feedpak::list_saved(&dir)));
            ctx.request_repaint();
        });
    }

    /// Put a song on the stage as if its link had been pasted.
    pub fn show_song(&mut self, song: Song, cover: Option<Vec<u8>>, kind: Option<Kind>) {
        let on = song.parts_of(kind);
        let picked = song.parts.iter().map(|p| on.contains(&p.index)).collect();
        if song.videos.is_empty() && self.prefs.sound == 0 && song.has_songsterr_sound() {
            self.prefs.sound = 1;
        }
        self.stage = Stage::Song(Box::new(Loaded { song, picked, cover_jpeg: cover, cover: None }));
        self.parts_scroll = 0.0;
        self.job = Job::Idle;
    }

    pub fn show_hits(&mut self, hits: Vec<SearchHit>) {
        self.stage = Stage::Hits(hits);
    }

    pub fn show_saved(&mut self, saved: Vec<Saved>) {
        self.saved = saved;
    }

    pub fn show_job_running(&mut self, step: Step, fraction: f32) {
        self.job = Job::Running(step, fraction);
    }

    pub fn show_job_done(&mut self, done: Done) {
        self.job = Job::Done(Box::new(done));
    }

    pub fn show_failure(&mut self, why: &str) {
        self.stage = Stage::Failed(why.into());
    }

    pub fn set_query(&mut self, text: &str) {
        self.query = text.into();
        self.asked = text.into();
    }

    pub fn set_sound(&mut self, sound: usize) {
        self.prefs.sound = sound.min(SOUNDS.len() - 1);
    }

    fn load(&mut self, ctx: &egui::Context, song_id: u64) {
        self.ticket += 1;
        self.stage = Stage::Loading;
        self.job = Job::Idle;
        let (tx, ticket, ctx) = (self.tx.clone(), self.ticket, ctx.clone());
        std::thread::spawn(move || {
            let found = convert::load_song(song_id).map(|song| {
                let cover = song.videos.iter().take(3).find_map(|v| crate::audio::cover(&v.id));
                (song, cover)
            });
            let _ = tx.send(Msg::Song(ticket, found));
            ctx.request_repaint();
        });
    }

    fn search(&mut self, ctx: &egui::Context, words: String) {
        self.ticket += 1;
        self.stage = Stage::Searching;
        let (tx, ticket, ctx) = (self.tx.clone(), self.ticket, ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Hits(ticket, songsterr::search(&words)));
            ctx.request_repaint();
        });
    }

    /// Act on what the field holds: a link loads its song, words search.
    fn ask(&mut self, ctx: &egui::Context) {
        let text = self.query.trim().to_string();
        self.asked = text.clone();
        if text.is_empty() {
            self.ticket += 1;
            self.stage = Stage::Empty;
        } else if let Some(id) = songsterr::song_id_of(&text) {
            self.load(ctx, id);
        } else if text.chars().count() >= 3 {
            self.search(ctx, text);
        }
    }

    fn source(&self) -> Option<Source> {
        match self.prefs.sound {
            0 => Some(Source::Recording),
            1 => Some(Source::Songsterr),
            _ => self.file.clone().map(Source::File),
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        let (Stage::Song(loaded), Some(source)) = (&self.stage, self.source()) else { return };
        let parts = loaded.song.parts.iter().zip(&loaded.picked).filter(|(_, on)| **on).map(|(p, _)| p.index).collect();
        let offset = self.offset_ms.trim().replace(',', ".").parse::<f64>().unwrap_or(0.0) / 1000.0;
        let req = Request { parts, source, out_dir: self.prefs.songs_dir.clone(), offset };
        self.job = Job::Running(Step::Audio, 0.0);
        self.prefs.save();
        let (tx, song, ctx) = (self.tx.clone(), loaded.song.clone(), ctx.clone());
        std::thread::spawn(move || {
            let done = convert::convert(&song, &req, &mut |step, fraction| {
                let _ = tx.send(Msg::Step(step, fraction));
                ctx.request_repaint();
            });
            let _ = tx.send(Msg::Done(Box::new(done)));
            ctx.request_repaint();
        });
    }

    fn take_messages(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Hits(ticket, found) if ticket == self.ticket => {
                    self.stage = match found {
                        Ok(hits) => Stage::Hits(hits),
                        Err(e) => Stage::Failed(format!("Songsterr didn't answer ({e}). Check the connection and try again.")),
                    }
                }
                Msg::Song(ticket, found) if ticket == self.ticket => match found {
                    Ok((song, cover)) => {
                        let kind = match songsterr::instrument_in_link(&self.asked) {
                            Some("bass") => Some(Kind::Bass),
                            Some(_) => Some(Kind::Guitar),
                            None => None,
                        };
                        self.show_song(song, cover, kind);
                    }
                    Err(e) => self.stage = Stage::Failed(e),
                },
                Msg::Hits(..) | Msg::Song(..) => {}
                Msg::Step(step, fraction) => {
                    if let Job::Running(..) = self.job {
                        self.job = Job::Running(step, fraction);
                    }
                }
                Msg::Done(done) => {
                    self.job = match *done {
                        Ok(done) => Job::Done(Box::new(done)),
                        Err(e) => Job::Failed(e),
                    };
                    self.refresh_saved(ctx);
                }
                Msg::Saved(saved) => {
                    self.covers.retain(|path, _| saved.iter().any(|s| &s.path == path));
                    self.saved = saved;
                }
            }
        }
    }

    pub fn draw(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // once per egui context: quickRS's fonts and theme
        let installed = egui::Id::new("tabpak-look");
        if !ctx.data(|d| d.get_temp::<bool>(installed).unwrap_or(false)) {
            ctx.data_mut(|d| d.insert_temp(installed, true));
            text::install(&ctx);
            theme::set_theme(quickrs_theme());
            ctx.all_styles_mut(|s| {
                s.visuals.selection.bg_fill = t().accent.gamma_multiply(0.45);
                s.visuals.selection.stroke.color = t().text;
                s.visuals.text_cursor.stroke.color = t().accent;
            });
            // the fonts are there from the next frame on
            ui.painter().rect_filled(ctx.content_rect(), 0.0, t().base);
            ctx.request_repaint();
            return;
        }
        self.take_messages(&ctx);
        let th = t();
        let full = ctx.content_rect();
        let p = ui.painter().clone();
        p.rect_filled(full, 0.0, th.base);

        let side = Rect::from_min_size(full.min + vec2(PAD, PAD), vec2(SIDE, full.height() - 2.0 * PAD));
        let main = Rect::from_min_max(pos2(side.max.x + 32.0, full.min.y + PAD), full.max - vec2(PAD, PAD));
        self.sidebar(ui, &p, side);

        // the title and the field
        let (x0, w) = (main.min.x, main.width());
        let mut y = main.min.y + 4.0;
        line(&p, "Add a song from Songsterr", pos2(x0, y), 28.0, 800, th.text, w);
        y += 50.0;
        let paste_w = widgets::button_width(&ctx, "Paste", "Ctrl V", 14.0, false) + 20.0;
        let field = Rect::from_min_size(pos2(x0, y), vec2(w - paste_w - 14.0, 52.0));
        paint::inset(&p, field, Some(26.0), 3.0, Fill::Base);
        icons::icon(&p, Icon::Search, pos2(field.min.x + 28.0, field.center().y), 16.0, th.caption);
        let edit = egui::TextEdit::singleline(&mut self.query)
            .id(egui::Id::new("query"))
            .frame(egui::Frame::NONE)
            .font(egui::FontId::new(15.0, text::family(500)))
            .text_color(th.text)
            .hint_text(egui::RichText::new("Paste a Songsterr link, or type a song or artist").color(th.caption))
            .desired_width(field.width() - 76.0);
        let answer = ui.put(Rect::from_min_size(pos2(field.min.x + 50.0, field.center().y - 11.0), vec2(field.width() - 76.0, 22.0)), edit);
        let now = ctx.input(|i| i.time);
        if answer.changed() {
            self.typed_at = now;
        }
        let entered = answer.lost_focus() && ctx.input(|i| i.key_pressed(egui::Key::Enter));
        let paste = Rect::from_min_size(pos2(field.max.x + 14.0, y), vec2(paste_w, 52.0));
        let pasted = if ctx.memory(|m| m.focused().is_none()) {
            ctx.input(|i| i.events.iter().find_map(|e| if let egui::Event::Paste(text) = e { Some(text.clone()) } else { None }))
        } else {
            None
        };
        let clicked = widgets::button(ui, paste, "paste", "Paste", "Ctrl V", ButtonStyle { r: Some(26.0), ..ButtonStyle::default() }).clicked();
        let clip = pasted.or_else(|| if clicked { arboard::Clipboard::new().and_then(|mut c| c.get_text()).ok() } else { None });
        if let Some(clip) = clip {
            self.query = clip.trim().to_string();
            self.ask(&ctx);
        }
        let pending = self.query.trim() != self.asked;
        if pending {
            let link = songsterr::song_id_of(&self.query).is_some();
            if entered || link || now - self.typed_at > 0.45 {
                self.ask(&ctx);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(120));
            }
        }
        y += 52.0 + 22.0;

        let stage = Rect::from_min_max(pos2(x0, y), main.max);
        paint::card(&p, stage, 30.0, false);
        let inner = stage.shrink(26.0);
        match &self.stage {
            Stage::Empty => self.empty(&p, inner),
            Stage::Searching => notice(&p, inner, Icon::Search, th.accent, "Looking on Songsterr\u{2026}", ""),
            Stage::Loading => notice(&p, inner, Icon::Music, th.accent, "Reading the tab\u{2026}", "Every guitar and bass part, and the videos synced to it."),
            Stage::Failed(why) => {
                // the first sentence says what happened, the rest how to go on
                let (what, next) = why.split_once(". ").map(|(a, b)| (format!("{a}."), b)).unwrap_or((why.clone(), ""));
                notice(&p, inner, Icon::X, th.miss, &what, next);
            }
            Stage::Hits(_) => self.hits(ui, &p, inner),
            Stage::Song(_) => self.song(ui, &p, inner),
        }
        let _ = ui.allocate_rect(full, Sense::hover());
    }

    fn empty(&self, p: &egui::Painter, r: Rect) {
        let th = t();
        line(p, "Three steps to a new song in quickRS", r.min, 20.0, 800, th.text, r.width());
        let steps = [
            ("1", "Find the song", "Paste its link from songsterr.com, or type its name above and pick it from the list."),
            ("2", "Pick the sound and your parts", "The real recording from YouTube, or Songsterr's own playback with your instrument on a track you can mute."),
            ("3", "Press Convert", "The song lands in your songs folder as a feedpak. quickRS shows it the next time it looks."),
        ];
        let mut y = r.min.y + 48.0;
        for (n, title, about) in steps {
            let row = Rect::from_min_size(pos2(r.min.x, y), vec2(r.width(), 78.0));
            paint::inset(p, row, Some(16.0), 3.0, Fill::Base);
            text::draw(p, n, pos2(row.min.x + 30.0, row.center().y), Align2::CENTER_CENTER, Style::new(22.0, 800, th.accent), None);
            line(p, title, pos2(row.min.x + 60.0, row.min.y + 16.0), 15.0, 800, th.text, row.width() - 80.0);
            line(p, about, pos2(row.min.x + 60.0, row.min.y + 42.0), 12.0, 500, th.caption, row.width() - 80.0);
            y += 92.0;
        }
        if !self.has_ytdlp {
            let needs = "Real recordings need yt-dlp (in Terminal: brew install yt-dlp). Songsterr's sound works without it.";
            line(p, needs, pos2(r.min.x, y + 6.0), 12.0, 500, th.caption, r.width());
        }
    }

    fn hits(&mut self, ui: &mut egui::Ui, p: &egui::Painter, r: Rect) {
        let th = t();
        let have: Vec<u64> = self.saved.iter().filter_map(|s| s.song_id).collect();
        let Stage::Hits(hits) = &self.stage else { return };
        if hits.is_empty() {
            notice(p, r, Icon::Search, th.caption, "Nothing on Songsterr by that name", "Check the spelling, or paste the song's link instead.");
            return;
        }
        let row_h = 40.0;
        let rows = (((r.height() - 28.0) / row_h) as usize).min(hits.len());
        let found = if rows < hits.len() { format!("The first {rows} of {} songs on Songsterr", hits.len()) } else { format!("{} song{} on Songsterr", hits.len(), if hits.len() == 1 { "" } else { "s" }) };
        caption(p, &found, r.min);
        let mut chosen = None;
        for (i, hit) in hits.iter().take(rows).enumerate() {
            let row = Rect::from_min_size(pos2(r.min.x - 10.0, r.min.y + 26.0 + i as f32 * row_h), vec2(r.width() + 20.0, row_h));
            let resp = widgets::interact(ui, row, ("hit", i), Sense::click());
            if resp.hovered() {
                paint::rrect(p, row.shrink2(vec2(0.0, 2.0)), 12.0, th.base_hi);
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            } else if i > 0 {
                p.rect_filled(Rect::from_min_size(pos2(r.min.x, row.min.y), vec2(r.width(), 1.0)), 0.0, th.divider);
            }
            line(p, &hit.title, pos2(r.min.x, row.min.y + 4.0), 14.0, 700, th.text, r.width() - 270.0);
            line(p, &hit.artist, pos2(r.min.x, row.min.y + 22.0), 11.0, 500, th.caption, r.width() - 270.0);
            let mut x = r.max.x;
            for (label, has) in [("Bass", hit.has_bass), ("Guitar", hit.has_guitar)] {
                let wd = text::width(ui.ctx(), label, Style::new(11.0, 700, th.text_2)) + 18.0;
                if has {
                    let chip = Rect::from_min_size(pos2(x - wd, row.center().y - 11.0), vec2(wd, 22.0));
                    paint::inset(p, chip, Some(8.0), 2.0, Fill::Base);
                    text::draw(p, label, chip.center(), Align2::CENTER_CENTER, Style::new(11.0, 700, th.text_2), None);
                }
                x -= wd + 8.0;
            }
            if have.contains(&hit.song_id) {
                let st = Style::new(11.0, 700, th.hit);
                let wd = text::width(ui.ctx(), "In your songs", st);
                text::draw(p, "In your songs", pos2(x - 6.0, row.center().y), Align2::RIGHT_CENTER, st, None);
                icons::icon(p, Icon::Check, pos2(x - wd - 18.0, row.center().y), 12.0, th.hit);
            }
            if resp.clicked() {
                chosen = Some(hit.song_id);
            }
        }
        if let Some(id) = chosen {
            self.load(ui.ctx(), id);
        }
    }

    fn song(&mut self, ui: &mut egui::Ui, p: &egui::Painter, r: Rect) {
        let th = t();
        let ctx = ui.ctx().clone();
        let running = matches!(self.job, Job::Running(..));
        let Stage::Song(loaded) = &mut self.stage else { return };

        // the song
        let art = Rect::from_min_size(r.min, vec2(132.0, 132.0));
        if let Some(jpeg) = loaded.cover_jpeg.take() {
            loaded.cover = texture(&ctx, &format!("song-{}", loaded.song.id), &jpeg);
        }
        let song = &loaded.song;
        paint::art(p, loaded.cover.as_ref().map(|c| c.id()), art, 20.0, true);
        if loaded.cover.is_none() {
            icons::icon(p, Icon::Music, art.center(), 40.0, th.caption);
        }
        let (tx, tw) = (art.max.x + 24.0, r.max.x - art.max.x - 24.0);
        caption(p, "Songsterr tab", pos2(tx, r.min.y + 6.0));
        // a long title steps down in size before it is cut
        let size = [34.0, 28.0, 23.0].into_iter().find(|s| text::width(&ctx, &song.title, Style::new(*s, 800, th.text)) <= tw).unwrap_or(23.0);
        line(p, &song.title, pos2(tx, r.min.y + 26.0 + (34.0 - size) * 0.6), size, 800, th.text, tw);
        let (seconds, real) = song.length();
        let about = format!("{}  \u{00b7}  {}{}  \u{00b7}  {} measures  \u{00b7}  tabbed by {}", song.artist, clock(seconds), if real { "" } else { " at the tab's tempo" }, song.measures, song.transcriber);
        line(p, &about, pos2(tx, r.min.y + 76.0), 14.0, 500, th.text_2, tw);
        let synced = song.videos.iter().map(|v| v.points.len()).max().unwrap_or(0);
        let sync = match (song.videos.len(), synced) {
            (0, _) => "No recording is synced to this tab: use Songsterr's sound".to_string(),
            (_, points) if points >= song.measures => "Synced to the recording  \u{00b7}  every measure".to_string(),
            (_, points) => format!("Synced to the recording  \u{00b7}  {points} of {} measures", song.measures),
        };
        let chip_w = text::width(&ctx, &sync, Style::new(12.0, 700, th.text_2)) + 44.0;
        let chip = Rect::from_min_size(pos2(tx, r.min.y + 102.0), vec2(chip_w.min(tw), 28.0));
        paint::inset(p, chip, Some(10.0), 2.0, Fill::Base);
        paint::dot(p, pos2(chip.min.x + 16.0, chip.center().y), 4.0, if song.videos.is_empty() { th.late } else { th.hit });
        text::draw(p, &sync, pos2(chip.min.x + 30.0, chip.center().y), Align2::LEFT_CENTER, Style::new(12.0, 700, th.text_2), Some(chip.width() - 40.0));

        // the convert row sits at the bottom; the sound and the parts share what's between
        let foot = Rect::from_min_max(pos2(r.min.x, r.max.y - 50.0), r.max);
        let body = Rect::from_min_max(pos2(r.min.x, art.max.y + 26.0), pos2(r.max.x, foot.min.y - 22.0));
        let parts_w = (body.width() * 0.55).round();
        let left = Rect::from_min_max(body.min, pos2(body.max.x - parts_w - 28.0, body.max.y));
        let right = Rect::from_min_max(pos2(body.max.x - parts_w, body.min.y), body.max);

        // 1: the sound
        caption(p, "1  The sound", left.min);
        let seg = Rect::from_min_size(pos2(left.min.x, left.min.y + 24.0), vec2(left.width(), 46.0));
        let mut options = Segmented::new(&SOUNDS, Some(self.prefs.sound));
        options.size = 13.0;
        let natural = widgets::segment_widths(&ctx, &SOUNDS, 13.0, options.pad);
        // every label whole when there is room, the spare shared out; never wider than the track
        let room = seg.width() - 2.0 * options.pad - 2.0 * options.gap;
        let wanted: f32 = natural.iter().sum();
        options.widths = Some(if wanted <= room { natural.iter().map(|w| w + (room - wanted) / 3.0).collect() } else { natural.iter().map(|w| (w * room / wanted).floor()).collect() });
        let (_, clicked) = widgets::segmented(ui, seg, "sound", &options);
        if let Some(choice) = clicked.filter(|_| !running) {
            self.prefs.sound = choice;
            self.job = Job::Idle;
        }
        let about = match self.prefs.sound {
            0 if self.has_ytdlp => "The real recording, from the YouTube video Songsterr synced to the tab measure by measure.",
            0 => "The real recording, from YouTube. It needs yt-dlp: in Terminal, brew install yt-dlp.",
            1 => "Songsterr's own playback. Each part you pick gets its own track, so in quickRS you can mute it and play it yourself.",
            _ => "Your own audio file, with the notes placed by Songsterr's sync for the song's video.",
        };
        let mut y = para(p, about, pos2(left.min.x, seg.max.y + 12.0), 12.0, th.caption, left.width()) + 10.0;
        if self.prefs.sound == 2 {
            let well = Rect::from_min_size(pos2(left.min.x, y), vec2(left.width() - 110.0, 40.0));
            paint::inset(p, well, Some(12.0), 3.0, Fill::Base);
            let name = self.file.as_ref().and_then(|f| f.file_name()).map(|n| n.to_string_lossy().into_owned());
            let (label, color) = match &name {
                Some(n) => (n.as_str(), th.text),
                None => ("No file picked", th.caption),
            };
            line(p, label, pos2(well.min.x + 14.0, well.min.y + 11.0), 14.0, 500, color, well.width() - 28.0);
            let browse = Rect::from_min_size(pos2(well.max.x + 10.0, y + 2.0), vec2(100.0, 36.0));
            if widgets::button(ui, browse, "browse", "Browse\u{2026}", "", ButtonStyle::default()).clicked()
                && let Some(f) = rfd::FileDialog::new().set_title("Your audio file").add_filter("Audio", &["mp3", "m4a", "flac", "wav", "ogg", "opus"]).pick_file()
            {
                self.file = Some(f);
            }
            y += 52.0;
            line(p, "Shift the notes (ms)", pos2(left.min.x, y + 2.0), 12.0, 700, th.text_2, left.width() - 110.0);
            line(p, "Plus plays them later, minus earlier", pos2(left.min.x, y + 19.0), 11.0, 500, th.caption, left.width() - 110.0);
            let off = Rect::from_min_size(pos2(left.max.x - 100.0, y), vec2(100.0, 34.0));
            paint::inset(p, off, Some(10.0), 3.0, Fill::Base);
            let edit = egui::TextEdit::singleline(&mut self.offset_ms)
                .id(egui::Id::new("offset"))
                .frame(egui::Frame::NONE)
                .font(egui::FontId::new(14.0, text::family(500)))
                .text_color(th.text)
                .hint_text(egui::RichText::new("0").color(th.caption))
                .desired_width(off.width() - 24.0);
            ui.put(Rect::from_min_size(pos2(off.min.x + 12.0, off.min.y + 8.0), vec2(off.width() - 24.0, 20.0)), edit);
        }

        // 2: the parts
        let Stage::Song(loaded) = &mut self.stage else { return };
        let song = &loaded.song;
        let count = loaded.picked.iter().filter(|on| **on).count();

        // what quickRS will get: the tracks its mixer shows
        let mut tracks: Vec<(String, &str)> = Vec::new();
        match self.prefs.sound {
            1 => {
                for (part, _) in song.parts.iter().zip(&loaded.picked).filter(|(_, on)| **on) {
                    tracks.push((part_label(&part.name), "its own track"));
                }
                tracks.push(("Rest of the band".into(), "one track"));
                tracks.push(("Full mix".into(), "for players without a mixer"));
            }
            0 => tracks.push(("The recording".into(), if song.videos.is_empty() { "no video to take it from" } else { "one track, from YouTube" })),
            _ => tracks.push(("Your file".into(), "one track")),
        }
        let room = left.max.y - (y + if self.prefs.sound == 2 { 46.0 } else { 0.0 }) - 8.0;
        if self.prefs.sound != 2 && room >= 70.0 {
            let rows = (((room - 44.0) / 26.0) as usize).min(tracks.len());
            let well = Rect::from_min_size(pos2(left.min.x, y + 4.0), vec2(left.width(), 40.0 + rows as f32 * 26.0));
            paint::inset(p, well, Some(16.0), 3.0, Fill::Base);
            caption(p, "What quickRS's mixer gets", pos2(well.min.x + 18.0, well.min.y + 14.0));
            for (i, (name, about)) in tracks.iter().take(rows).enumerate() {
                let ry = well.min.y + 38.0 + i as f32 * 26.0;
                let (name, about) = if i + 1 == rows && tracks.len() > rows { (format!("and {} more", tracks.len() - rows + 1), "") } else { (name.clone(), *about) };
                paint::dot(p, pos2(well.min.x + 24.0, ry + 9.0), 3.0, th.accent);
                let drawn = line_mid(p, &name, pos2(well.min.x + 38.0, ry), 13.0, 700, th.text, well.width() * 0.55);
                line(p, about, pos2(drawn.max.x + 12.0, ry + 1.0), 12.0, 500, th.caption, well.max.x - drawn.max.x - 28.0);
            }
        }
        caption(p, "2  Your parts", right.min);
        let total = format!("bars: where it plays, taller is busier  \u{00b7}  {count} of {} picked", song.parts.len());
        text::draw(p, &total, pos2(right.max.x, right.min.y), Align2::RIGHT_TOP, Style::new(12.0, 500, th.caption), None);
        let panel = Rect::from_min_max(pos2(right.min.x, right.min.y + 24.0), right.max);
        paint::display(p, panel, Some(18.0), 4.0);
        let view = panel.shrink2(vec2(0.0, 8.0));
        let row_h = (view.height() / song.parts.len() as f32).clamp(36.0, 50.0);
        let over = (row_h * song.parts.len() as f32 - view.height()).max(0.0);
        if over > 0.0 && ui.rect_contains_pointer(panel) {
            self.parts_scroll -= ctx.input(|i| i.smooth_scroll_delta.y);
        }
        self.parts_scroll = self.parts_scroll.clamp(0.0, over);
        if over > 0.0 {
            let rail = Rect::from_min_max(pos2(panel.max.x - 7.0, view.min.y + 4.0), pos2(panel.max.x - 4.0, view.max.y - 4.0));
            let len = rail.height() * view.height() / (view.height() + over);
            let top = rail.min.y + (rail.height() - len) * self.parts_scroll / over;
            paint::pill(p, Rect::from_min_size(pos2(rail.min.x, top), vec2(rail.width(), len)), th.divider);
        }
        let full = p;
        let p = &full.with_clip_rect(view);
        let switch = widgets::switch_size("sm");
        for (i, part) in song.parts.iter().enumerate() {
            let row = Rect::from_min_size(pos2(panel.min.x + 8.0, view.min.y + i as f32 * row_h - self.parts_scroll), vec2(panel.width() - 16.0, row_h));
            if !row.intersects(view) {
                continue;
            }
            let on = loaded.picked[i];
            let resp = widgets::interact(ui, row.intersect(view), ("part", i), Sense::click());
            if resp.hovered() && !running {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if i > 0 {
                p.rect_filled(Rect::from_min_size(pos2(row.min.x + 8.0, row.min.y), vec2(row.width() - 16.0, 1.0)), 0.0, th.divider);
            }
            let sw = Rect::from_min_size(pos2(row.min.x + 10.0, row.center().y - switch.y / 2.0), switch);
            widgets::draw_switch(p, sw, egui::Id::new(("part-switch", i)), on);
            let strip_w = (row.width() * 0.28).round();
            let name_x = sw.max.x + 14.0;
            let name_w = row.max.x - strip_w - 24.0 - name_x;
            let facts = format!("{}  \u{00b7}  {}  \u{00b7}  {} notes", if part.kind == Kind::Bass { "Bass" } else { "Guitar" }, part.tuning_name, thousands(part.notes));
            let ink = if on { th.text } else { th.text_2 };
            line_mid(p, &part_label(&part.name), pos2(name_x, row.center().y - 17.0), 14.0, 700, ink, name_w);
            line(p, &facts, pos2(name_x, row.center().y + 2.0), 11.0, 500, th.caption, name_w);
            let strip = Rect::from_min_size(pos2(row.max.x - strip_w - 10.0, row.center().y - 9.0), vec2(strip_w, 18.0));
            density_strip(p, strip, &part.density, on);
            if resp.clicked() && !running {
                loaded.picked[i] = !on;
                self.job = Job::Idle;
            }
        }
        let p = full;

        // 3: convert, and how it's going
        let ready = match (count, self.prefs.sound, &self.file) {
            (0, ..) => Err("Switch on at least one part"),
            (_, 0, _) if !self.has_ytdlp => Err("Recordings need yt-dlp: install it, or pick Songsterr sound"),
            (_, 1, _) if !song.has_songsterr_sound() => Err("Songsterr has no sound for this tab: pick another sound"),
            (_, 2, None) => Err("Browse for your audio file first"),
            _ => Ok(()),
        };
        let go = Rect::from_min_size(foot.min, vec2(200.0, 46.0));
        let done = matches!(self.job, Job::Done(_));
        // once the song is made, quickRS is the next step: the glass key moves there
        let st = ButtonStyle { enabled: ready.is_ok() && !running, ..if done { ButtonStyle::default().size(15.0) } else { ButtonStyle::primary().size(15.0) } };
        let label = if running { "Converting\u{2026}" } else if done { "Convert again" } else { "Convert" };
        let pressed = widgets::button(ui, go, "go", label, "Ctrl Enter", st).clicked() || ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
        let (status, fraction, bad): (String, f32, bool) = match &self.job {
            Job::Idle => (ready.map(|()| "Ready".to_string()).unwrap_or_else(str::to_string), 0.0, false),
            Job::Running(Step::Audio, f) => (format!("Getting the sound  \u{00b7}  {:.0}%", f.max(0.0) * 100.0), 0.05 + 0.6 * f.max(0.0), false),
            Job::Running(Step::Sync, _) => ("Placing the notes on the sound".into(), 0.68, false),
            Job::Running(Step::Pack, f) => ("Writing the song".into(), 0.7 + 0.3 * f.max(0.0), false),
            Job::Done(_) => ("In your songs folder".into(), 1.0, false),
            Job::Failed(why) => (why.clone(), 0.0, true),
        };
        let done_path = if let Job::Done(done) = &self.job { Some(done.path.clone()) } else { None };
        let keys_w = if done_path.is_some() { 300.0 } else { 0.0 };
        let sx = go.max.x + 22.0;
        let sw = foot.max.x - sx - keys_w;
        if bad {
            para(p, &status, pos2(sx, foot.min.y), 13.0, th.miss, sw);
        } else if let Job::Done(done) = &self.job {
            let notes: usize = done.parts.iter().map(|(_, n)| n).sum();
            let facts = format!("{} part{}  \u{00b7}  {} notes", done.parts.len(), if done.parts.len() == 1 { "" } else { "s" }, thousands(notes));
            paint::dot(p, pos2(sx + 5.0, foot.min.y + 14.0), 5.0, th.hit);
            line(p, &status, pos2(sx + 20.0, foot.min.y + 4.0), 14.0, 700, th.text, sw - 20.0);
            line(p, &facts, pos2(sx + 20.0, foot.min.y + 27.0), 12.0, 500, th.caption, sw - 20.0);
        } else {
            line(p, &status, pos2(sx, foot.min.y + 4.0), 14.0, 700, th.text, sw);
            let bar = Rect::from_min_size(pos2(sx, foot.min.y + 30.0), vec2(sw, 10.0));
            paint::inset(p, bar, Some(5.0), 2.0, Fill::Base);
            if fraction > 0.0 {
                paint::pill(p, Rect::from_min_size(bar.min, vec2((bar.width() * fraction).max(10.0), bar.height())), th.accent);
            }
        }
        if let Some(path) = done_path {
            let show = Rect::from_min_size(pos2(foot.max.x - 284.0, foot.min.y + 5.0), vec2(136.0, 36.0));
            if widgets::button(ui, show, "reveal", "Show the file", "", ButtonStyle::default()).clicked() {
                open_path(&path, true);
            }
            let open = Rect::from_min_size(pos2(foot.max.x - 136.0, foot.min.y + 5.0), vec2(136.0, 36.0));
            if widgets::button(ui, open, "quickrs", "Play in quickRS", "", ButtonStyle::main()).clicked() {
                open_quickrs();
            }
        }
        if let Job::Done(done) = &self.job
            && let Some(note) = done.notes.first()
        {
            line(p, note, pos2(r.min.x, foot.max.y + 4.0), 11.0, 500, th.caption, r.width());
        }
        if pressed && ready.is_ok() && !running {
            self.start(&ctx);
        }
    }

    fn sidebar(&mut self, ui: &mut egui::Ui, p: &egui::Painter, side: Rect) {
        let th = t();
        let ctx = ui.ctx().clone();
        paint::card(p, side, 30.0, false);
        let (x, w) = (side.min.x + 22.0, side.width() - 44.0);
        // the name plate
        let mark = Rect::from_min_size(pos2(x, side.min.y + 20.0), vec2(44.0, 44.0));
        if self.logo.is_none() {
            // the icon's plate without the margin macOS wants around it
            let plate = image::load_from_memory(include_bytes!("../assets/icon.png")).ok().map(|i| i.crop_imm(100, 100, 824, 824).resize(176, 176, image::imageops::FilterType::Lanczos3).to_rgba8());
            self.logo = plate.map(|i| ctx.load_texture("logo", egui::ColorImage::from_rgba_unmultiplied([i.width() as usize, i.height() as usize], i.as_raw()), egui::TextureOptions::LINEAR));
        }
        if let Some(logo) = &self.logo {
            p.image(logo.id(), mark, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        line(p, "Tabpak", pos2(mark.max.x + 14.0, mark.min.y + 3.0), 17.0, 800, th.text, w - 58.0);
        line(p, "Songsterr to quickRS", pos2(mark.max.x + 14.0, mark.min.y + 25.0), 11.0, 500, th.caption, w - 58.0);

        // the foot: the folder and quickRS
        let open = Rect::from_min_size(pos2(x, side.max.y - 22.0 - 40.0), vec2(w, 40.0));
        let open_key = ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::O));
        if widgets::button(ui, open, "side-quickrs", "Open quickRS", "Ctrl O", ButtonStyle::default().icon(Icon::Play)).clicked() || open_key {
            open_quickrs();
        }
        let half = (w - 8.0) / 2.0;
        let small = ButtonStyle { size: 12.0, r: Some(10.0), ..Default::default() };
        let folder = Rect::from_min_size(pos2(x, open.min.y - 12.0 - 30.0), vec2(half, 30.0));
        if widgets::button(ui, folder, "side-folder", "Open folder", "", small).clicked() {
            let _ = std::fs::create_dir_all(&self.prefs.songs_dir);
            open_path(&self.prefs.songs_dir, false);
        }
        let change = folder.translate(vec2(half + 8.0, 0.0));
        if widgets::button(ui, change, "side-change", "Change\u{2026}", "", small).clicked()
            && let Some(dir) = rfd::FileDialog::new().set_title("Where your songs go").set_directory(&self.prefs.songs_dir).pick_folder()
        {
            self.prefs.songs_dir = dir;
            self.prefs.save();
            self.quickrs_knows = convert::quickrs_knows(&self.prefs.songs_dir);
            self.refresh_saved(&ctx);
        }
        let mut fy = folder.min.y - 10.0;
        if self.quickrs_knows == Some(false) {
            let add = Rect::from_min_size(pos2(x, fy - 30.0), vec2(w, 30.0));
            if widgets::button(ui, add, "side-add", "Add this folder to quickRS", "", small).clicked() && convert::quickrs_add(&self.prefs.songs_dir).is_ok() {
                self.quickrs_knows = Some(true);
            }
            line(p, "quickRS doesn't look in this folder yet", pos2(x, add.min.y - 20.0), 11.0, 500, th.late, w);
            fy = add.min.y - 28.0;
        }
        line(p, &home_short(&self.prefs.songs_dir), pos2(x, fy - 16.0), 12.0, 700, th.text_2, w);
        side_caption(p, "Songs folder", pos2(x, fy - 34.0));
        let list_bottom = fy - 36.0 - 18.0;

        // the songs made so far
        let top = mark.max.y + 30.0;
        side_caption(p, "Your songs", pos2(x, top + 2.0));
        text::draw(p, &self.saved.len().to_string(), pos2(x + w, top), Align2::RIGHT_TOP, Style::new(12.0, 700, th.caption), None);
        let row_h = 52.0;
        let rows = ((list_bottom - top - 26.0) / row_h).floor().max(0.0) as usize;
        if self.saved.is_empty() {
            para(p, "Songs you convert show here, newest first.", pos2(x, top + 28.0), 12.0, th.caption, w);
        }
        let mut again = None;
        let on_stage = if let Stage::Song(loaded) = &self.stage { Some(loaded.song.id) } else { None };
        for (i, s) in self.saved.iter().take(rows).enumerate() {
            let row = Rect::from_min_size(pos2(side.min.x + 10.0, top + 24.0 + i as f32 * row_h), vec2(side.width() - 20.0, row_h));
            let resp = widgets::interact(ui, row, ("saved", i), Sense::click());
            let current = s.song_id.is_some() && s.song_id == on_stage;
            if current {
                paint::inset(p, row.shrink2(vec2(0.0, 2.0)), Some(14.0), 2.0, Fill::Base);
            }
            if resp.hovered() {
                if !current {
                    paint::rrect(p, row.shrink2(vec2(0.0, 2.0)), 14.0, th.base_hi);
                }
                if s.song_id.is_some() {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
            let art = Rect::from_min_size(pos2(x, row.center().y - 20.0), vec2(40.0, 40.0));
            if !self.covers.contains_key(&s.path)
                && let Some(tex) = s.cover.as_ref().and_then(|jpeg| texture(&ctx, &s.path.to_string_lossy(), jpeg))
            {
                self.covers.insert(s.path.clone(), tex);
            }
            let tex = self.covers.get(&s.path).map(|c| c.id());
            paint::art(p, tex, art, 9.0, tex.is_none());
            if tex.is_none() {
                icons::icon(p, Icon::Music, art.center(), 14.0, th.text_3);
            }
            let tw = row.max.x - art.max.x - 22.0;
            line(p, &s.title, pos2(art.max.x + 12.0, row.center().y - 17.0), 13.0, 700, if current { th.accent } else { th.text }, tw);
            let mut under = s.artist.clone();
            if s.songsterr_sound {
                under.push_str("  \u{00b7}  Songsterr sound");
            } else if !s.parts.is_empty() {
                under.push_str(&format!("  \u{00b7}  {} part{}", s.parts.len(), if s.parts.len() == 1 { "" } else { "s" }));
            }
            line(p, &under, pos2(art.max.x + 12.0, row.center().y + 2.0), 11.0, 500, th.caption, tw);
            if resp.clicked() {
                again = s.song_id;
            }
        }
        if self.saved.len() > rows && rows > 0 {
            let more = format!("and {} more in the folder", self.saved.len() - rows);
            line(p, &more, pos2(x, top + 26.0 + rows as f32 * row_h), 11.0, 500, th.caption, w);
        }
        if let Some(id) = again {
            self.query = id.to_string();
            self.ask(&ctx);
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c = t().base;
        [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, 1.0]
    }
}
