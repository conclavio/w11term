//! Windows pseudoconsole transport for a local interactive process.
mod process;
#[cfg(test)]
mod tests;

use crate::{
    options::LocalOptions,
    ssh::Event,
    terminal::{ScopedInput, TransportInput},
};
use anyhow::{Context, Result};
use process::{Console, Launch, handle};
use std::{
    fs::File,
    io::{Read, Write},
    os::windows::io::{FromRawHandle, OwnedHandle},
    sync::{Arc, Mutex},
};
use tokio::{sync::mpsc, task::spawn_blocking};
use wezterm_term::TerminalSize;
use windows::Win32::{
    Foundation::{ERROR_BROKEN_PIPE, WAIT_OBJECT_0},
    System::Threading::{
        CreateEventW, GetExitCodeProcess, INFINITE, SetEvent, WaitForMultipleObjects,
    },
};

struct Shutdown(Arc<OwnedHandle>);
impl Drop for Shutdown {
    fn drop(&mut self) {
        // Also runs when the session task is aborted or the pane runtime shuts down.
        let _ = unsafe { SetEvent(handle(self.0.as_ref())) };
    }
}

fn read_output(mut pipe: File, tx: mpsc::Sender<std::io::Result<Vec<u8>>>) {
    let mut buffer = [0u8; 32 * 1024];
    loop {
        let bytes = match pipe.read(&mut buffer) {
            Ok(0) => return,
            Ok(n) => Ok(buffer[..n].to_vec()),
            Err(e) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE.0 as i32) => return,
            Err(e) => {
                let _ = tx.blocking_send(Err(e));
                return;
            }
        };
        if tx.blocking_send(bytes).is_err() {
            return;
        }
    }
}

fn write_input(
    mut pipe: File,
    console: Arc<Mutex<Option<Console>>>,
    mut rx: mpsc::Receiver<TransportInput>,
    generation: u64,
) -> Result<()> {
    while let Some(message) = rx.blocking_recv() {
        match message {
            TransportInput::Data(bytes) | TransportInput::Automatic(bytes) => {
                pipe.write_all(&bytes).context("write local input")?
            }
            TransportInput::Scoped {
                connection,
                reply,
                kind,
                ..
            } if connection != generation => {
                kind.reject(reply, kind.stale_error());
            }
            TransportInput::Scoped {
                kind: ScopedInput::GroupInterrupt(_),
                reply,
                ..
            } => {
                let _ = reply.send(Err("Group interrupts require a direct SSH shell.".into()));
            }
            TransportInput::Raw(bytes, reply) | TransportInput::Scoped { bytes, reply, .. } => {
                let result = pipe.write_all(&bytes);
                let _ = reply.send(
                    result
                        .as_ref()
                        .map(|_| ())
                        .map_err(|e| format!("write local input: {e}")),
                );
                result?;
            }
            TransportInput::Resize(size) => {
                let guard = console.lock().unwrap();
                if let Some(console) = guard.as_ref() {
                    console.resize(size)?;
                }
            }
            TransportInput::Sftp(job) => {
                let _ = job.reply.send(Err("SFTP requires an SSH session".into()));
            }
            TransportInput::Authenticate(_) => {}
            message @ TransportInput::DisconnectShell { .. } => {
                message.reject_input("Disconnect requires a direct SSH shell.");
            }
        }
    }
    Ok(())
}

pub async fn run(
    options: LocalOptions,
    size: TerminalSize,
    input: mpsc::UnboundedReceiver<TransportInput>,
    output: mpsc::Sender<Event>,
) -> Result<()> {
    run_generation(options, size, input, output, 1).await
}

pub(crate) async fn run_generation(
    options: LocalOptions,
    size: TerminalSize,
    mut input: mpsc::UnboundedReceiver<TransportInput>,
    output: mpsc::Sender<Event>,
    generation: u64,
) -> Result<()> {
    let signal =
        Arc::new(unsafe { OwnedHandle::from_raw_handle(CreateEventW(None, true, false, None)?.0) });
    let shutdown = Shutdown(signal.clone());
    let Launch {
        input: write,
        output: read,
        job,
        console,
        process,
    } = spawn_blocking(move || Launch::start(&options, size)).await??;
    let console = Arc::new(Mutex::new(Some(console)));
    let closer = console.clone();
    let (read_tx, mut read_rx) = mpsc::channel(4);
    let (write_tx, write_rx) = mpsc::channel(1);
    let _reader = spawn_blocking(move || read_output(read, read_tx));
    let mut writer = spawn_blocking(move || write_input(write, console, write_rx, generation));
    let monitor = spawn_blocking(move || -> Result<Option<u32>> {
        let wait = unsafe {
            WaitForMultipleObjects(
                &[handle(&process), handle(signal.as_ref())],
                false,
                INFINITE,
            )
        };
        // Releasing this private job closes the entire local process tree. Closing
        // the pseudoconsole can flush final VT data; the reader runs independently.
        drop(job);
        drop(closer.lock().unwrap().take());
        if wait == WAIT_OBJECT_0 {
            let mut code = 0;
            unsafe {
                GetExitCodeProcess(handle(&process), &mut code)?;
            }
            Ok(Some(code))
        } else if wait.0 == WAIT_OBJECT_0.0 + 1 {
            Ok(None)
        } else {
            Err(std::io::Error::last_os_error()).context("wait for local process")
        }
    });
    let _ = output.send(Event::Connected(generation)).await;
    let _ = output.send(Event::Status("Local shell".into())).await;
    let forward_input = async {
        while let Some(command) = input.recv().await {
            if write_tx.send(command).await.is_err() {
                break;
            }
        }
    };
    let forward_output = async {
        while let Some(bytes) = read_rx.recv().await {
            if output
                .send(Event::Data(bytes.context("read local process output")?))
                .await
                .is_err()
            {
                break;
            }
        }
        anyhow::Ok(())
    };
    let finished = async {
        let ((), code) = tokio::try_join!(forward_output, async { monitor.await? })?;
        if let Some(code) = code {
            let _ = output
                .send(Event::Status(format!(
                    "Local process exited with code {code}"
                )))
                .await;
        }
        anyhow::Ok(())
    };
    tokio::pin!(finished);
    let writer_result = tokio::select! {
        result = &mut finished => return result,
        _ = forward_input => None,
        result = &mut writer => Some(result),
        _ = output.closed() => return Ok(()),
    };
    // Closing stdin or losing the writer can race the child's last writes.
    // Stop the job and let ClosePseudoConsole flush into the live reader.
    drop(shutdown);
    drop(write_tx);
    finished.await?;
    match writer_result {
        Some(result) => result??,
        None => writer.await??,
    }
    Ok(())
}
