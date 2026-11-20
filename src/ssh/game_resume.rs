//! Private, opt-in RustMUSH control channel. No sideband bytes enter the VT stream.
use super::sharing::ConnectionShare;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::oneshot,
    time::{Instant, timeout_at},
};
use zeroize::{Zeroize, Zeroizing};

const SUBSYSTEM: &str = "rustmush-resume-v1";
pub(super) const ENVIRONMENT: &str = "RUSTMUSH_RESUME_V1";
const MAX_FRAME: usize = 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_GRACE: Duration = Duration::from_secs(300);

struct Lease {
    ticket: Zeroizing<String>,
    peer: String,
    grace: Duration,
    lost: Option<Instant>,
}
pub(super) struct Pane {
    nonce: String,
    lease: Option<Lease>,
}
impl Pane {
    pub fn new() -> Self {
        Self {
            nonce: format!("{:032x}", rand::random::<u128>()),
            lease: None,
        }
    }
    pub fn disconnected(&mut self) {
        if let Some(lease) = &mut self.lease {
            lease.lost.get_or_insert_with(Instant::now);
        }
    }
    pub fn probe(&mut self, peer: &str) -> Probe {
        if self.lease.as_ref().is_some_and(|l| {
            l.peer != peer || l.lost.is_none_or(|lost| Instant::now() >= lost + l.grace)
        }) {
            self.lease = None;
        }
        Probe {
            pane: self.nonce.clone(),
            ticket: self.lease.as_ref().map(|l| l.ticket.clone()),
            deadline: self
                .lease
                .as_ref()
                .and_then(|l| l.lost.map(|lost| lost + l.grace)),
        }
    }
    pub fn finish(&mut self, peer: &str, outcome: Outcome) -> &'static str {
        let status = outcome.status();
        self.lease = match outcome {
            Outcome::Issued(ticket, grace) | Outcome::Redeemed(ticket, grace) => Some(Lease {
                ticket,
                grace,
                peer: peer.to_owned(),
                lost: None,
            }),
            _ => None,
        };
        status
    }
}

pub(super) struct Probe {
    pane: String,
    ticket: Option<Zeroizing<String>>,
    deadline: Option<Instant>,
}
impl Probe {
    pub fn redeeming(&self) -> bool {
        self.ticket.is_some()
    }
    fn operation(&self) -> &'static str {
        if self.redeeming() { "redeem" } else { "issue" }
    }
    pub fn marker(&self) -> String {
        format!("{}:{}", self.operation(), self.pane)
    }
    pub async fn exchange(&self, link: &ConnectionShare, auth: oneshot::Sender<()>) -> Outcome {
        let open = async {
            let mut side = link.open_shell().await.ok()?;
            let channel = side.channel();
            channel.request_subsystem(true, SUBSYSTEM).await.ok()?;
            if !matches!(channel.wait().await, Some(russh::ChannelMsg::Success)) {
                return None;
            }
            let request = Request {
                v: 1,
                op: self.operation(),
                pane: &self.pane,
                ticket: self.ticket.as_ref().map(|t| t.as_str()),
            };
            let payload = Zeroizing::new(serde_json::to_vec(&request).ok()?);
            channel
                .data(&(payload.len() as u32).to_be_bytes()[..])
                .await
                .ok()?;
            channel.data(payload.as_slice()).await.ok()?;
            Some(side)
        };
        let final_deadline = self.deadline.unwrap_or_else(|| Instant::now() + MAX_GRACE);
        let first_deadline = (Instant::now() + PROBE_TIMEOUT).min(final_deadline);
        let Ok(Some(mut side)) = timeout_at(first_deadline, open).await else {
            return Outcome::Unsupported;
        };
        let mut reader = side.channel().make_reader();
        let first = if self.redeeming() {
            first_deadline
        } else {
            final_deadline
        };
        let Ok(Some(mut response)) = timeout_at(first, read_response(&mut reader)).await else {
            return Outcome::Unavailable;
        };
        if matches!(response, Response::AuthRequired) {
            let _ = auth.send(());
            let Ok(Some(final_response)) =
                timeout_at(final_deadline, read_response(&mut reader)).await
            else {
                return Outcome::Unavailable;
            };
            response = final_response;
        }
        match response {
            Response::Issued {
                ticket,
                grace_seconds,
            } if !self.redeeming() => accepted(ticket, grace_seconds, false),
            Response::Redeemed {
                ticket,
                grace_seconds,
            } if self.redeeming() => accepted(ticket, grace_seconds, true),
            Response::Expired => Outcome::Expired,
            Response::Denied => Outcome::Denied,
            Response::Busy => Outcome::Busy,
            _ => Outcome::Unavailable,
        }
    }
}
#[derive(Serialize)]
struct Request<'a> {
    v: u8,
    op: &'a str,
    pane: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    ticket: Option<&'a str>,
}
#[derive(Deserialize)]
struct Envelope {
    v: u8,
    #[serde(flatten)]
    response: Response,
}
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Response {
    Issued { ticket: String, grace_seconds: u16 },
    Redeemed { ticket: String, grace_seconds: u16 },
    AuthRequired,
    Expired,
    Denied,
    Busy,
    Unavailable,
}
async fn read_response(r: &mut (impl AsyncRead + Unpin)) -> Option<Response> {
    let n = r.read_u32().await.ok()? as usize;
    if !(1..=MAX_FRAME).contains(&n) {
        return None;
    }
    let mut frame = Zeroizing::new(vec![0; n]);
    r.read_exact(&mut frame).await.ok()?;
    let response: Envelope = serde_json::from_slice(&frame).ok()?;
    (response.v == 1).then_some(response.response)
}
fn accepted(ticket: String, grace: u16, redeemed: bool) -> Outcome {
    let ticket = Zeroizing::new(ticket);
    let mut raw = [0u8; 32];
    let canonical =
        ticket.len() == 43 && URL_SAFE_NO_PAD.decode_slice(ticket.as_bytes(), &mut raw) == Ok(32);
    raw.zeroize();
    if !canonical || !(1..=300).contains(&grace) {
        return Outcome::Unavailable;
    }
    let grace = Duration::from_secs(grace.into());
    if redeemed {
        Outcome::Redeemed(ticket, grace)
    } else {
        Outcome::Issued(ticket, grace)
    }
}

