use super::*;

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
                    "exit_code": entry.exit_code,
                    "output_available": entry.output.is_ok(),
                })
            })
            .collect();
        Ok(json!({
            "pane": pane,
            "connection_generation": frame.connection_generation,
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
        ensure!(
            frame.connection_generation == connection_generation,
            "pane {pane} connection generation changed (requested {connection_generation}, current {})",
            frame.connection_generation
        );
        let entry = frame
            .command_history
            .iter()
            .find(|entry| entry.id == id)
            .with_context(|| format!("command {id} is not retained in pane {pane}"))?;
        Ok(json!({
            "pane": pane,
            "connection_generation": connection_generation,
            "id": entry.id,
            "command": entry.command,
            "exit_code": entry.exit_code,
            "output": entry.output.as_ref().ok(),
            "output_error": entry.output.as_ref().err(),
        }))
    }
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
            label: "background pane".into(),
            read_only: true,
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
            "\x1b]133;B\x07echo 雪\n\x1b]133;C\x07  雪 \"x\"\n\n  end\x1b]133;D;7\x07",
            "\x1b]133;C\x07\x1b]133;D\x07", // available empty output and unknown exit code
            "\x1b]133;D;1\x07",             // no C marker for this output
        ));
        let target = registry.pane(23).unwrap();
        let before = Registry::capture(&target).await.unwrap().0;
        let list = execute(&registry, json!({"verb":"list-commands","pane":23}))
            .await
            .unwrap();
        let generation = before.connection_generation;
        assert_eq!(
            list,
            json!({
                "pane":23,"connection_generation":generation,
                "commands":[
                    {"id":3,"command":null,"exit_code":1,"output_available":false},
                    {"id":2,"command":null,"exit_code":null,"output_available":true},
                    {"id":1,"command":"echo 雪","exit_code":7,"output_available":true}
                ]
            })
        );
        for (id, command, code, output, error) in [
            (
                1,
                Some("echo 雪"),
                Some(7),
                Some("  雪 \"x\"\r\n\r\n  end"),
                None,
            ),
            (2, None, None, Some(""), None),
            (3, None, Some(1), None, Some("No command output marker")),
        ] {
            let result = execute(
                &registry,
                json!({"verb":"get-command","pane":23,"id":id,"connection_generation":generation}),
            )
            .await
            .unwrap();
            assert_eq!(
                result,
                json!({"pane":23,"id":id,"connection_generation":generation,
                "command":command,"exit_code":code,"output":output,"output_error":error})
            );
        }
        let after = Registry::capture(&target).await.unwrap().0;
        assert_eq!(before.generation, after.generation);
        assert!(before.same_presentation(&after));
        let pane = registry.pane(23).unwrap();
        assert!(pane.read_only && !pane.active && !pane.visible);
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
