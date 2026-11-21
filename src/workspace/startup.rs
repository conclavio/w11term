use crate::options::SshOptions;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// Only a locally authored workspace recipe can request remote execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshStartup {
    host: String,
    port: u16,
    user: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub directory: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
}

impl SshStartup {
    pub fn new(options: &SshOptions, directory: String, command: String) -> Result<Self> {
        let startup = Self {
            host: options.host.clone(),
            port: options.port,
            user: options.user.clone(),
            directory,
            command,
        };
        startup.check_target(options)?;
        Ok(startup)
    }

    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            !self.host.is_empty() && self.port != 0 && !self.user.is_empty(),
            "Missing workspace startup destination"
        );
        ensure!(
            !self.directory.is_empty() || !self.command.trim().is_empty(),
            "Enter a starting directory or a command"
        );
        ensure!(
            self.directory.is_empty() || self.directory.starts_with('/'),
            "Enter an absolute remote directory"
        );
        ensure!(
            !self.directory.contains('\0') && !self.command.contains('\0'),
            "Startup values cannot contain NUL"
        );
        Ok(())
    }

    pub(crate) fn check_target(&self, options: &SshOptions) -> Result<()> {
        self.validate()?;
        ensure!(
            self.host.eq_ignore_ascii_case(&options.host)
                && self.port == options.port
                && self.user == options.user,
            "Workspace startup destination has changed. Reconfigure the pane for the new SSH account."
        );
        ensure!(
            !options.durable && !options.player.rustmush,
            "Workspace startup requires an ordinary SSH shell profile"
        );
        Ok(())
    }

    pub(crate) fn exec_request(&self) -> String {
        if self.directory.is_empty() {
            return self.command.clone();
        }
        // Pass the directory as a positional argument so whitespace, quotes
        // and shell metacharacters remain literal even when launching a script.
        fn quote(value: &str) -> String {
            format!("'{}'", value.replace('\'', "'\\''"))
        }
        let dir = quote(&self.directory);
        if self.command.is_empty() {
            format!(r#"sh -c 'cd "$1" && exec "${{SHELL:-/bin/sh}}" -i' w11term {dir}"#)
        } else {
            format!(
                r#"sh -c 'cd "$1" && exec "${{SHELL:-/bin/sh}}" -c "$2"' w11term {dir} {}"#,
                quote(&self.command)
            )
        }
    }
}
