use termwiz::escape::{
    Action, CSI,
    csi::{Device, Edit, Window, WindowTitleScope},
    parser::Parser,
};

fn parse_fragmented(wire: &str) -> Vec<Action> {
    let mut parser = Parser::new();
    wire.as_bytes()
        .chunks(1)
        .flat_map(|byte| parser.parse_as_vec(byte))
        .collect()
}

#[test]
fn title_save_and_restore_parse_fragmented_and_encode_canonical_wire() {
    use WindowTitleScope::{Both, Icon};
    for (wire, expected, encoded) in [
        ("22t", Window::PushIconAndWindowTitle, "22;0t"),
        ("23t", Window::PopIconAndWindowTitle, "23;0t"),
        ("22;0;0t", Window::PushIconAndWindowTitle, "22;0t"),
        ("23;0;0t", Window::PopIconAndWindowTitle, "23;0t"),
        ("22;;t", Window::PushIconAndWindowTitle, "22;0t"),
        ("23;;0t", Window::PopIconAndWindowTitle, "23;0t"),
        ("22;1;t", Window::PushIconTitle, "22;1t"),
        ("23;1t", Window::PopIconTitle, "23;1t"),
        ("22;2;0t", Window::PushWindowTitle, "22;2t"),
        ("23;2;t", Window::PopWindowTitle, "23;2t"),
        (
            "22;;1t",
            Window::SaveTitleToSlot {
                scope: Both,
                slot: 1,
            },
            "22;0;1t",
        ),
        (
            "23;0;10t",
            Window::RestoreTitleFromSlot {
                scope: Both,
                slot: 10,
            },
            "23;0;10t",
        ),
        (
            "22;1;2t",
            Window::SaveTitleToSlot {
                scope: Icon,
                slot: 2,
            },
            "22;1;2t",
        ),
        (
            "23;2;9t",
            Window::RestoreTitleFromSlot {
                scope: WindowTitleScope::Window,
                slot: 9,
            },
            "23;2;9t",
        ),
        ("#S", Window::ReportTitleStackPosition, "#S"),
        ("14;2t", Window::ReportWindowSizePixels, "14;2t"),
        ("16t", Window::ReportCellSizePixels, "16t"),
    ] {
        let action = Action::CSI(CSI::Window(Box::new(expected)));
        assert_eq!(action.to_string(), format!("\x1b[{encoded}"), "{wire}");
        assert_eq!(
            parse_fragmented(&format!("\x1b[{wire}")),
            [action],
            "{wire}"
        );
    }
}

#[test]
fn unsupported_title_operations_and_report_replies_remain_unspecified() {
    for fields in [
        "22;3t",
        "23;3;1t",
        "22;0;11t",
        "23;2;256t",
        "22;1;1;2t",
        "22;1;0;t",
        "22;;;t",
        "23;1:2t",
        "22;1;999999999999999999999999999t",
        "22;0;0;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;;t",
        "0#S",
        "2;10#S",
        ";#S",
    ] {
        let actions = parse_fragmented(&format!("\x1b[{fields}"));
        assert!(
            matches!(actions.as_slice(), [Action::CSI(CSI::Unspecified(_))]),
            "{fields}: {actions:?}"
        );
    }

    let actions = parse_fragmented("\x1b[2S\x1b[?2;1S");
    assert!(
        matches!(actions.as_slice(),
            [Action::CSI(CSI::Edit(Edit::ScrollUp(2))), Action::CSI(CSI::Device(device))]
                if matches!(**device, Device::XtSmGraphics(_))
        ),
        "{actions:?}"
    );
}
