//! coderadio — terminal player for freeCodeCamp Code Radio.

mod nowplaying;
mod stream;

use clap::Parser;
use crossterm::cursor::{Hide, MoveTo, MoveToColumn, Show};
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
};
use parking_lot::Mutex;
use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink, Source};
use std::io::{self, Cursor, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const NOWPLAYING: &str =
    "https://coderadio-admin-v2.freecodecamp.org/api/nowplaying_static/coderadio.json";

const ART: [&str; 3] = [
    "▄█████ ▄████▄ ████▄  ██████   █████▄  ▄████▄ ████▄  ██ ▄████▄",
    "██     ██  ██ ██  ██ ██▄▄     ██▄▄██▄ ██▄▄██ ██  ██ ██ ██  ██",
    "▀█████ ▀████▀ ████▀  ██▄▄▄▄   ██   ██ ██  ██ ████▀  ██ ▀████▀",
];
/// Alternate between a CD and a DVD to convey the disc "spinning".
const SPIN: [char; 2] = ['💿', '📀'];

/// Ellipsis animation frames (trailing spaces keep the line a fixed width).
const DOTS: [&str; 3] = [".  ", ".. ", "..."];
const CONTROLS: &str = "[\"Space\" to Play/Pause] [\"0-9\" for Volume] [\"Ctrl+C\" to Quit]";
const REPO: &str = "https://github.com/p1n2o/coderadio";
const SPEAKER: &str = "🔊";
const MUTED: &str = "🔇";
const HINT: &str = "Muted (\"0\" to Unmute)";

const FRAME: Duration = Duration::from_millis(80); // ~12.5 fps tick

fn listen_url(cli: &Cli) -> String {
    if cli.low {
        "https://coderadio-admin-v2.freecodecamp.org/listen/coderadio/low.mp3".into()
    } else {
        cli.url.clone()
    }
}

#[derive(Parser)]
#[command(
    name = "coderadio",
    version,
    about = "Play freeCodeCamp Code Radio in your terminal"
)]
struct Cli {
    /// Use the 64kbps stream instead of the default 128kbps.
    #[arg(long)]
    low: bool,

    /// Playback volume, 0–100.
    #[arg(long, default_value_t = 100)]
    volume: u8,

    /// Override the MP3 stream URL.
    #[arg(
        long,
        default_value = "https://coderadio-admin-v2.freecodecamp.org/listen/coderadio/radio.mp3"
    )]
    url: String,

    /// Fetch this many bytes of the stream, decode them, print stats, then
    /// exit. Verifies networking + decoding without needing an audio device.
    #[arg(long, value_name = "BYTES")]
    probe: Option<usize>,
}

/// Shared UI/playback state, mutated by the input path, the metadata poller
/// and the ctrl-C handler.
struct UiState {
    playing: bool,
    volume: u8,       // 0–100
    prev: Option<u8>, // last non-muted volume for the 0-key mute toggle
    song: String,     // "Artist — Title"
    quit: bool,
}

/// Resources owned by the main loop. `_output` keeps the stream alive; a
/// paused stream is dropped, so resume always reconnects to live audio.
struct Playback {
    _output: OutputStream,
    sink: Arc<Sink>,
    stream: Option<Arc<stream::Stream>>,
}

/// A decoder is being spun up on a background thread.
static CONNECTING: AtomicBool = AtomicBool::new(false);

