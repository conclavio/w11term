use super::*;
use wezterm_term::CompletedCommand;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommandStatus {
    #[default]
    All,
    Failed,
    Succeeded,
    Unknown,
}

impl CommandStatus {
    pub fn matches(self, exit_code: Option<i32>) -> bool {
        match self {
            Self::All => true,
            Self::Failed => exit_code.is_some_and(|code| code != 0),
            Self::Succeeded => exit_code == Some(0),
            Self::Unknown => exit_code.is_none(),
        }
    }

    pub(super) fn is_all(&self) -> bool {
        *self == Self::All
    }
}

impl Registry {
    pub(super) async fn list_commands(&self, pane: u64, status: CommandStatus) -> Result<Value> {
        let (frame, _) = Self::capture(&self.pane(pane)?).await?;
        let commands: Vec<_> = frame
            .command_history
            .iter()
            .rev()
            .filter(|entry| status.matches(entry.exit_code))
            .map(|entry| {
                json!({
                    "id": entry.id,
                    "command": entry.command,
                    "reported_directory": entry.reported_directory.as_deref(),
                    "started_unix_ms": entry.timing.started_unix_ms,
                    "finished_unix_ms": entry.timing.finished_unix_ms,
                    "duration_ms": entry.timing.duration_ms,
                    "exit_code": entry.exit_code,
                    "output_available": entry.output.is_ok(),
                })
            })
            .collect();
        Ok(json!({
            "pane": pane,
            "connection_generation": frame.connection_generation,
            "latest_id": frame.latest_command_id,
            "commands": commands,
        }))
    }

    pub(super) async fn get_command(
        &self,
        pane: u64,
        id: u64,
        connection_generation: u64,
    ) -> Result<Value> {
        // Resolve the connection and record against the same committed frame.
        let (frame, _) = Self::capture(&self.pane(pane)?).await?;
        check_generation(&frame, pane, connection_generation)?;
        let entry = frame
            .command_history
            .iter()
            .find(|entry| entry.id == id)
            .with_context(|| format!("command {id} is not retained in pane {pane}"))?;
        Ok(command_detail(pane, connection_generation, entry))
    }

    pub(super) async fn wait_command(
        &self,
        pane: u64,
        after_id: u64,
        connection_generation: u64,
        timeout_seconds: u64,
    ) -> Result<Value> {
        ensure!(
            (1..=3600).contains(&timeout_seconds),
            "wait timeout must be 1..=3600 seconds"
        );
        let target = self.pane(pane)?;
        let events = if connection_generation == 0 {
            None
        } else {
            Some(Self::pane_events(&target).await?)
        };
        let frames = Self::live_frames(&target).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_seconds);
        let wait = async {
            loop {
                let frame = frames.load();
                check_generation(&frame, pane, connection_generation)?;
                if let Some(entry) = next_command(&frame, after_id)? {
                    return Ok::<_, anyhow::Error>(Arc::clone(entry));
                }
                ensure!(
                    tokio::time::Instant::now() < deadline
                        && frames.changed(frame.generation, deadline).await,
                    "wait-command timed out without a new completed command"
                );
            }
        };
        const STALE: &str =
            "The pane connection changed or is no longer active; no command was accepted";
        let entry = tokio::select! {
            biased;
            _ = async {
                match &events {
                    Some(events) => events.terminal_connection_changed(connection_generation).await,
                    None => std::future::pending().await,
                }
            } => anyhow::bail!(STALE),
            _ = target.input.closed() => anyhow::bail!("pane is closed"),
            result = wait => result?,
        };
        ensure!(
            events.is_none_or(|events| events.terminal_connection_is(connection_generation)),
            STALE
        );
        Ok(command_detail(pane, connection_generation, &entry))
    }
}

fn check_generation(frame: &Snapshot, pane: u64, expected: u64) -> Result<()> {
    ensure!(
        frame.connection_generation == expected,
        "pane {pane} connection generation changed (requested {expected}, current {})",
        frame.connection_generation
    );
    Ok(())
}

fn next_command(frame: &Snapshot, after_id: u64) -> Result<Option<&Arc<CompletedCommand>>> {
    ensure!(
        after_id <= frame.latest_command_id,
        "command ID is ahead of this pane; check the instance and pane ID"
    );
    if after_id == frame.latest_command_id {
        return Ok(None);
    }
    let next = frame
        .command_history
        .iter()
        .find(|entry| entry.id > after_id);
    ensure!(
        next.is_some_and(|entry| entry.id == after_id + 1),
        "command history is no longer retained immediately after ID {after_id}; get a new cursor with list-commands"
    );
    Ok(next)
}

