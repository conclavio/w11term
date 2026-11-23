use super::*;
use tokio::time::advance;
use w11term::recorder::Recorder;
use wezterm_term::TerminalSize;

#[tokio::test(start_paused = true)]
#[ignore = "steps a genuine timed capture from WinUI and the CLI beside a live PowerShell pane"]
async fn native_paused_replay_steps_images_and_resizes_without_changing_powershell_focus() {
    let state = tempfile::tempdir().unwrap();
    let capture = state.path().join("review.vt");
    let initial = TerminalSize {
        rows: 18,
        cols: 64,
        pixel_width: 640,
        pixel_height: 324,
        dpi: 96,
    };
    let mut recorder = Recorder::start_timed(&capture, initial).unwrap();
    advance(Duration::from_millis(100)).await;
    recorder.write(b"\x1b[?25l\x1b[HFIRST-RED\x1b[2;1H\x1b_Ga=T,i=7,p=1,f=24,s=1,v=1,c=1,r=1,C=1;/wAA\x1b\\").unwrap();
    advance(Duration::from_millis(50)).await;
    // The terminal holds both the replacement image and the new grid until DECRST.
    recorder.write(b"\x1b[?2026h\x1b[HSECOND-BLUE\x1b[2;1H\x1b_Ga=T,i=7,p=1,f=24,s=1,v=1,c=1,r=1,C=1;AAD/\x1b\\").unwrap();
    recorder
        .resize(TerminalSize {
            cols: 72,
            rows: 20,
            pixel_width: 792,
            pixel_height: 400,
            dpi: 120,
        })
        .unwrap();
    advance(Duration::from_millis(50)).await;
    recorder.write(b"\x1b[?2026l").unwrap();
    // A long recorded idle period leaves time to use the real UI. Step skips it.
    advance(Duration::from_secs(3600)).await;
    recorder.write(b"\x1b[4;1HAFTER-LONG-IDLE").unwrap();
    advance(Duration::from_millis(500)).await;
    recorder.write(b"\x1b[5;1HFINAL-PLAYED").unwrap();
    advance(Duration::from_millis(500)).await;
    recorder.finish().unwrap();

    fs::write(state.path().join("profiles.json"), serde_json::to_vec(&json!({"profiles":{
        "Review PowerShell":{"backend":"local","executable":"powershell.exe","args":["-NoProfile","-NoLogo"]}
    }})).unwrap()).unwrap();
    drive_gui("replay-controls-ui.ps1", state.path());
    for (name, pixel, absent) in [
        ("first", [255, 0, 0, 255], [0, 0, 255, 255]),
        ("held", [255, 0, 0, 255], [0, 0, 255, 255]),
        ("committed", [0, 0, 255, 255], [255, 0, 0, 255]),
    ] {
        let surface = image::open(state.path().join(format!("{name}.png")))
            .unwrap()
            .to_rgba8();
        assert!(
            surface.pixels().any(|p| p.0 == pixel),
            "{name} screenshot lost its image"
        );
        assert!(
            !surface.pixels().any(|p| p.0 == absent),
            "{name} screenshot leaked another frame"
        );
    }
}