fn main() {
    let cli = Cli::parse();

    if let Some(bytes) = cli.probe {
        probe(&listen_url(&cli), bytes);
        return;
    }
    let url = listen_url(&cli);

    enable_raw_mode().expect("raw mode");
    let mut out = io::stdout();
    queue!(out, EnterAlternateScreen, Hide).expect("enter alt screen");
    out.flush().unwrap();

    // Best-effort: let capable terminals report key repeats/releases so a
    // held key is treated as a single press. Ignored where unsupported.
    queue!(
        out,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        )
    )
    .ok();
    out.flush().ok();

    let state = Arc::new(Mutex::new(UiState {
        playing: true,
        volume: cli.volume.min(100),
        prev: None,
        song: String::new(),
        quit: false,
    }));

    // ctrl-C safety net (most terminals deliver it as a key event in raw mode).
    ctrlc::set_handler({
        let state = state.clone();
        move || state.lock().quit = true
    })
    .expect("install ctrl-c handler");

    // Metadata poller keeps `state.song` fresh.
    {
        let state = state.clone();
        thread::spawn(move || nowplaying::run(NOWPLAYING, state));
    }

    let mut playback = match open_playback(cli.volume) {
        Ok(p) => p,
        Err(e) => {
            tear_down(&mut out);
            eprintln!("error: cannot open audio output: {e}");
            eprintln!("hint: pass --probe <bytes> to verify the stream without a device");
            std::process::exit(2);
        }
    };

    let mut tick: u64 = 0;
    let mut last_toggle = Instant::now() - Duration::from_secs(10);
    loop {
        // Drain queued key events.
        if event::poll(FRAME).unwrap() {
            loop {
                match event::read() {
                    Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => match k.code {
                        KeyCode::Char(' ') => {
                            // Debounce: some terminals re-deliver / auto-repeat
                            // the key, which would toggle twice instantly.
                            let now = Instant::now();
                            if now.duration_since(last_toggle) >= Duration::from_millis(150) {
                                last_toggle = now;
                                let mut s = state.lock();
                                s.playing = !s.playing;
                            }
                        }
                        KeyCode::Char('0') => {
                            // Toggle mute (debounced: it's a toggle, same
                            // phantom-double-press risk as Space).
                            let now = Instant::now();
                            if now.duration_since(last_toggle) >= Duration::from_millis(150) {
                                last_toggle = now;
                                let v = apply_volume_key(&mut state.lock(), '0');
                                playback.sink.set_volume(v as f32 / 100.0);
                            }
                        }
                        KeyCode::Char(c @ '1'..='9') => {
                            let v = apply_volume_key(&mut state.lock(), c);
                            playback.sink.set_volume(v as f32 / 100.0);
                        }
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            state.lock().quit = true;
                        }
                        _ => {}
                    },
                    Ok(_) => {}
                    Err(_) => break,
                }
                if !event::poll(Duration::ZERO).unwrap() {
                    break;
                }
            }
        }

        if state.lock().quit {
            break;
        }

        playback_tick(&mut playback, &url, &state);
        render(&mut out, &state, tick);
        out.flush().unwrap();
        tick = tick.wrapping_add(1);
    }

    playback.sink.stop();
    tear_down(&mut out);
    eprintln!("bye");
}

/// Advance the live-stream state machine for one frame. Never blocks: the
/// potentially-slow decoder setup runs on a background thread so the TUI
/// stays responsive while (re)connecting.
fn playback_tick(pb: &mut Playback, url: &str, state: &Arc<Mutex<UiState>>) {
    if !state.lock().playing {
        // Paused: stop audio and tear down the live stream so resume
        // reconnects to the current broadcast (never the old position).
        if !pb.sink.empty() {
            pb.sink.stop();
        }
        if let Some(s) = pb.stream.take() {
            s.stop();
        }
        CONNECTING.store(false, Ordering::Relaxed);
        return;
    }
    if pb.stream.is_none() {
        let s = stream::Stream::new(url.to_string());
        let t = s.clone();
        thread::spawn(move || t.run());
        pb.stream = Some(s);
    }
    if pb.sink.empty() && !CONNECTING.load(Ordering::Relaxed) {
        CONNECTING.store(true, Ordering::Relaxed);
        let reader = pb.stream.as_ref().unwrap().reader();
        let sink = pb.sink.clone();
        thread::spawn(move || {
            match Decoder::new(reader) {
                Ok(d) => sink.append(d),
                Err(_) => thread::sleep(Duration::from_secs(2)),
            }
            CONNECTING.store(false, Ordering::Relaxed);
        });
    }
}

fn render(out: &mut impl Write, state: &Arc<Mutex<UiState>>, tick: u64) {
    let st = state.lock();
    let playing = st.playing;
    let volume = st.volume;
    let status = if playing {
        // Slow cadence: the CD/DVD icon toggles every 5 frames (~1.6s/turn);
        // the ellipsis cycles ".  " -> ".. " -> "..." every 6 frames.
        let spin = SPIN[(tick / 5) as usize % SPIN.len()];
        let dots = DOTS[(tick / 6) as usize % DOTS.len()];
        format!("{spin} Now Playing: {}{dots}", st.song)
    } else {
        // Blink the paused line (dim <-> normal) every ~6 frames.
        let base = "⏸  Paused [\"Space\" to Resume]";
        if (tick / 6) % 2 == 1 {
            format!("\x1b[2m{base}\x1b[0m")
        } else {
            base.to_string()
        }
    };
    drop(st);

    let art_width = ART[0].chars().count();
    let rows = [
        String::new(),
        ART[0].into(),
        ART[1].into(),
        ART[2].into(),
        String::new(), // spacing after the ascii art
        center(REPO, art_width),
        String::new(), // spacing after the repo URL
        volume_line(volume, art_width, tick),
        String::new(),
        status,
        String::new(),
        CONTROLS.into(),
    ];

    queue!(out, MoveTo(0, 0)).unwrap();
    for r in &rows {
        queue!(
            out,
            MoveToColumn(0),
            Print(r),
            Clear(ClearType::UntilNewLine),
            Print("\n")
        )
        .unwrap();
    }
}