pub(super) enum Outcome {
    Issued(Zeroizing<String>, Duration),
    Redeemed(Zeroizing<String>, Duration),
    Unsupported,
    Expired,
    Denied,
    Busy,
    Unavailable,
}
impl Outcome {
    fn status(&self) -> &'static str {
        match self {
            Self::Issued(..) => "issued",
            Self::Redeemed(..) => "redeemed",
            Self::Unsupported => "unsupported",
            Self::Expired => "expired",
            Self::Denied => "denied",
            Self::Busy => "busy",
            Self::Unavailable => "unavailable",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn issued() -> Outcome {
        accepted(URL_SAFE_NO_PAD.encode([7; 32]), 20, false)
    }
    #[test]
    fn tickets_stay_on_the_verified_peer_and_keep_the_first_drop_deadline() {
        let mut game = Pane::new();
        game.finish("first", issued());
        game.disconnected();
        let first = game.probe("first").deadline.unwrap();
        game.disconnected();
        assert_eq!(game.probe("first").deadline, Some(first));
        assert!(!game.probe("different-key").redeeming());
        game.finish("first", issued());
        game.lease.as_mut().unwrap().lost = Some(Instant::now() - Duration::from_secs(21));
        assert!(!game.probe("first").redeeming());
    }
    #[tokio::test]
    async fn capped_frames_and_canonical_tickets() {
        for bytes in [
            0u32.to_be_bytes(),
            1025u32.to_be_bytes(),
            u32::MAX.to_be_bytes(),
        ] {
            assert!(read_response(&mut &bytes[..]).await.is_none());
        }
        let good = URL_SAFE_NO_PAD.encode([0; 32]);
        for (value, grace) in [
            (good.clone(), 0),
            (good.clone(), 301),
            (good + "=", 10),
            ("!".repeat(43), 10),
        ] {
            assert!(matches!(accepted(value, grace, true), Outcome::Unavailable));
        }
        let body = br#"{"v":2,"status":"expired"}"#;
        let frame = [(body.len() as u32).to_be_bytes().as_slice(), body].concat();
        assert!(read_response(&mut &frame[..]).await.is_none());
    }
}
