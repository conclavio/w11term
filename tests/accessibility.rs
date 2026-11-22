#![cfg(feature = "ui-smoke")]
use std::{
    path::Path,
    process::{Child, Command},
    time::{Duration, Instant},
};
use windows::{
    Win32::{Foundation::*, System::Com::*, UI::Accessibility::*},
    core::*,
};

#[implement(IUIAutomationEventHandler)]
struct Subscription(std::sync::mpsc::Sender<UIA_EVENT_ID>);
impl IUIAutomationEventHandler_Impl for Subscription_Impl {
    fn HandleAutomationEvent(
        &self,
        _: Ref<IUIAutomationElement>,
        eventid: UIA_EVENT_ID,
    ) -> Result<()> {
        let _ = self.0.send(eventid);
        Ok(())
    }
}

struct Apartment;
impl Apartment {
    fn new() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() }
    }
}

struct Gui {
    child: Child,
    state: tempfile::TempDir,
}
impl Drop for Gui {
    fn drop(&mut self) {
        let _ = std::fs::write(self.state.path().join("finish"), b"");
        let _ = self.child.wait();
    }
}
fn fixture() -> anyhow::Result<Gui> {
    let state = tempfile::tempdir()?;
    std::fs::write(state.path().join("profiles.json"), br#"{"profiles":{}}"#)?;
    let wrapper = r#"
        $profiles = '"' + (Join-Path $env:W11TERM_UIA_SMOKE 'profiles.json') + '"'
        $p = Start-Process -FilePath $env:W11TERM_BINARY -ArgumentList @('--demo','--profiles',$profiles) -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $env:W11TERM_UIA_SMOKE 'gui.log')
        if (-not $p.WaitForExit(45000)) { $p.Kill(); exit 1 }; exit $p.ExitCode
    "#;
    let child = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", wrapper])
        .env("W11TERM_BINARY", env!("CARGO_BIN_EXE_w11term"))
        .env("W11TERM_UI_SMOKE", "1")
        .env("W11TERM_UIA_SMOKE", state.path())
        .env("W11TERM_DATA_DIR", state.path().join("data"))
        .env(
            "W11TERM_SETTINGS_PATH",
            state.path().join("appearance.json"),
        )
        .spawn()?;
    Ok(Gui { child, state })
}

