use std::net::Ipv6Addr;
use w11term::options::SshOptions;

pub(super) fn display(user: &str, host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("{user}@[{host}]:{port}")
    } else {
        format!("{user}@{host}:{port}")
    }
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Endpoint {
    pub user: String,
    pub host: String,
    pub port: u16,
}

impl From<&SshOptions> for Endpoint {
    fn from(ssh: &SshOptions) -> Self {
        let host = ssh
            .host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(&ssh.host);
        let (ip, scope) = host.split_once('%').unwrap_or((host, ""));
        let host = match ip.parse::<Ipv6Addr>() {
            Ok(ip) if !scope.is_empty() => format!("{ip}%{scope}"),
            Ok(ip) => ip.to_string(),
            Err(_) => host.to_ascii_lowercase(),
        };
        Self {
            user: ssh.user.clone(),
            host,
            port: ssh.port,
        }
    }
}
