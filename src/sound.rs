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
                if self.notices.try_send(Notice::Play(name)).is_err() {
                    self.events.push(
                        "sound-error",
                        serde_json::json!({"reason":"command queue full"}),
                    );
                }
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
                    let allowed = {
                        let player = self.player.read().unwrap();
                        match &notice {
                            Notice::Play(_) => player.remote_sounds_enabled(),
                            Notice::Pack(_) => player.sync_assets_enabled(),
                        }
                    };
                    if !allowed {
                        self.events.push(
                            "remote-extension-blocked",
                            serde_json::json!({"reason":"profile policy"}),
                        );
                        return;
                    }
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
fn exact_local_sound(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let wanted = format!("{name}.wav");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .find(|entry| {
            entry.file_name().to_str() == Some(wanted.as_str())
                && entry.file_type().is_ok_and(|t| t.is_file())
        })
        .map(|entry| entry.path())
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
                                .and_then(|dir| exact_local_sound(&dir.join("sounds"), &name))
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
                                .and_then(|b| crate::wav::ValidatedSound::parse(&b).ok());
                            if let Some(sound) = bytes {
                                if unsafe {
                                    PlaySoundW(
                                        windows::core::PCWSTR(sound.as_bytes().as_ptr().cast()),
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
    fn osc440_requires_profile_policy_and_bell_overflow_is_reported() {
        use wezterm_term::{Alert, AlertHandler};
        let events = Events::default();
        let player = Arc::new(RwLock::new(PlayerOptions::default()));
        let (notices, mut receiver) = mpsc::channel(1);
        let mut alerts = Alerts {
            events: events.clone(),
            notices,
            player: player.clone(),
        };
        let command = || {
            Alert::W11termOsc440(vec![
                b"440".to_vec(),
                b"1".to_vec(),
                b"play".to_vec(),
                b"shot".to_vec(),
            ])
        };
        alerts.alert(command());
        assert!(receiver.try_recv().is_err());
        player.write().unwrap().remote_sounds = Some(true);
        alerts.alert(command());
        assert!(matches!(receiver.try_recv(),Ok(Notice::Play(name)) if name=="shot"));
        alerts.alert(Alert::Bell);
        alerts.alert(Alert::Bell);
        assert!(
            events
                .since(0)
                .events
                .iter()
                .any(|event| event.kind == "sound-error")
        );
    }
    #[test]
    fn local_sound_names_are_case_sensitive_on_windows_too() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Shot.wav"), []).unwrap();
        assert!(exact_local_sound(dir.path(), "Shot").is_some());
        assert!(exact_local_sound(dir.path(), "shot").is_none());
    }
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
