use super::super::includes::{load, parse_test};
use super::super::{ImportReport, ImportedAlias};
use std::{fs, path::Path};

fn parse(text: &str) -> ImportReport {
    parse_test(text, Path::new(r"C:\Users\pilot"), "pilot").unwrap()
}

fn alias<'a>(report: &'a ImportReport, name: &str) -> &'a ImportedAlias {
    report.aliases.iter().find(|a| a.name == name).unwrap()
}

fn ports(alias: &ImportedAlias) -> Vec<(u16, u16)> {
    alias
        .options
        .as_ref()
        .unwrap()
        .local_forwards
        .iter()
        .map(|f| (f.local_port.get(), f.remote_port.get()))
        .collect()
}

#[test]
fn includes_accumulate_in_order_deduplicate_exact_pairs_and_isolate_jump_settings() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    fs::create_dir(home.join(".ssh")).unwrap();
    fs::write(home.join(".ssh/aliases"), "Host db staging\nLocalForward 8001 localhost:81\nHost db* !db-old\nLocalForward 127.0.0.1:8001 127.0.0.1:82\nHost bastion\nLocalForward 9000 localhost:90\nLocalForward *:7777 elsewhere:77").unwrap();
    let path = home.join("config");
    fs::write(&path, "LocalForward 8000 localhost:80\nInclude aliases\nHost db\nProxyJump bastion\nLocalForward LOCALHOST:8001 127.0.0.1:81\nLocalForward 65535 127.0.0.1:1\nHost db-old\n").unwrap();
    let report = load(&path, home, "pilot").unwrap();
    let db = alias(&report, "db");
    assert_eq!(ports(db), [(8000, 80), (8001, 81), (8001, 82), (65535, 1)]);
    assert!(db.warnings.is_empty(), "{:?}", db.warnings);
    assert_eq!(db.options.as_ref().unwrap().proxy_jump[0].host, "bastion");
    assert_eq!(ports(alias(&report, "db-old")), [(8000, 80)]);
    assert_eq!(ports(alias(&report, "staging")), [(8000, 80), (8001, 81)]);
    assert_eq!(ports(alias(&report, "bastion")), [(8000, 80), (9000, 90)]);
    assert!(alias(&report, "bastion").warnings[0].contains("aliases:7"));
}

#[test]
fn clear_all_and_gateway_ports_use_first_obtained_value_regardless_of_rule_order() {
    let report = parse(
        "Host open closed cleared\nLocalForward 8000 localhost:80\nLocalForward localhost:8001 localhost:81\nHost open\nGatewayPorts yes\nHost closed\nGatewayPorts no\nHost cleared\nClearAllForwardings yes\nHost *\nGatewayPorts yes\nClearAllForwardings no\nLocalForward 127.0.0.1:8002 localhost:82",
    );
    assert_eq!(ports(alias(&report, "open")), [(8001, 81), (8002, 82)]);
    assert!(alias(&report, "open").warnings[0].contains("GatewayPorts"));
    assert_eq!(
        ports(alias(&report, "closed")),
        [(8000, 80), (8001, 81), (8002, 82)]
    );
    assert!(alias(&report, "closed").warnings.is_empty());
    assert!(ports(alias(&report, "cleared")).is_empty());
    assert!(alias(&report, "cleared").warnings[0].contains("ClearAllForwardings yes"));
}