fn read_pane(
    client: &IUIAutomation,
    state: &Path,
    marker: &str,
) -> anyhow::Result<IUIAutomationTextPattern2> {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = String::new();
    while Instant::now() < deadline {
        if let Ok(data) = std::fs::read_to_string(state.join(marker)) {
            let fields: Vec<usize> = data
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            if let [hwnd, pane] = fields.as_slice() {
                let result = (|| -> Result<IUIAutomationTextPattern2> {
                    unsafe {
                        let root = client.ElementFromHandle(HWND(*hwnd as *mut _))?;
                        let condition = client.CreatePropertyCondition(
                            UIA_AutomationIdPropertyId,
                            &format!("w11term-pane-{pane}").as_str().into(),
                        )?;
                        let node = root.FindFirst(TreeScope_Descendants, &condition)?;
                        node.GetCurrentPatternAs(UIA_TextPattern2Id)
                    }
                })();
                match result {
                    Ok(pattern) => return Ok(pattern),
                    Err(e) => last = e.to_string(),
                }
            }
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    anyhow::bail!(
        "Could not discover {marker} through the native UI Automation tree. {last}. GUI stderr: {}",
        std::fs::read_to_string(state.join("gui.log")).unwrap_or_default()
    )
}

#[test]
#[ignore = "launches an isolated Windows GUI and an external UI Automation client"]
fn native_reader_reads_a_terminal_selection_and_tracks_focus_between_split_panes()
-> anyhow::Result<()> {
    let _apartment = Apartment::new()?;
    // Native UIA screen coordinates are physical pixels even on a scaled display.
    use windows::Win32::UI::HiDpi::*;
    let prior_dpi =
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    struct Dpi(DPI_AWARENESS_CONTEXT);
    impl Drop for Dpi {
        fn drop(&mut self) {
            unsafe {
                SetThreadDpiAwarenessContext(self.0);
            }
        }
    }
    let _dpi = Dpi(prior_dpi);
    let gui = fixture()?;
    unsafe {
        let client: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
        let right = read_pane(&client, gui.state.path(), "right-reader")?;
        let doc = right.DocumentRange()?;
        let node = doc.GetEnclosingElement()?;
        let (event_sender, events) = std::sync::mpsc::channel();
        let listener: IUIAutomationEventHandler = Subscription(event_sender).into();
        for id in [
            UIA_Text_TextSelectionChangedEventId,
            UIA_Text_TextChangedEventId,
        ] {
            client.AddAutomationEventHandler(id, &node, TreeScope_Element, None, &listener)?;
        }
        let actual = doc.GetText(-1)?.to_string();
        assert!(
            actual.contains("right-reader"),
            "right viewport was {actual:?}"
        );
        assert!(!doc.GetText(-1)?.to_string().contains("left-reader"));
        let selected = right.GetSelection()?.GetElement(0)?;
        assert_eq!(selected.GetText(-1)?.to_string(), "right");
        let mut active = BOOL::default();
        let caret = right.GetCaretRange(&mut active)?;
        assert!(caret.GetText(-1)?.is_empty());
        assert!(
            caret.CompareEndpoints(
                TextPatternRangeEndpoint_Start,
                &selected,
                TextPatternRangeEndpoint_Start
            )? > 0
        );
        let word = doc.FindText(&BSTR::from("READER"), false, true)?;
        assert_eq!(word.GetText(3)?.to_string(), "rea");
        let bounds = range_rectangles(&word)?;
        let xy = bounds
            .first()
            .expect("visible word has a bounding rectangle");
        let at = right.RangeFromPoint(POINT {
            x: xy[0].ceil() as i32 + 1,
            y: xy[1].ceil() as i32 + 1,
        })?;
        at.ExpandToEnclosingUnit(TextUnit_Character)?;
        assert_eq!(
            at.GetText(-1)?.to_string(),
            "r",
            "hit at {xy:?}, element bounds {:?}, viewport {:?}",
            word.GetEnclosingElement()?.CurrentBoundingRectangle()?,
            right.DocumentRange()?.GetText(-1)?.to_string()
        );
        // A and Alef are adjacent in text but separated on screen by numbers
        // and two other Hebrew letters. Narrator must receive both rectangles.
        let mixed = doc.FindText(&BSTR::from("Aא"), false, false)?;
        let rectangles = range_rectangles(&mixed)?;
        assert_eq!(
            rectangles.len(),
            2,
            "one logical range should occupy two rectangles"
        );
        assert!(rectangles[1][0] > rectangles[0][0] + 2.0 * rectangles[0][2]);
        for (rect, expected) in rectangles.iter().zip(["A", "א"]) {
            let hit = right.RangeFromPoint(POINT {
                x: (rect[0] + rect[2] / 2.0).round() as i32,
                y: (rect[1] + rect[3] / 2.0).round() as i32,
            })?;
            hit.ExpandToEnclosingUnit(TextUnit_Character)?;
            assert_eq!(hit.GetText(-1)?.to_string(), expected);
        }
        word.Select()?;
        let wait = Instant::now() + Duration::from_secs(3);
        while right.GetSelection()?.GetElement(0)?.GetText(-1)? != "reader" {
            anyhow::ensure!(
                Instant::now() < wait,
                "range.Select did not update terminal selection"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        let mut got_selection = false;
        let wait = Instant::now() + Duration::from_secs(3);
        while !got_selection {
            got_selection = events.recv_timeout(wait.saturating_duration_since(Instant::now()))?
                == UIA_Text_TextSelectionChangedEventId;
        }
        std::fs::write(gui.state.path().join("append"), b"")?;
        let wait = Instant::now() + Duration::from_secs(3);
        loop {
            if events.recv_timeout(wait.saturating_duration_since(Instant::now()))?
                == UIA_Text_TextChangedEventId
                && right
                    .DocumentRange()?
                    .GetText(-1)?
                    .to_string()
                    .contains("new-output")
            {
                break;
            }
        }
        client.RemoveAllEventHandlers()?;
        let marker = std::fs::read_to_string(gui.state.path().join("right-reader"))?;
        let hwnd: usize = marker.split_whitespace().next().unwrap().parse()?;
        let root = client.ElementFromHandle(HWND(hwnd as *mut _))?;
        for (command, cell, target) in [
            ("plain", "1, 1", "None"),
            ("osc", "1, 2", "https://osc.example/test"),
            ("detected", "1, 3", "https://text.example/path"),
            ("neighbor", "Outside grid", "None"),
        ] {
            let pending = gui.state.path().join("pending-pointer");
            std::fs::write(&pending, command)?;
            std::fs::rename(pending, gui.state.path().join("pointer"))?;
            for label in [
                format!("Pointer cell   {cell}"),
                format!("Hyperlink   {target}"),
            ] {
                let condition =
                    client.CreatePropertyCondition(UIA_NamePropertyId, &label.as_str().into())?;
                let wait = Instant::now() + Duration::from_secs(5);
                while root.FindFirst(TreeScope_Descendants, &condition).is_err() {
                    anyhow::ensure!(
                        Instant::now() < wait,
                        "inspector did not expose {label:?} after {command}"
                    );
                    std::thread::sleep(Duration::from_millis(35));
                }
            }
        }
        std::fs::write(gui.state.path().join("switch"), b"")?;
        let left = read_pane(&client, gui.state.path(), "left-reader")?;
        assert!(
            left.DocumentRange()?
                .GetText(-1)?
                .to_string()
                .contains("left-reader")
        );
        assert!(
            doc.GetText(-1).is_err(),
            "retired pane ranges must become unavailable"
        );
    }
    Ok(())
}

fn append_output(state: &Path, target: &str, text: &str) -> std::io::Result<()> {
    let staged = state.join("staged-output");
    std::fs::write(&staged, text)?;
    std::fs::rename(staged, state.join(target))
}

fn wait_visible(
    pattern: &IUIAutomationTextPattern2,
    description: &str,
    condition: impl Fn(&str) -> bool,
) -> anyhow::Result<String> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let text = unsafe {
            pattern
                .GetVisibleRanges()?
                .GetElement(0)?
                .GetText(-1)?
                .to_string()
        };
        if condition(&text) {
            return Ok(text);
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "Waiting for {description}, viewport was {text:?}"
        );
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn range_rectangles(range: &IUIAutomationTextRange) -> Result<Vec<[f64; 4]>> {
    use windows::Win32::System::Ole::*;
    unsafe {
        let array = range.GetBoundingRectangles()?;
        if array.is_null() {
            return Ok(Vec::new());
        }
        let values = (|| -> Result<Vec<f64>> {
            (SafeArrayGetLBound(array, 1)?..=SafeArrayGetUBound(array, 1)?)
                .map(|i| {
                    let mut value = 0.0;
                    SafeArrayGetElement(array, &i, std::ptr::from_mut(&mut value).cast())?;
                    Ok(value)
                })
                .collect()
        })();
        SafeArrayDestroy(array)?;
        let values = values?;
        let (rectangles, remainder) = values.as_chunks::<4>();
        assert!(remainder.is_empty());
        Ok(rectangles.to_vec())
    }
}

#[test]
#[ignore = "uses the Windows UIA client to reveal a retained range in a native split pane"]
fn native_reader_scrolls_a_retained_multiline_range_without_moving_selection_or_focus()
-> anyhow::Result<()> {
    let _apartment = Apartment::new()?;
    let gui = fixture()?;
    let state = gui.state.path();
    unsafe {
        let client: IUIAutomation = CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)?;
        let right = read_pane(&client, state, "right-reader")?;
        let initial = wait_visible(&right, "initial right pane", |v| v.contains("right-reader"))?;
        let rows = initial.split("\r\n").count();
        anyhow::ensure!(rows >= 4, "fixture pane too short");

        // Give both panes enough real history to scroll. A second pane with an
        // empty buffer would hide accidental scrolling directed to that pane.
        let padding = "padding\n".repeat(rows + 4);
        append_output(
            state,
            "append-neighbor",
            &format!("{padding}\x1b[2J\x1b[Hleft-reader-untouched"),
        )?;
        append_output(
            state,
            "append",
            &format!("{padding}range-first\nkeep-selected\nrange-last\nafter-range"),
        )?;
        wait_visible(&right, "multiline target", |v| v.contains("after-range"))?;
        let doc = right.DocumentRange()?;
        let held = doc.FindText(&BSTR::from("range-first"), false, false)?;
        let after = doc.FindText(&BSTR::from("after-range"), false, false)?;
        // End exactly at column zero on the following physical row. End is
        // exclusive: aligning to the bottom must stop on range-last.
        held.MoveEndpointByRange(
            TextPatternRangeEndpoint_End,
            &after,
            TextPatternRangeEndpoint_Start,
        )?;
        let selected = doc.FindText(&BSTR::from("keep-selected"), false, false)?;
        selected.Select()?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while right.GetSelection()?.GetElement(0)?.GetText(-1)? != "keep-selected" {
            anyhow::ensure!(Instant::now() < deadline, "range selection failed");
            std::thread::sleep(Duration::from_millis(25));
        }
        let source = doc.GetEnclosingElement()?.CurrentAutomationId()?;
        append_output(state, "append", &format!("\n{padding}live-tail"))?;
        wait_visible(&right, "old range outside the viewport", |v| {
            v.contains("live-tail") && !v.contains("range-first")
        })?;
        assert!(held.GetText(-1)?.to_string().contains("keep-selected"));
        assert!(
            range_rectangles(&held)?.is_empty(),
            "buried range reported on-screen bounds"
        );

        held.ScrollIntoView(true)?;
        wait_visible(&right, "first covered row at top", |v| {
            v.split("\r\n")
                .next()
                .is_some_and(|s| s.trim_end() == "range-first")
        })?;
        let top_rects = range_rectangles(&held)?;
        assert_eq!(top_rects.len(), 3);
        let top = right
            .DocumentRange()?
            .FindText(&BSTR::from("range-first"), false, false)?;
        assert_eq!(top_rects[0][1], range_rectangles(&top)?[0][1]);
        assert_eq!(
            right
                .GetSelection()?
                .GetElement(0)?
                .GetText(-1)?
                .to_string(),
            "keep-selected"
        );
        held.ScrollIntoView(false)?;
        wait_visible(&right, "last covered row at bottom", |v| {
            v.rsplit("\r\n")
                .next()
                .is_some_and(|s| s.trim_end() == "range-last")
        })?;
        let bottom_rects = range_rectangles(&held)?;
        assert_eq!(bottom_rects.len(), 3);
        let bottom = right
            .DocumentRange()?
            .FindText(&BSTR::from("range-last"), false, false)?;
        assert_eq!(bottom_rects[2][1], range_rectangles(&bottom)?[0][1]);
        assert!(bottom_rects[0][1] > top_rects[0][1]);
        assert_eq!(
            right
                .GetSelection()?
                .GetElement(0)?
                .GetText(-1)?
                .to_string(),
            "keep-selected"
        );
        assert_eq!(
            right
                .DocumentRange()?
                .GetEnclosingElement()?
                .CurrentAutomationId()?,
            source
        );

        append_output(state, "append", "\x1b[3J\ncleared-tail")?;
        let at_clear = wait_visible(&right, "old rows cleared from scrollback", |v| {
            v.contains("cleared-tail") && !v.contains("range-last")
        })?;
        assert!(
            held.ScrollIntoView(true).is_err(),
            "an erased range still scrolled"
        );
        assert_eq!(
            right
                .GetVisibleRanges()?
                .GetElement(0)?
                .GetText(-1)?
                .to_string(),
            at_clear
        );

        std::fs::write(state.join("switch"), [])?;
        let left = read_pane(&client, state, "left-reader")?;
        let neighbor = wait_visible(&left, "original neighboring viewport", |v| {
            v.starts_with("left-reader-untouched")
        })?;
        assert!(!neighbor.contains("range-first"));
        assert!(
            held.ScrollIntoView(false).is_err(),
            "a range from a retired pane still scrolled"
        );
        assert_eq!(
            left.GetVisibleRanges()?
                .GetElement(0)?
                .GetText(-1)?
                .to_string(),
            neighbor
        );
    }
    Ok(())
}