/// `🔊 [██████…░░] 80` — the bar fills the full ascii-art width, with the fill
/// proportional to volume (muted icon and empty bar at volume 0).
/// Volume bar: normal bars while playing; when muted, a blinking, centered
/// `[0 to Unmute]` hint fills the bar area so the user knows why it's silent.
/// The bar always spans the full ascii-art width (no shift).
fn volume_line(volume: u8, width: usize, tick: u64) -> String {
    let icon = if volume == 0 { MUTED } else { SPEAKER };
    let left = format!("{icon} [");
    // Right-align the number to 3 digits so the bar width never shifts.
    let right = format!("] {volume:>3}");
    let bar_width = width.saturating_sub(cell_width(&left) + cell_width(&right));

    let line = if volume == 0 {
        format!("{left}{}{right}", center_muted(bar_width))
    } else {
        let filled = (bar_width as u64 * volume as u64 / 100) as usize;
        let bar = format!("{}{}", "█".repeat(filled), "░".repeat(bar_width - filled));
        format!("{left}{bar}{right}")
    };

    if volume == 0 && (tick / 6) % 2 == 1 {
        format!("\x1b[2m{line}\x1b[0m") // blink: dim on odd phase
    } else {
        line
    }
}

/// Center the hint (one space either side) within `bar_width` cells, filling
/// the rest with empty bar cells so the total row width stays constant.
fn center_muted(bar_width: usize) -> String {
    let content = format!(" {} ", HINT); // padded with a space either side
    let cw = content.chars().count();
    let rest = bar_width.saturating_sub(cw);
    if rest >= 2 {
        let left = rest / 2;
        let right = rest - left;
        format!("{}{}{}", "░".repeat(left), content, "░".repeat(right))
    } else {
        content // cannot fit: drop the padding and show it anyway
    }
}

/// Center `text` within `width` cells (no-op / as-is when it won't fit).
fn center(text: &str, width: usize) -> String {
    let tl = text.chars().count();
    if tl >= width {
        return text.to_string();
    }
    let left = (width - tl) / 2;
    let right = width - tl - left;
    format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
}

/// Approximate terminal cell width (the speaker emoji is double-width).
fn cell_width(s: &str) -> usize {
    s.chars()
        .map(|c| if matches!(c, '🔊' | '🔇') { 2 } else { 1 })
        .sum()
}

/// Apply a volume or mute (`0`) key to `s`, returning the new volume.
///
/// `0` toggles between mute and the previous volume; with no previous volume
/// it falls back to 100. Digits `1-9` set a concrete volume and record it as
/// the restore point.
fn apply_volume_key(s: &mut UiState, key: char) -> u8 {
    match key {
        '0' => {
            if s.volume > 0 {
                s.prev = Some(s.volume);
                s.volume = 0;
            } else {
                s.volume = s.prev.unwrap_or(100); // 100 = "no previous" fallback
                s.prev = None;
            }
        }
        c @ '1'..='9' => {
            let d = c as u8 - b'0';
            s.volume = ((d as f32) * 100.0 / 9.0).round() as u8;
            s.prev = Some(s.volume);
        }
        _ => {}
    }
    s.volume
}

fn tear_down(out: &mut impl Write) {
    let _ = disable_raw_mode();
    let _ = queue!(out, PopKeyboardEnhancementFlags, LeaveAlternateScreen, Show);
    let _ = out.flush();
}

/// Open the default output device and return a control handle.
fn open_playback(volume: u8) -> Result<Playback, rodio::StreamError> {
    let _output = OutputStreamBuilder::from_default_device()?.open_stream()?;
    let sink = Arc::new(Sink::connect_new(_output.mixer()));
    sink.set_volume((volume.min(100) as f32) / 100.0);
    Ok(Playback {
        _output,
        sink,
        stream: None,
    })
}

