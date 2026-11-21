use crate::interaction::{Interaction, PasteWarning};
use crate::terminal::TransportInput;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

/// A clipboard action originates on one pane and one connection. Its worker
/// decides whether the receiving application has actually enabled brackets.
#[derive(Debug)]
pub enum ClipboardPaste {
    New {
        text: String,
        raw: bool,
        connection: u64,
        preferences: Interaction,
    },
    Confirm(PendingPaste),
}

// No Clone: the confirmation consumes the exact payload that was offered.
#[derive(Debug)]
pub struct PendingPaste {
    text: String,
    raw: bool,
    connection: u64,
    origin: Arc<()>,
    bracketed: bool,
    pub warning: PasteWarning,
}

impl PendingPaste {
    pub fn connection(&self) -> u64 {
        self.connection
    }
    pub fn is_raw(&self) -> bool {
        self.raw
    }
}

type PasteResult = Result<Option<PendingPaste>, String>;
pub type ClipboardReply = oneshot::Sender<PasteResult>;

#[derive(Debug)]
pub(super) enum Decision {
    Send { text: String, raw: bool },
    Confirm(PendingPaste),
}

impl ClipboardPaste {
    pub(super) fn evaluate(
        self,
        origin: &Arc<()>,
        connection: u64,
        bracketed: bool,
    ) -> Result<Decision, String> {
        let expected = match &self {
            Self::New { connection, .. } => *connection,
            Self::Confirm(pending) => pending.connection,
        };
        if expected != connection {
            return Err("The pane reconnected. Paste again to use the new connection.".into());
        }
        match self {
            Self::New {
                mut text,
                raw,
                preferences,
                ..
            } => {
                let (prepared, warning) = preferences.clipboard_paste(&text, raw, bracketed);
                text.truncate(prepared.len());
                Ok(match warning {
                    Some(warning) => Decision::Confirm(PendingPaste {
                        text,
                        raw,
                        connection,
                        origin: origin.clone(),
                        bracketed,
                        warning,
                    }),
                    None => Decision::Send { text, raw },
                })
            }
            Self::Confirm(pending) => {
                if !Arc::ptr_eq(&pending.origin, origin) {
                    return Err("The pane that requested this paste has closed.".into());
                }
                if !pending.raw && pending.bracketed != bracketed {
                    return Err(
                        "The application changed paste mode. Paste again to review it.".into(),
                    );
                }
                Ok(Decision::Send {
                    text: pending.text,
                    raw: pending.raw,
                })
            }
        }
    }
}

/// A disconnect can occur after the terminal worker checks the generation
/// and before the transport consumes this message. The transport checks again.
pub(super) fn write(
    writer: &mpsc::UnboundedSender<TransportInput>,
    connection: u64,
    bytes: Vec<u8>,
    reply: ClipboardReply,
) {
    let (ack, received) = oneshot::channel();
    if writer
        .send(TransportInput::Clipboard {
            connection,
            bytes,
            reply: ack,
        })
        .is_err()
    {
        let _ = reply.send(Err("Terminal connection is closed".into()));
        return;
    }
    tokio::spawn(async move {
        let result = received
            .await
            .unwrap_or_else(|_| Err("The connection closed before acknowledging paste.".into()));
        let _ = reply.send(result.map(|()| None));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(origin: &Arc<()>, raw: bool) -> PendingPaste {
        let request = ClipboardPaste::New {
            text: "one\r\ntwo \r\n".into(),
            raw,
            connection: 4,
            preferences: Interaction::default(),
        };
        let Decision::Confirm(pending) = request.evaluate(origin, 4, false).unwrap() else {
            panic!("expected an offer for multiline clipboard text");
        };
        pending
    }

    #[test]
    fn delayed_confirmation_cannot_follow_a_pane_or_a_connection_change() {
        let original = Arc::new(());
        assert!(
            ClipboardPaste::Confirm(offer(&original, false))
                .evaluate(&Arc::new(()), 4, false)
                .is_err()
        );
        assert!(
            ClipboardPaste::Confirm(offer(&original, true))
                .evaluate(&original, 5, false)
                .is_err()
        );
        assert!(
            ClipboardPaste::Confirm(offer(&original, false))
                .evaluate(&original, 4, true)
                .is_err()
        );
    }

    #[test]
    fn confirmed_text_is_the_prepared_original_and_raw_ignores_bracket_changes() {
        let pane = Arc::new(());
        for (raw, bracketed, expected) in [
            (false, false, "one\r\ntwo"),
            (true, true, "one\r\ntwo \r\n"),
        ] {
            let confirmed = ClipboardPaste::Confirm(offer(&pane, raw))
                .evaluate(&pane, 4, bracketed)
                .unwrap();
            assert!(
                matches!(confirmed, Decision::Send { text, raw: mode } if text == expected && raw == mode)
            );
        }
        let stale = ClipboardPaste::New {
            text: "one".into(),
            raw: false,
            connection: 4,
            preferences: Interaction::default(),
        };
        assert!(stale.evaluate(&pane, 5, false).is_err());
    }
}
