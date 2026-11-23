use super::*;
use std::{
    io::BufRead,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use windows::Win32::{
    Foundation::WAIT_TIMEOUT,
    System::{
        Console::*,
        Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    },
};
use windows::core::BOOL;

const FIXTURE: &str = "w11term-conpty-test-fixture";
const ARGV_PROBE: &str = "an argument \"with quotes\" 界\\";
static INTERRUPT: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn ctrl(kind: u32) -> BOOL {
    if kind == CTRL_C_EVENT {
        INTERRUPT.store(true, Ordering::Release);
        true.into()
    } else {
        false.into()
    }
}
fn child_size() -> (i16, i16) {
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    unsafe {
        GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE).unwrap(), &mut info).unwrap();
    }
    (
        info.srWindow.Right - info.srWindow.Left + 1,
        info.srWindow.Bottom - info.srWindow.Top + 1,
    )
}
fn child_args(test: &str) -> Vec<String> {
    vec![
        "--exact".into(),
        format!("conpty::tests::{test}"),
        "--ignored".into(),
        "--nocapture".into(),
        "--test-threads=1".into(),
        "--skip".into(),
        ARGV_PROBE.into(),
    ]
}
#[test]
#[ignore = "helper process started by a native ConPTY integration test"]
fn nested_child() {
    if Path::new(FIXTURE).exists() {
        std::thread::sleep(Duration::from_secs(30));
    }
}
#[test]
#[ignore = "helper process started by a native ConPTY integration test"]
fn native_child() {
    if !Path::new(FIXTURE).exists() {
        return;
    }
    assert!(std::env::args().any(|arg| arg == ARGV_PROBE));
    unsafe {
        SetConsoleCtrlHandler(Some(ctrl), true).unwrap();
    }
    let (width, height) = child_size();
    println!("READY-{width}x{height}");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).unwrap();
    if line.trim() == "tree" {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(child_args("nested_child"))
            .spawn()
            .unwrap();
        println!("TREE.{}.{}.END", std::process::id(), child.id());
        for _ in 0..300 {
            if Path::new("fill-output").exists() {
                let line = "123456789abcdef".repeat(5);
                for n in 0..50_000 {
                    println!("{n}-{line}");
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = child.wait();
        return;
    }
    let (width, height) = child_size();
    println!("INPUT-{}-{width}x{height}", line.trim());
    for _ in 0..1000 {
        if INTERRUPT.load(Ordering::Acquire) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(INTERRUPT.load(Ordering::Acquire));
    println!("CTRL-C-DELIVERED");
    // More than an anonymous pipe's buffer, with a unique sentinel at the end.
    for i in 0..600 {
        println!("final-data-{i:04}-abcdefghijklmnop");
    }
    println!("LAST-OUTPUT");
    std::io::stdout().flush().unwrap();
    std::process::exit(37);
}

struct Fixture {
    input: mpsc::UnboundedSender<TransportInput>,
    output: mpsc::Receiver<Event>,
    run: tokio::task::JoinHandle<Result<()>>,
    screen: String,
    cwd: tempfile::TempDir,
}
impl Fixture {
    fn start() -> Result<Self> {
        let cwd = tempfile::tempdir()?;
        std::fs::write(cwd.path().join(FIXTURE), b"")?;
        let options = LocalOptions {
            executable: std::env::current_exe()?,
            args: child_args("native_child"),
            cwd: Some(cwd.path().into()),
            scrollback_lines: Default::default(),
            color_scheme: Default::default(),
            tab_color: Default::default(),
            powershell_integration: false,
        };
        let (input, rx) = mpsc::unbounded_channel();
        let (tx, output) = mpsc::channel(3);
        let run = tokio::spawn(run(options, size(80, 24), rx, tx));
        Ok(Self {
            input,
            output,
            run,
            screen: String::new(),
            cwd,
        })
    }
    async fn until(&mut self, value: &str) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(12), async {
            while !self.screen.contains(value) {
                match self.output.recv().await {
                    Some(Event::Data(bytes)) => {
                        self.screen.push_str(&String::from_utf8_lossy(&bytes))
                    }
                    Some(Event::Status(status)) => self.screen.push_str(&format!("\n[{status}]\n")),
                    Some(Event::Connected(_) | Event::Disconnected) => {}
                    Some(Event::HerdrViewer) => panic!("ConPTY supplied an SSH viewer stream"),
                    Some(Event::SessionListUpdated(_)) => {
                        panic!("ConPTY supplied an SSH session list")
                    }
                    None => anyhow::bail!("ConPTY ended before {value}, output {:?}", self.screen),
                }
            }
            anyhow::Ok(())
        })
        .await
        .with_context(|| format!("waiting for {value}"))?
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.run.abort();
    }
}
fn size(cols: usize, rows: usize) -> TerminalSize {
    TerminalSize {
        cols,
        rows,
        ..Default::default()
    }
}

#[tokio::test]
async fn local_process_receives_input_interrupt_and_resize_and_drains_before_exit() -> Result<()> {
    let mut fixture = Fixture::start()?;
    fixture.until("READY-80x24").await?;
    fixture.input.send(TransportInput::Resize(size(104, 33)))?;
    let (reply, stale) = tokio::sync::oneshot::channel();
    fixture.input.send(TransportInput::Scoped {
        connection: 0,
        bytes: b"old paste ".to_vec(),
        reply,
        kind: crate::terminal::ScopedInput::Clipboard,
    })?;
    assert!(stale.await?.is_err());
    let (ack, rx) = tokio::sync::oneshot::channel();
    fixture
        .input
        .send(TransportInput::Raw(b"typed ".to_vec(), ack))?;
    rx.await?.map_err(anyhow::Error::msg)?;
    let (reply, delivered) = tokio::sync::oneshot::channel();
    fixture.input.send(TransportInput::Scoped {
        connection: 1,
        bytes: b"input\r".to_vec(),
        reply,
        kind: crate::terminal::ScopedInput::Clipboard,
    })?;
    delivered.await?.map_err(anyhow::Error::msg)?;
    fixture.until("INPUT-typed input-104x33").await?;
    fixture.input.send(TransportInput::Data(vec![3]))?;
    fixture.until("[Local process exited with code 37]").await?;
    assert!(
        fixture.screen.contains("CTRL-C-DELIVERED"),
        "{}",
        fixture.screen
    );
    assert!(fixture.screen.contains("LAST-OUTPUT"));
    (&mut fixture.run).await??;
    Ok(())
}
#[tokio::test]
async fn aborting_a_pane_releases_its_child_tree_with_other_handles_still_open() -> Result<()> {
    let mut fixture = Fixture::start()?;
    fixture.until("READY-80x24").await?;
    fixture
        .input
        .send(TransportInput::Data(b"tree\r".to_vec()))?;
    fixture.until(".END").await?;
    let text = fixture
        .screen
        .split_once("TREE.")
        .unwrap()
        .1
        .split_once(".END")
        .unwrap()
        .0;
    let processes = text
        .split('.')
        .map(|pid| {
            anyhow::Ok(unsafe {
                OwnedHandle::from_raw_handle(
                    OpenProcess(PROCESS_SYNCHRONIZE, false, pid.parse()?)?.0,
                )
            })
        })
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(processes.len(), 2);
    for proc in &processes {
        assert_eq!(
            unsafe { WaitForSingleObject(handle(proc), 0) },
            WAIT_TIMEOUT
        );
    }
    std::fs::write(fixture.cwd.path().join("fill-output"), b"")?;
    tokio::time::timeout(Duration::from_secs(4), async {
        while fixture.output.capacity() != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("child did not fill the undrained output queue")?;
    fixture.run.abort();
    assert!((&mut fixture.run).await.unwrap_err().is_cancelled());
    for proc in &processes {
        assert_eq!(
            unsafe { WaitForSingleObject(handle(proc), 3000) },
            WAIT_OBJECT_0
        );
    }
    Ok(())
}

#[tokio::test]
async fn missing_executable_returns_a_launch_error_and_releases_endpoints() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let options = LocalOptions {
        executable: dir.path().join("missing-w11term-fixture.exe"),
        args: vec![],
        cwd: None,
        scrollback_lines: Default::default(),
        color_scheme: Default::default(),
        tab_color: Default::default(),
        powershell_integration: false,
    };
    let (_input, rx) = mpsc::unbounded_channel();
    let (tx, mut output) = mpsc::channel(1);
    let result =
        tokio::time::timeout(Duration::from_secs(3), run(options, size(80, 24), rx, tx)).await?;
    assert!(format!("{:#}", result.unwrap_err()).contains("missing-w11term-fixture.exe"));
    assert!(output.recv().await.is_none());
    Ok(())
}

#[tokio::test]
async fn failed_local_launch_offers_one_fresh_restart_per_termination() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let options = LocalOptions {
        executable: dir.path().join("missing-w11term-fixture.exe"),
        args: vec![],
        cwd: None,
        scrollback_lines: Default::default(),
        color_scheme: Default::default(),
        tab_color: Default::default(),
        powershell_integration: false,
    };
    let pane = crate::session::Session::start(crate::options::Source::Local(options));
    let mut cursor = 0;
    for token in 1..=2 {
        let batch = pane.events.wait(cursor, Duration::from_secs(5)).await;
        let exit = batch
            .events
            .iter()
            .find(|e| e.kind == "pane-exited")
            .context("missing exit event")?;
        assert_eq!(exit.data, serde_json::json!({"exit_token":token}));
        assert!(
            pane.frames
                .load()
                .status
                .contains("missing-w11term-fixture.exe")
        );
        assert_eq!(pane.frames.load().connection_generation, 0);
        if token == 1 {
            pane.restart_exited(token)?;
        }
        assert!(pane.restart_exited(token - 1).is_err());
        cursor = batch.next;
    }
    Ok(())
}
