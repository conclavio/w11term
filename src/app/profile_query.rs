use super::{SshOptions, host_groups};

pub(super) struct Query(String);

impl Query {
    pub(super) fn new(value: &str) -> Self {
        Self(value.trim().to_lowercase())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    // Smaller ranks put an exact name first, then name prefixes. Equal ranks keep catalog order.
    pub(super) fn rank(&self, name: &str, details: &str) -> Option<(bool, bool)> {
        if self.is_empty() {
            return Some((true, false));
        }
        let name = name.to_lowercase();
        let details = details.to_lowercase();
        self.0
            .split_whitespace()
            .all(|word| name.contains(word) || details.contains(word))
            .then(|| (name != self.0, !name.starts_with(&self.0)))
    }
}

pub(super) fn address(ssh: &SshOptions) -> String {
    host_groups::ssh_address(&ssh.user, &ssh.host, ssh.port)
}
