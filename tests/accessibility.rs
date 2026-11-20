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
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() }
        }
    }
    let _apartment = Apartment;
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
        let bounds = word.GetBoundingRectangles()?;
        use windows::Win32::System::Ole::*;
        let upper = SafeArrayGetUBound(bounds, 1)?;
        let mut xy = [0.0f64; 2];
        for (i, coord) in xy.iter_mut().enumerate() {
            SafeArrayGetElement(bounds, &(i as i32), std::ptr::from_mut(coord).cast())?;
        }
        SafeArrayDestroy(bounds)?;
        assert!(upper >= 3);
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
        word.Select()?;
        let wait = Instant::now() + Duration::from_secs(3);
        while right
            .GetSelection()?
            .GetElement(0)?
            .GetText(-1)?
            != "reader"
        {
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
