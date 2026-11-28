use super::{seconds_text, timeline};
use std::fmt::Write as _;
use w11term::ssh::{
    PacketCaptureComparison, PacketConversationChange, PacketConversationEndpoints,
    ip_protocol_label,
};

const MAX_PROTOCOL_CHANGES: usize = 8;

pub(super) fn preview(
    model: &PacketCaptureComparison,
    selected: Option<&PacketConversationChange>,
) -> String {
    let mut text = String::from("Saved packet capture comparison\n");
    if let Some(window) = &model.window {
        let interval = timeline::interval_label(window.start_offset_ns..window.end_offset_ns);
        let _ = writeln!(text, "Scope · Aligned elapsed interval {interval}");
        for (label, side) in [("Before", &window.before), ("After", &window.after)] {
            let _ = writeln!(
                text,
                "{label} · {} · {} source packets without timestamps",
                side.coverage_label(),
                side.untimestamped_packets
            );
        }
    } else {
        text.push_str("Scope · Whole captures\n");
    }

    for (label, side) in [("Before", &model.before), ("After", &model.after)] {
        let scan = if side.complete {
            "Complete scan"
        } else {
            "Partial scan"
        };
        let span = side.timestamp_span_ns.map_or_else(
            || "Timestamp span unavailable".into(),
            |ns| format!("Timestamp span {} s", seconds_text(ns)),
        );
        let _ = writeln!(
            text,
            "{label} · {} packets · {} captured bytes · {} original bytes · {} truncated",
            side.packets, side.captured_bytes, side.original_bytes, side.truncated_packets,
        );
        let _ = writeln!(
            text,
            "{label} coverage · {scan} · {span} · {} timestamped · {} unclassified · {} untracked IP · {} without ports · {} untracked transport",
            side.timestamped_packets,
            side.unclassified_packets,
            side.untracked_packets,
            side.packets_without_ports,
            side.transport_untracked_packets,
        );
    }
    if let Some(delta) = &model.totals_delta {
        let _ = writeln!(
            text,
            "Overall change · {:+} packets · {:+} captured bytes · {:+} original bytes · {:+} truncated",
            delta.packets, delta.captured_bytes, delta.original_bytes, delta.truncated_packets
        );
    } else {
        text.push_str("Overall change · Unavailable for this coverage\n");
    }
    append_lines(&mut text, model.average_rates.report_lines());

    if let Some(change) = selected {
        let label = match change.endpoints {
            PacketConversationEndpoints::Ip { .. } => {
                "Selected IP pair · Endpoint A and Endpoint B".into()
            }
            PacketConversationEndpoints::Transport { protocol, a, b } => format!(
                "Selected {} pair · Endpoint A port {} · Endpoint B port {}",
                ip_protocol_label(protocol),
                a.port(),
                b.port()
            ),
        };
        let _ = writeln!(text, "\n{label}");
        for (side, observation) in [("Before", change.before), ("After", change.after)] {
            if let Some(observation) = observation {
                let a = observation.a_to_b;
                let b = observation.b_to_a;
                let _ = writeln!(
                    text,
                    "{side} · A to B {} packets, {} captured bytes · B to A {} packets, {} captured bytes",
                    a.packets, a.captured_bytes, b.packets, b.captured_bytes
                );
            } else {
                let _ = writeln!(text, "{side} · Pair observation unavailable");
            }
        }
        if let Some(rates) = change.average_rates {
            append_lines(&mut text, rates.report_lines());
        }
    }

    if !model.protocol_changes.is_empty() {
        text.push_str("\nLargest recorded IP protocol changes\n");
        append_lines(
            &mut text,
            model
                .protocol_changes
                .iter()
                .take(MAX_PROTOCOL_CHANGES)
                .map(|change| change.summary()),
        );
    }
    if let Some(flags) = &model.tcp_flag_comparison {
        text.push_str("\nTCP flags across the compared captures\n");
        append_lines(&mut text, flags.report_lines());
    }
    text
}

fn append_lines(text: &mut String, lines: impl IntoIterator<Item = String>) {
    for line in lines {
        let _ = writeln!(text, "{line}");
    }
}
