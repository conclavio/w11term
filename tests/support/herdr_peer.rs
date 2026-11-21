//! Framed Herdr peer used only by the native SSH transport tests.
use super::*;
use serde_json::{Value, json};
pub(super) const BOOT: &str = "28850aa9-f121-4489-b864-7dedfbbe30b7";
const CORE: &str = "0123456789abcdef0123456789abcdef";
#[derive(Default)]
pub(super) struct Records {
    pub ticks: u64,
    inputs: Vec<Vec<u8>>,
    grids: Vec<Vec<u8>>,
    attaches: usize,
    viewer: Option<(server::Handle, ChannelId)>,
    automatic: Vec<u8>,
}
#[derive(Default, Clone)]
pub(super) struct Wire {
    framed: bool,
    pending: Vec<u8>,
}
fn frame(channel: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
    [
        channel.to_le_bytes().as_slice(),
        &[kind],
        &(payload.len() as u32).to_le_bytes(),
        payload,
    ]
    .concat()
}
fn live() -> Value {
    json!({"address":"t7","pid":112,"name":"build:$(display only)","created_ms":233,"state":"live","keep":true})
}
impl Wire {
    pub(super) fn feed(
        &mut self,
        records: &mut Records,
        c: ChannelId,
        bytes: &[u8],
        s: &mut server::Session,
    ) -> Result<()> {
        self.pending.extend_from_slice(bytes);
        loop {
            if !self.framed {
                let Some(pos) = self.pending.iter().position(|&byte| byte == b'\n') else {
                    break;
                };
                let req: Value =
                    serde_json::from_slice(&self.pending.drain(..=pos).collect::<Vec<_>>())?;
                assert_eq!(req["method"], "hello");
                s.data(c,format!("{}\n",json!({"jsonrpc":"2.0","id":req["id"],"result":{
                    "core":{"id":CORE},"generation":1,"framed":true,"methods":["terminal.list","terminal.attach"],"features":["automatic_input"]}})).into_bytes())?;
                self.framed = true;
                continue;
            }
            if self.pending.len() < 9 {
                break;
            }
            let size = u32::from_le_bytes(self.pending[5..9].try_into().unwrap()) as usize;
            if self.pending.len() < 9 + size {
                break;
            }
            let bytes: Vec<_> = self.pending.drain(..9 + size).collect();
            let data = &bytes[9..];
            let channel = u32::from_le_bytes(bytes[..4].try_into().unwrap());
            match (channel, bytes[4]) {
                (0, 0) => {
                    let req: Value = serde_json::from_slice(data)?;
                    let mut painted = false;
                    let result = match req["method"].as_str().unwrap() {
                        "terminal.list" => {
                            json!({"terminals":[live(),{"address":"remote/t8","state":"live","pid":113,"created_ms":233}]})
                        }
                        "terminal.attach" => {
                            assert_eq!(req["params"]["terminal"], "t7");
                            records.attaches += 1;
                            records.viewer = Some((s.handle(), c));
                            painted = true;
                            json!({"channel":19,"terminal":live(),"automatic_input":true})
                        }
                        other => panic!("unexpected Herdr RPC {other}"),
                    };
                    let mut response = frame(
                        0,
                        0,
                        serde_json::to_string(
                            &json!({"jsonrpc":"2.0","id":req["id"],"result":result}),
                        )?
                        .as_bytes(),
                    );
                    if painted {
                        response.extend(frame(19, 0, b"restore-screen\r\n"));
                    }
                    // Fragment the header too; attach reply and first DATA share the receive buffer.
                    s.data(c, response[..3].to_vec())?;
                    s.data(c, response[3..].to_vec())?;
                }
                (19, 0) => {
                    records.inputs.push(data.to_vec());
                    s.data(c, frame(19, 0, data))?;
                }
                (19, 3) => records.grids.push(data.to_vec()),
                (19, 6) => records.automatic.extend_from_slice(data),
                (19, 1 | 2) => {}
                other => panic!("unexpected Herdr viewer frame {other:?}"),
            }
        }
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn native_ssh_herdr_checks_identity_before_input_and_preserves_the_remote_pty() {
    let fixture = Fixture::new().await;
    fixture.peer.backend.lock().unwrap().herdr.ticks = 45;
    let session = Session::discover_herdr(fixture.options.clone(), Default::default(), false);
    phase(&session, DurablePhase::Choosing).await;
    let state = session.durable_state().unwrap();
    assert!(state.herdr_browser);
    assert_eq!(state.candidates.len(), 1);
    let selected = state.candidates[0].reference.clone();
    assert_eq!(selected.name, "t7");
    assert_eq!(selected.backend, w11term::durable::Backend::Herdr);
    session
        .durable_action(DurableAction::Choose(selected.clone()))
        .unwrap();
    phase(&session, DurablePhase::Attached).await;
    session.input.send(Command::SetReadOnly(true)).unwrap();
    assert!(
        raw(&session, "locked")
            .await
            .unwrap_err()
            .contains("read-only")
    );
    let (peer, channel) = fixture
        .peer
        .backend
        .lock()
        .unwrap()
        .herdr
        .viewer
        .clone()
        .unwrap();
    peer.data(channel, frame(19, 0, b"\x1b[c")).await.unwrap();
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if fixture
                .peer
                .backend
                .lock()
                .unwrap()
                .herdr
                .automatic
                .ends_with(b"c")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("locked Herdr pane must still send automatic device attributes");
    session.input.send(Command::SetReadOnly(false)).unwrap();
    raw(&session, "live-typed").await.unwrap();
    session
        .input
        .send(Command::Resize(wezterm_term::TerminalSize {
            cols: 98,
            rows: 36,
            ..Default::default()
        }))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if screen(&session, "live-typed")
                && fixture
                    .peer
                    .backend
                    .lock()
                    .unwrap()
                    .herdr
                    .grids
                    .contains(&vec![98, 0, 36, 0])
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    session.durable_action(DurableAction::Detach).unwrap();
    phase(&session, DurablePhase::Detached).await;
    assert!(raw(&session, "never").await.is_err());
    // Same core, record address, creation date and recycled PID, but a different process.
    fixture.peer.backend.lock().unwrap().herdr.ticks += 1;
    session.durable_action(DurableAction::Reattach).unwrap();
    phase(&session, DurablePhase::Failed).await;
    let state = session.durable_state().unwrap();
    assert!(
        state.detail.contains("generation changed"),
        "{}",
        state.detail
    );
    assert!(
        !state.permits(DurableAction::UseDirectShell) && !state.permits(DurableAction::StartNew)
    );
    let mut other_core = selected;
    other_core.generation = other_core.generation.replacen(CORE, &"f".repeat(32), 1);
    let restored = fixture.open(Some(other_core));
    phase(&restored, DurablePhase::Failed).await;
    assert!(
        restored
            .durable_state()
            .unwrap()
            .detail
            .contains("Core identity changed")
    );
    assert!(raw(&restored, "never").await.is_err());
    let backend = fixture.peer.backend.lock().unwrap();
    assert_eq!(backend.herdr.attaches, 1);
    assert_eq!(backend.herdr.inputs, [b"live-typed".as_slice()]);
    assert!(backend.terminal_types.is_empty() && backend.environments.is_empty());
    assert!(
        backend
            .commands
            .iter()
            .all(|c| c == "hc connect" || c.starts_with("sh -c "))
    );
}

#[cfg(feature = "ui-smoke")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opens the native Herdr browser against an isolated loopback SSH account"]
async fn native_browse_existing_herdr_terminal() {
    let fixture = Fixture::new().await;
    fixture.peer.backend.lock().unwrap().herdr.ticks = 32;
    let dir = tempfile::tempdir().unwrap();
    gui::run_gui(
        &Source::Ssh(fixture.options.clone()),
        dir.path(),
        &[("W11TERM_DISCOVERY_SMOKE", "herdr")],
        "Host discovery smoke passed",
    )
    .await;
    let backend = fixture.peer.backend.lock().unwrap();
    assert_eq!(backend.herdr.attaches, 1);
    assert!(
        !backend
            .commands
            .iter()
            .any(|c| c.contains("tmux ") || c.contains("zmx "))
    );
}
