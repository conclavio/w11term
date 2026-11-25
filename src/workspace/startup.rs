use crate::options::SshOptions;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

/// Only a locally authored workspace recipe can supply a command. A directory
/// split supplies a working directory and starts the interactive shell there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshStartup {
    host: String,
    #[serde(
        default,
        deserialize_with = "crate::options::deserialize_host_key_alias",
        skip_serializing_if = "Option::is_none"
    )]
    host_key_alias: Option<String>,
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
            host_key_alias: options.host_key_alias.clone(),
            port: options.port,
            user: options.user.clone(),
            directory,
            command,
        };
        startup.check_target(options)?;
        Ok(startup)
    }

    pub(super) fn validate(&self) -> Result<()> {
        crate::options::validate_ssh_host(&self.host)?;
        if let Some(alias) = &self.host_key_alias {
            crate::options::validate_host_key_alias(alias)?;
        }
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
            !self.directory.chars().any(char::is_control),
            "Startup directory cannot contain control characters"
        );
        ensure!(
            !self.command.contains('\0'),
            "Startup values cannot contain NUL"
        );
        Ok(())
    }

    pub fn check_target(&self, options: &SshOptions) -> Result<()> {
        self.validate()?;
        ensure!(
            self.host.eq_ignore_ascii_case(&options.host)
                && self.host_key_alias == options.host_key_alias
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

    pub fn exec_request(&self) -> String {
        if self.directory.is_empty() {
            return self.command.clone();
        }
        // Encode arguments as pure octal digits ([0-7]+) so the remote login
        // shell (POSIX sh, bash, zsh, fish, csh, pwsh, or cmd.exe) never sees
        // quotes, backslashes, or shell metacharacters in untrusted arguments.
        // The fixed POSIX `sh -c` decoder contains neither embedded single
        // quotes nor `\\`, so it is parsed identically by POSIX sh and fish.
        fn encode_octal(value: &str) -> String {
            use std::fmt::Write;
            let mut out = String::with_capacity(value.len() * 3);
            for byte in value.as_bytes() {
                let _ = write!(out, "{byte:03o}");
            }
            out
        }
        const DECODER: &str = r#"d(){ h=$1;o=;while [ -n "$h" ];do r=${h#???};o="$o\0${h%"$r"}";h=$r;done;printf %bx "$o"; };"#;
        let dir = encode_octal(&self.directory);
        if self.command.is_empty() {
            format!(
                r#"sh -c '{DECODER}a=$(d "$1");a=${{a%x}};cd -- "$a" && exec "${{SHELL:-/bin/sh}}" -i' w11term {dir}"#
            )
        } else {
            let cmd = encode_octal(&self.command);
            format!(
                r#"sh -c '{DECODER}a=$(d "$1");a=${{a%x}};b=$(d "$2");b=${{b%x}};cd -- "$a" && exec "${{SHELL:-/bin/sh}}" -c "$b"' w11term {dir} {cmd}"#
            )
        }
    }
}
