//! Track metadata from the AzuraCast now-playing JSON API.

use crate::UiState;
use parking_lot::Mutex;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

#[derive(Deserialize, Clone, Default)]
pub struct NowPlaying {
    #[serde(default)]
    pub now_playing: Option<Slot>,
}

#[derive(Deserialize, Clone, Default)]
pub struct Slot {
    #[serde(default)]
    pub song: Option<Song>,
}

#[derive(Deserialize, Clone, Default)]
pub struct Song {
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// `now playing - <artist — title>` (or just title when no artist), or `None`
/// when there's nothing identifiable to show.
fn status_text(song: &Song) -> Option<String> {
    match (&song.artist, &song.title) {
        (Some(a), Some(t)) if !a.is_empty() && !t.is_empty() => Some(format!("{a} — {t}")),
        (_, Some(t)) if !t.is_empty() => Some(t.clone()),
        _ => None,
    }
}

/// Background thread: poll the now-playing endpoint and keep
/// `state.song` (displayed by the TUI) in sync with the current track.
pub fn run(url: &str, state: Arc<Mutex<UiState>>) {
    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("coderadio/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(10))
        .build()
        .expect("http client");

    let mut last: Option<String> = None;
    while !state.lock().quit {
        if let Some(data) = fetch(&client, url) {
            let text = data
                .now_playing
                .as_ref()
                .and_then(|s| s.song.as_ref())
                .and_then(status_text);

            if let Some(text) = text {
                if last.as_deref() != Some(text.as_str()) {
                    state.lock().song = text.clone();
                    last = Some(text);
                }
            }
        }
        for _ in 0..4 {
            if state.lock().quit {
                return;
            }
            std::thread::sleep(Duration::from_millis(1000));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(artist: &str, title: &str) -> Song {
        Song {
            artist: Some(artist.to_string()),
            title: Some(title.to_string()),
        }
    }

    #[test]
    fn formats_artist_and_title() {
        assert_eq!(
            status_text(&song("Nym", "Come Back")),
            Some("Nym — Come Back".into())
        );
    }

    #[test]
    fn falls_back_to_title_without_artist() {
        assert_eq!(status_text(&song("", "Interlude")), Some("Interlude".into()));
    }

    #[test]
    fn recognizes_missing_track() {
        assert_eq!(status_text(&Song::default()), None);
    }
}

fn fetch(client: &reqwest::blocking::Client, url: &str) -> Option<NowPlaying> {
    let resp = client.get(url).send().ok()?.error_for_status().ok()?;
    resp.json().ok()
}
