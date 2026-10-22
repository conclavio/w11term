//! Names-only sound commands. Audio never blocks the parser or changes focus.
use crate::{assets, events::Events, options::PlayerOptions};
use std::{
    io::Read,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;
#[derive(Debug)]
pub enum Notice {
    Play(String),
    Pack(String),
}
pub struct Alerts {
    pub events: Events,
    pub notices: mpsc::Sender<Notice>,
    pub player: Arc<RwLock<PlayerOptions>>,
}
impl wezterm_term::AlertHandler for Alerts {
    fn alert(&mut self, alert: wezterm_term::Alert) {
        use wezterm_term::Alert;
        match alert {
            Alert::Bell => {
                self.events.push("bell", serde_json::json!({}));
                let name = self.player.read().unwrap().bell.clone();
                let _ = self.notices.try_send(Notice::Play(name));
            }
            Alert::WindowTitleChanged(title) => self
                .events
                .push("title", serde_json::json!({"title":title})),
            Alert::ToastNotification { title, body, .. } => self.events.push(
                "notification",
                serde_json::json!({"title":title,"body":body}),
            ),
            Alert::W11termOsc440(parts) => {
                let parsed = parse(&parts);
                if let Some(notice) = parsed {
                    if self.notices.try_send(notice).is_err() {
                        self.events.push(
                            "sound-error",
                            serde_json::json!({"reason":"command queue full"}),
                        );
                    }
                } else {
                    self.events.push(
                        "sound-error",
                        serde_json::json!({"reason":"invalid OSC 440 command"}),
                    );
                }
            }
            _ => {}
        }
    }
}
pub fn parse(parts: &[Vec<u8>]) -> Option<Notice> {
    if parts.len() != 4 || parts[0] != b"440" || parts[1] != b"1" {
        return None;
    }
    let value = std::str::from_utf8(&parts[3]).ok()?;
    if !assets::name(value) {
        return None;
    }
    match parts[2].as_slice() {
        b"play" => Some(Notice::Play(value.into())),
        b"pack" => Some(Notice::Pack(value.into())),
        _ => None,
    }
}
pub fn worker(
    player: Arc<RwLock<PlayerOptions>>,
    store: assets::Store,
    events: Events,
) -> std::sync::mpsc::SyncSender<String> {
    let (sender, receiver) = std::sync::mpsc::sync_channel::<String>(16);
    std::thread::Builder::new()
        .name("terminal-audio".into())
        .spawn(move || {
            let mut previous = Instant::now() - Duration::from_secs(1);
            for name in receiver {
                let muted = player.read().unwrap().mute;
                let outcome = if !assets::name(&name) {
                    "invalid"
                } else if muted || name == "off" {
                    "muted"
                } else if previous.elapsed() < Duration::from_millis(40) {
                    "rate-limited"
                } else {
                    previous = Instant::now();
                    if name == "system" {
                        use windows::Win32::Media::Audio::{
                            PlaySoundW, SND_ALIAS, SND_NODEFAULT, SND_SYNC,
                        };
                        if unsafe {
                            PlaySoundW(
                                windows::core::w!("SystemDefault"),
                                None,
                                SND_ALIAS | SND_NODEFAULT | SND_SYNC,
                            )
                        }
                        .as_bool()
                        {
                            "played"
                        } else {
                            "failed"
                        }
                    } else {
                        let path = store.sound(&name).or_else(|| {
                            crate::keys::data_dir()
                                .ok()
                                .map(|dir| dir.join("sounds").join(format!("{name}.wav")))
                                .filter(|p| p.is_file())
                        });
                        if let Some(path) = path {
                            use windows::Win32::Media::Audio::{
                                PlaySoundW, SND_MEMORY, SND_NODEFAULT, SND_SYNC,
                            };
                            let bytes = std::fs::File::open(&path)
                                .ok()
                                .and_then(|file| {
                                    let mut bytes = Vec::new();
                                    file.take((assets::MAX_FILE + 1) as u64)
                                        .read_to_end(&mut bytes)
                                        .ok()?;
                                    Some(bytes)
                                })
                                .filter(|b| {
                                    b.len() <= assets::MAX_FILE && assets::validate_wav(b).is_ok()
                                });
                            if let Some(bytes) = bytes {
                                if unsafe {
                                    PlaySoundW(
                                        windows::core::PCWSTR(bytes.as_ptr().cast()),
                                        None,
                                        SND_MEMORY | SND_NODEFAULT | SND_SYNC,
                                    )
                                }
                                .as_bool()
                                {
                                    "played"
                                } else {
                                    "failed"
                                }
                            } else {
                                "invalid"
                            }
                        } else {
                            "missing"
                        }
                    }
                };
                events.push("sound", serde_json::json!({"name":name,"outcome":outcome}));
            }
        })
        .expect("starting audio worker");
    sender
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn osc_contract_is_strict() {
        let parse_text = |s: &str| {
            parse(
                &s.split(';')
                    .map(|s| s.as_bytes().to_vec())
                    .collect::<Vec<_>>(),
            )
        };
        assert!(matches!(
            parse_text("440;1;play;shot"),
            Some(Notice::Play(_))
        ));
        assert!(matches!(
            parse_text("440;1;pack;2026_10"),
            Some(Notice::Pack(_))
        ));
        for bad in [
            "440;2;play;shot",
            "440;1;play;../shot.wav",
            "440;1;play;https://x",
            "440;1;play;shot;extra",
            "440;1;exec;shot",
        ] {
            assert!(parse_text(bad).is_none());
        }
    }
}