/// Download a fixed chunk, decode it offline, and report what we got.
fn probe(url: &str, bytes: usize) {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("coderadio/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client");
    let mut resp = match client.get(url).send().and_then(|r| r.error_for_status()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("probe: HTTP error: {e}");
            std::process::exit(1);
        }
    };

    let mut body = Vec::with_capacity(bytes.min(1 << 20));
    let mut taken = 0u64;
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        match resp.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                body.extend_from_slice(&chunk[..n]);
                taken += n as u64;
                if taken >= bytes as u64 {
                    break;
                }
            }
            Err(e) => {
                eprintln!("probe: read error: {e}");
                std::process::exit(1);
            }
        }
    }

    eprintln!("probe: fetched {} bytes", body.len());
    let mut dec = match Decoder::new(Cursor::new(body)) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("probe: decode failed: {e}");
            std::process::exit(1);
        }
    };

    let channels = dec.channels() as u64;
    let rate = dec.sample_rate() as u64;
    let mut samples = 0u64; // one iterator step = one interleaved channel sample
    while let Some(_) = dec.next() {
        samples += 1;
    }
    let secs = samples / channels / rate;
    eprintln!("probe: ok — {channels} ch, {rate} Hz, {samples} samples (~{secs}s audio)");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui(playing: bool, song: &str, volume: u8) -> Arc<Mutex<UiState>> {
        Arc::new(Mutex::new(UiState {
            playing,
            volume,
            prev: None,
            song: song.to_string(),
            quit: false,
        }))
    }

    fn frame(playing: bool, song: &str, volume: u8, tick: u64) -> String {
        let mut buf = Vec::new();
        render(&mut buf, &ui(playing, song, volume), tick);
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn playing_frame_has_banner_song_and_controls() {
        let f = frame(true, "Nym — Come Back", 80, 12); // 12 -> dots "..."
        assert!(f.contains(ART[0]), "banner row 1");
        assert!(
            f.contains("Now Playing: Nym — Come Back..."),
            "song + full dots"
        );
        assert!(f.contains("\"Space\" to Play/Pause"), "controls");
    }

    #[test]
    fn playing_dots_animate_slowly_one_at_a_time() {
        let t0 = frame(true, "Song", 80, 0); //   (0/6)%3 = 0 -> ".  "
        let t6 = frame(true, "Song", 80, 6); //   (6/6)%3 = 1 -> ".. "
        let t12 = frame(true, "Song", 80, 12); // (12/6)%3 = 2 -> "..."
        let t18 = frame(true, "Song", 80, 18); // (18/6)%3 = 0 -> ".  " again
        assert!(!t0.contains("Song.."), "frame 0 is single dot");
        assert!(
            t6.contains("Song..") && !t6.contains("Song..."),
            "frame 1 is two dots"
        );
        assert!(t12.contains("Song..."), "frame 2 is three dots");
        assert!(
            t18.contains("Song.") && !t18.contains("Song.."),
            "wraps back to single dot"
        );
    }

    #[test]
    fn cd_icon_toggles_between_cd_and_dvd() {
        assert!(frame(true, "Song", 80, 0).contains('💿'), "CD icon @t0");
        assert!(!frame(true, "Song", 80, 0).contains('📀'), "no DVD @t0");
        assert!(frame(true, "Song", 80, 5).contains('📀'), "DVD icon @t5");
        assert!(!frame(true, "Song", 80, 5).contains('💿'), "no CD @t5");
        assert!(
            frame(true, "Song", 80, 10).contains('💿'),
            "back to CD @t10"
        );
    }

    #[test]
    fn paused_frame_shows_paused_line() {
        let f = frame(false, "Nym — Come Back", 80, 1);
        assert!(f.contains("Paused"), "paused line shown");
        assert!(
            !f.contains("Now Playing"),
            "no now-playing line while paused"
        );
    }

    #[test]
    fn paused_line_blinks_via_dim_attribute() {
        let normal = frame(false, "", 80, 1); // (1/6)%2 == 0 -> normal
        let dim = frame(false, "", 80, 7); //    (7/6)%2 == 1 -> dimmed
        assert!(!normal.contains("\x1b[2m"), "not dimmed on even phase");
        assert!(dim.contains("\x1b[2m"), "dimmed on odd phase");
    }

    #[test]
    fn volume_bar_expands_to_art_width_and_is_proportional() {
        let w = ART[0].chars().count();
        let l80 = volume_line(80, w, 0);
        assert!(l80.starts_with("🔊 ["), "speaker icon");
        assert!(l80.ends_with("80"), "value shown");
        assert_eq!(cell_width(&l80), w, "bar fills the full art width");
        // fill ratio ~80%: many filled, some empty.
        let filled = l80.matches('█').count();
        let empty = l80.matches('░').count();
        assert!(filled > empty && empty > 0, "mostly filled at 80");
    }

    #[test]
    fn full_volume_fills_the_bar() {
        let l = volume_line(100, ART[0].chars().count(), 0);
        assert!(l.starts_with("🔊 ["), "speaker");
        assert!(l.matches('░').count() == 0, "no empty cells at 100");
        // actually: at 100, bar_width == w - cells(left) - cells(right)
        let bar_cells = ART[0].chars().count() - cell_width("🔊 [") - cell_width("] 100");
        assert!(l.matches('█').count() == bar_cells, "fully filled");
    }

    #[test]
    fn muted_shows_centered_hint_in_bar() {
        let l = volume_line(0, ART[0].chars().count(), 1); // tick 1 -> normal
        assert!(l.starts_with("🔇 ["), "muted icon");
        assert!(l.matches('█').count() == 0, "no filled cells when muted");
        assert!(l.ends_with("0"), "zero shown");
        assert!(l.contains(HINT), "hint shown when muted");
        assert_eq!(
            cell_width(&l),
            ART[0].chars().count(),
            "hint keeps full bar width"
        );
    }

    #[test]
    fn muted_hint_blinks() {
        let normal = volume_line(0, ART[0].chars().count(), 1); // (1/6)%2 == 0
        let dim = volume_line(0, ART[0].chars().count(), 7); //   (7/6)%2 == 1
        assert!(!normal.contains("\x1b[2m"), "not dimmed on even phase");
        assert!(dim.contains("\x1b[2m"), "dimmed on odd phase");
    }

    #[test]
    fn bar_width_is_stable_across_volume_values() {
        // The 3-digit reservation must keep the bar the same length whether
        // the value is 0, 22, or 100 (no jank when the digit count changes).
        let w = ART[0].chars().count();
        let widths: Vec<usize> = [0u8, 22, 100]
            .iter()
            .map(|v| cell_width(&volume_line(*v, w, 0)))
            .collect();
        assert!(
            widths.iter().all(|&x| x == w),
            "bar width constant: {widths:?}"
        );
    }

    fn raw(volume: u8, prev: Option<u8>) -> UiState {
        UiState {
            playing: true,
            volume,
            prev,
            song: String::new(),
            quit: false,
        }
    }

    #[test]
    fn zero_toggles_mute_and_restores_previous() {
        let mut s = raw(80, None);
        assert_eq!(apply_volume_key(&mut s, '0'), 0, "mute sets 0");
        assert_eq!(s.prev, Some(80), "remembers previous volume");
        assert_eq!(
            apply_volume_key(&mut s, '0'),
            80,
            "unmute restores previous"
        );
    }

    #[test]
    fn zero_unmute_with_no_previous_falls_back_to_100() {
        let mut s = raw(0, None); // already muted, never set
        assert_eq!(apply_volume_key(&mut s, '0'), 100, "no previous -> 100");
    }

    #[test]
    fn digit_records_prev_then_zero_cycles() {
        let mut s = raw(0, None);
        assert_eq!(apply_volume_key(&mut s, '9'), 100);
        assert_eq!(s.prev, Some(100));
        assert_eq!(apply_volume_key(&mut s, '0'), 0, "mute");
        assert_eq!(
            apply_volume_key(&mut s, '0'),
            100,
            "restore to remembered 100"
        );
    }

    #[test]
    fn digit_overrides_mute_state() {
        let mut s = raw(0, Some(80));
        assert_eq!(
            apply_volume_key(&mut s, '3'),
            33,
            "digit sets a concrete volume"
        );
        assert_eq!(s.prev, Some(33), "becomes new restore point");
    }

    #[test]
    fn repo_url_is_centered_under_the_art() {
        let w = ART[0].chars().count();
        let c = center(REPO, w);
        assert_eq!(c.chars().count(), w, "fills the art width");
        assert!(c.contains(REPO), "url present");
        let left = c.find(REPO).unwrap();
        let right = w - left - REPO.chars().count();
        assert!((left as i64 - right as i64).abs() <= 1, "centered (pad {left}/{right})");
        assert!(frame(true, "Song", 80, 0).contains(REPO), "rendered in the frame");
    }
}