#[test]
fn conditional_overrides_only_exclude_possibly_affected_presets() {
    let report = parse(
        "Host db stable plain conditional\nLocalForward 8000 localhost:80\nLocalForward 127.0.0.1:8001 localhost:81\nHost stable\nGatewayPorts no\nClearAllForwardings no\nHost db\nHostName db.internal\nMatch host db.internal exec \"return unknown\"\nGatewayPorts yes\nMatch originalhost conditional exec unknown\nClearAllForwardings yes\nMatch originalhost=stable exec unknown\nGatewayPorts yes\nClearAllForwardings yes\nLocalForward 8003 localhost:83\nMatch originalhost *,!plain\nLocalForward 127.0.0.1:8002 localhost:82",
    );
    let db = alias(&report, "db");
    assert_eq!(ports(db), [(8001, 81), (8002, 82)]);
    assert_eq!(db.warnings.len(), 1);
    assert!(db.warnings[0].contains("GatewayPorts"));
    let stable = alias(&report, "stable");
    assert_eq!(ports(stable), [(8000, 80), (8001, 81), (8002, 82)]);
    assert_eq!(stable.warnings.len(), 1);
    assert!(stable.warnings[0].contains("Match condition"));
    assert_eq!(ports(alias(&report, "plain")), [(8000, 80), (8001, 81)]);
    assert!(alias(&report, "plain").warnings.is_empty());
    assert!(ports(alias(&report, "conditional")).is_empty());
    assert!(alias(&report, "conditional").warnings[0].contains("ClearAllForwardings"));
}

#[test]
fn address_family_checks_apply_to_each_listener_and_respect_host_and_match_precedence() {
    let report = parse(
        r#"Host ipv4 dual ipv6 conditional default
    LocalForward 8000 localhost:80
    LocalForward 127.0.0.1:8001 127.0.0.1:81
Host ipv4
    AddressFamily inet
Host dual
    AddressFamily any
Match originalhost ipv4,dual,conditional exec unknown
    AddressFamily inet6
Host ipv6
    AddressFamily inet6
Host *
    AddressFamily any"#,
    );
    for name in ["ipv4", "dual", "default"] {
        let entry = alias(&report, name);
        assert_eq!(ports(entry), [(8000, 80), (8001, 81)]);
        assert!(!entry.warnings.iter().any(|w| w.contains("was skipped")));
    }
    for (name, line) in [("ipv6", 11), ("conditional", 9)] {
        let entry = alias(&report, name);
        assert!(ports(entry).is_empty());
        let skipped: Vec<_> = entry
            .warnings
            .iter()
            .filter(|w| w.contains("was skipped"))
            .collect();
        assert_eq!(skipped.len(), 2);
        for (index, warning) in skipped.iter().enumerate() {
            assert!(
                warning.contains(&format!("config:{}: LocalForward", index + 2)),
                "{warning}"
            );
            assert!(
                warning.contains(&format!("AddressFamily inet6 at config:{line}")),
                "{warning}"
            );
        }
    }
}

#[test]
fn a_never_matching_clear_does_not_cancel_earlier_presets() {
    let report =
        parse("Host lab\nLocalForward 8080 localhost:80\nMatch !all\nClearAllForwardings yes");
    let lab = alias(&report, "lab");
    assert_eq!(ports(lab), [(8080, 80)]);
    assert!(lab.warnings.is_empty());
}

#[test]
fn unsupported_addresses_ports_and_forwarding_kinds_report_original_rule_with_location() {
    let rules = [
        "LocalForward *:8000 localhost:80",
        "LocalForward :8000 localhost:80",
        "LocalForward [::1]:8000 localhost:80",
        "LocalForward 8000 [::1]:80",
        "LocalForward 127.0.0.2:8000 localhost:80",
        "LocalForward 8000 db.example:80",
        "LocalForward /tmp/in localhost:80",
        "LocalForward 8000 /tmp/out",
        "LocalForward 0 localhost:80",
        "LocalForward 65536 localhost:80",
        "LocalForward 8000 localhost:+80",
        "LocalForward 8000 localhost:0",
        "LocalForward 8000",
        "LocalForward 8000 localhost:80 extra",
        "RemoteForward 8000 localhost:80",
        "DynamicForward 8000",
    ];
    let report = parse(&format!(
        "Host lab\n{}\nLocalForward 12 127.0.0.1:34",
        rules.join("\n")
    ));
    let lab = alias(&report, "lab");
    assert_eq!(ports(lab), [(12, 34)]);
    assert_eq!(lab.warnings.len(), rules.len());
    for (i, rule) in rules.iter().enumerate() {
        assert!(
            lab.warnings
                .iter()
                .any(|w| w.contains(&format!("config:{}: {rule} was skipped", i + 2))),
            "{rule}: {:?}",
            lab.warnings
        );
    }
}