fn command_detail(pane: u64, connection_generation: u64, entry: &CompletedCommand) -> Value {
    json!({
        "pane": pane,
        "connection_generation": connection_generation,
        "id": entry.id,
        "command": entry.command,
        "reported_directory": entry.reported_directory.as_deref(),
        "started_unix_ms": entry.timing.started_unix_ms,
        "finished_unix_ms": entry.timing.finished_unix_ms,
        "duration_ms": entry.timing.duration_ms,
        "exit_code": entry.exit_code,
        "output": entry.output.as_ref().ok(),
        "output_error": entry.output.as_ref().err(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{options::Source, session::Session};

    fn pane(bytes: &str) -> (Registry, Session) {
        let session = Session::start_sized(
            Source::Demo,
            TerminalSize {
                cols: 24,
                rows: 3,
                ..Default::default()
            },
        );
        session.input.send(Command::Paste(bytes.into())).unwrap();
        session.input.send(Command::SetReadOnly(true)).unwrap();
        let registry = Registry::default();
        registry.replace([Pane {
            id: 23,
            tab_id: 31,
            tab_title: None,
            pane_name: None,
            label: "background pane".into(),
            read_only: true,
            capture: None,
            active: false,
            visible: false,
            input: session.input.clone(),
        }]);
        (registry, session)
    }

    async fn execute(registry: &Registry, request: Value) -> Result<Value> {
        registry
            .execute(
                serde_json::from_value(request)?,
                Arc::new(|_, _, _| unreachable!()),
            )
            .await
    }

    #[tokio::test]
    async fn status_filters_use_reported_codes_and_preserve_order_and_metadata() {
        let (registry, _session) = pane(concat!(
            "\x1b]133;D;1\x07",
            "\x1b]133;D\x07",
            "\x1b]133;D;0\x07",
            "\x1b]133;D;9\x07",
        ));
        let legacy = json!({"verb":"list-commands","pane":23});
        let request: Request = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(request).unwrap(), legacy);
        let all = execute(&registry, legacy).await.unwrap();
        for (status, ids) in [
            ("all", vec![4, 3, 2, 1]),
            ("failed", vec![4, 1]),
            ("succeeded", vec![3]),
            ("unknown", vec![2]),
        ] {
            let list = execute(
                &registry,
                json!({"verb":"list-commands","pane":23,"status":status}),
            )
            .await
            .unwrap();
            assert_eq!(list["pane"], all["pane"]);
            assert_eq!(list["connection_generation"], all["connection_generation"]);
            assert_eq!(list["latest_id"], 4);
            let entries = all["commands"].as_array().unwrap();
            let expected: Vec<_> = ids.into_iter().map(|id: usize| &entries[4 - id]).collect();
            assert_eq!(list["commands"], json!(expected));
        }
        assert!(CommandStatus::Failed.matches(Some(-1)));
        assert!(
            execute(
                &registry,
                json!({"verb":"list-commands","pane":23,"status":"typo"})
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn metadata_only_listing_and_exact_output_from_read_only_background_pane() {
        let (registry, _session) = pane(concat!(
            "\x1b]7;file://loopback/old%20folder\x07",
            "\x1b]133;B\x07echo 雪\n\x1b]133;C\x07  雪 \"x\"\n\n  end",
            "\x1b]7;file://loopback/new\x07\x1b]133;D;7\x07",
            "\x1b]633;C\x07\x1b]633;D\x07", // available empty output and unknown exit code
            "\x1b]133;D;1\x07",             // no C marker for this output
        ));
        let target = registry.pane(23).unwrap();
        let before = Registry::capture(&target).await.unwrap().0;
        let list = execute(&registry, json!({"verb":"list-commands","pane":23}))
            .await
            .unwrap();
        let generation = before.connection_generation;
        let first = before.command_history[0].timing;
        let second = before.command_history[1].timing;
        let third = before.command_history[2].timing;
        for timing in [first, second] {
            assert!(timing.started_unix_ms.is_some());
            assert!(timing.duration_ms.is_some());
            assert!(timing.finished_unix_ms > 0);
        }
        assert!(third.started_unix_ms.is_none());
        assert!(third.duration_ms.is_none());
        assert!(third.finished_unix_ms > 0);
        assert_eq!(
            list,
            json!({
                "pane":23,"connection_generation":generation,"latest_id":3,
                "commands":[
                    {"id":3,"command":null,"reported_directory":null,"exit_code":1,"output_available":false,
                     "started_unix_ms":null,"finished_unix_ms":third.finished_unix_ms,"duration_ms":null},
                    {"id":2,"command":null,"reported_directory":"file://loopback/new","exit_code":null,"output_available":true,
                     "started_unix_ms":second.started_unix_ms,"finished_unix_ms":second.finished_unix_ms,"duration_ms":second.duration_ms},
                    {"id":1,"command":"echo 雪","reported_directory":"file://loopback/old%20folder","exit_code":7,"output_available":true,
                     "started_unix_ms":first.started_unix_ms,"finished_unix_ms":first.finished_unix_ms,"duration_ms":first.duration_ms}
                ]
            })
        );
        for (id, command, directory, code, output, error) in [
            (
                1,
                Some("echo 雪"),
                Some("file://loopback/old%20folder"),
                Some(7),
                Some("  雪 \"x\"\r\n\r\n  end"),
                None,
            ),
            (2, None, Some("file://loopback/new"), None, Some(""), None),
            (
                3,
                None,
                None,
                Some(1),
                None,
                Some("No command output marker"),
            ),
        ] {
            let timing = [first, second, third][id - 1];
            let result = execute(
                &registry,
                json!({"verb":"get-command","pane":23,"id":id,"connection_generation":generation}),
            )
            .await
            .unwrap();
            assert_eq!(
                result,
                json!({"pane":23,"id":id,"connection_generation":generation,
                "command":command,"reported_directory":directory,"exit_code":code,"output":output,"output_error":error,
                "started_unix_ms":timing.started_unix_ms,"finished_unix_ms":timing.finished_unix_ms,"duration_ms":timing.duration_ms})
            );
        }
        let after = Registry::capture(&target).await.unwrap().0;
        assert_eq!(before.generation, after.generation);
        assert!(before.same_presentation(&after));
        let pane = registry.pane(23).unwrap();
        assert!(pane.read_only && !pane.active && !pane.visible);
    }

    #[tokio::test]
    async fn wait_returns_the_first_new_record_and_detects_an_evicted_cursor_gap() {
        let (registry, _session) = pane(&"\x1b]133;C\x07\x1b]133;D;9\x07".repeat(65));
        let request = |after_id| {
            json!({
                "verb":"wait-command","pane":23,"connection_generation":0,
                "after_id":after_id,"timeout_seconds":1
            })
        };
        let first = execute(&registry, request(1)).await.unwrap();
        assert_eq!(first["id"], 2);
        assert_eq!(first["exit_code"], 9);
        assert_eq!(
            execute(&registry, request(0))
                .await
                .unwrap_err()
                .to_string(),
            "command history is no longer retained immediately after ID 0; get a new cursor with list-commands"
        );
        assert!(
            execute(&registry, request(66))
                .await
                .unwrap_err()
                .to_string()
                .contains("ahead of this pane")
        );
    }

    #[tokio::test]
    async fn reset_keeps_the_latest_id_for_new_waiters() {
        let (registry, session) = pane("\x1b]133;D;0\x07\x1bc");
        let list = execute(&registry, json!({"verb":"list-commands","pane":23}))
            .await
            .unwrap();
        assert_eq!(list["commands"], json!([]));
        assert_eq!(list["latest_id"], 1);
        let request = |after_id| {
            json!({
                "verb":"wait-command","pane":23,"connection_generation":0,
                "after_id":after_id,"timeout_seconds":1
            })
        };
        assert!(
            execute(&registry, request(0))
                .await
                .unwrap_err()
                .to_string()
                .contains("no longer retained")
        );
        session.input.send(Command::SetReadOnly(false)).unwrap();
        session
            .input
            .send(Command::Paste("\x1b]633;C\x07\x1b]633;D;7\x07".into()))
            .unwrap();
        let entry = execute(&registry, request(1)).await.unwrap();
        assert_eq!(entry["id"], 2);
        assert_eq!(entry["exit_code"], 7);
    }

    #[tokio::test]
    async fn only_retained_ids_are_readable_and_a_stale_generation_never_resolves() {
        let (registry, _session) = pane(&"\x1b]133;C\x07\x1b]133;D;0\x07".repeat(65));
        let list = execute(&registry, json!({"verb":"list-commands","pane":23}))
            .await
            .unwrap();
        let commands = list["commands"].as_array().unwrap();
        assert_eq!(commands.len(), 64);
        assert_eq!(commands.first().unwrap()["id"], 65);
        assert_eq!(commands.last().unwrap()["id"], 2);
        let generation = list["connection_generation"].as_u64().unwrap();
        for id in [1, 66] {
            let error = execute(
                &registry,
                json!({"verb":"get-command","pane":23,"id":id,"connection_generation":generation}),
            )
            .await
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("command {id} is not retained"))
            );
        }
        let error = execute(
            &registry,
            json!({"verb":"get-command","pane":23,"id":65,"connection_generation":generation + 1}),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("connection generation changed"));
        let error = execute(&registry, json!({"verb":"get-command","pane":23,"id":65}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("missing field"));
        assert!(
            execute(&registry, json!({"verb":"list-commands","pane":27}))
                .await
                .unwrap_err()
                .to_string()
                .contains("unknown pane 27")
        );
    }
}
