//! Stock tmux on the SSH account's default socket. Only parsed integers ever
//! enter a command. Session names are display text, and may change at any time.
use std::collections::BTreeSet;

pub(crate) const LIST: &str = "env LC_ALL=C tmux -N list-sessions -F '#{session_id}\t#{pid}\t#{session_created}\t#{session_windows}\t#{session_attached}\t#{session_name}'";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    id: u32,
    server: u32,
    created: u64,
}

fn decimal<T: std::str::FromStr>(value: &str) -> Option<T> {
    value
        .bytes()
        .all(|b| b.is_ascii_digit())
        .then(|| value.parse().ok())?
}

impl Target {
    pub(crate) fn parse(name: &str, generation: &str) -> Option<Self> {
        let (server, created) = generation.split_once('/')?;
        let target = Self {
            id: decimal(name.strip_prefix('$')?)?,
            server: decimal(server)?,
            created: decimal(created)?,
        };
        (target.server != 0
            && target.created != 0
            && target.name() == name
            && target.generation() == generation)
            .then_some(target)
    }
    pub(crate) fn name(self) -> String {
        format!("${}", self.id)
    }
    pub(crate) fn generation(self) -> String {
        format!("{}/{}", self.server, self.created)
    }
    pub(crate) fn attach(self) -> String {
        // if-shell -F evaluates within the same tmux server that performs the
        // attach. IDs can be reused after a restart, and names may resemble IDs.
        // The shell exec preserves its PID so startup can verify *this* client.
        format!(
            r#"sh -c 'echo "$$"; exec "$@"' w11term tmux -N if-shell -F -t '${id}:' '#{{==:#{{pid}}/#{{session_created}}/#{{session_id}},{server}/{created}/${id}}}' 'attach-session -t \${id}' 'display-message -p "Selected tmux session is gone"'"#,
            id = self.id,
            server = self.server,
            created = self.created
        )
    }
    pub(crate) fn clients(self) -> String {
        format!(
            "tmux -N list-clients -t '${}' -F '#{{pid}}/#{{session_created}}/#{{session_id}}/#{{client_pid}}'",
            self.id
        )
    }
    pub(crate) fn attached(self, client: u32, listing: &[u8]) -> bool {
        let expected = format!("{}/{}/{client}", self.generation(), self.name());
        listing
            .split(|b| *b == b'\n')
            .any(|row| row == expected.as_bytes())
    }
}

pub(crate) struct Entry {
    pub target: Target,
    pub name: String,
    pub windows: u32,
    pub clients: u32,
}

pub(crate) fn parse_listing(output: &[u8]) -> Result<Vec<Entry>, &'static str> {
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for row in output.split(|b| *b == b'\n').filter(|row| !row.is_empty()) {
        let row = String::from_utf8_lossy(row);
        let fields = row.splitn(6, '\t').collect::<Vec<_>>();
        let [id, server, created, windows, clients, name] = fields[..] else {
            return Err("Invalid tmux session listing");
        };
        let target = Target::parse(id, &format!("{server}/{created}"))
            .ok_or("Invalid tmux session ID or generation")?;
        if !seen.insert(target.id) {
            return Err("Ambiguous tmux listing: duplicate session ID");
        }
        entries.push(Entry {
            target,
            name: name
                .chars()
                .filter(|c| {
                    !c.is_control()
                        && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                })
                .take(200)
                .collect(),
            windows: decimal(windows).ok_or("Invalid tmux window count")?,
            clients: decimal(clients).ok_or("Invalid tmux client count")?,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_numeric_and_name_is_only_for_display() {
        let data = b"$3\t17\t1234\t4\t2\tops '$HOME'\t\x1b\n";
        let entries = parse_listing(data).unwrap();
        let entry = &entries[0];
        assert_eq!(entry.name, "ops '$HOME'");
        assert_eq!((entry.windows, entry.clients), (4, 2));
        assert!(!entry.target.attach().contains("HOME"));
        assert!(entry.target.attached(55, b"17/1234/$3/42\n17/1234/$3/55\n"));
        for wrong in [
            "17/1234/$3/555",
            "18/1234/$3/55",
            "17/1235/$3/55",
            "17/1234/$4/55",
        ] {
            assert!(!entry.target.attached(55, wrong.as_bytes()));
        }
        assert!(parse_listing(&[data.as_slice(), data.as_slice()].concat()).is_err());
        assert!(parse_listing(b"$1\t2\t5\t1\t0\n").is_err());
        assert!(parse_listing(b"").unwrap().is_empty());
    }

    #[test]
    fn untrusted_workspace_values_cannot_enter_a_command() {
        for (name, generation) in [
            ("$0;exit", "5/6"),
            ("$+5", "5/6"),
            ("$05", "5/6"),
            ("$4294967296", "5/6"),
            ("$1", "5/6\ncommand"),
            ("$1", "5/$HOME"),
            ("$1", "0/1"),
            ("$1", "5/0"),
        ] {
            assert!(Target::parse(name, generation).is_none());
        }
    }
}
