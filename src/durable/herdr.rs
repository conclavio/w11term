//! Herdr Core generation 1. SSH supplies unmodified hc connect stdio (no outer PTY).
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

const MAX_RPC: usize = 2 * 1024 * 1024;
const MAX_DATA: usize = 256 * 1024; // a stock stream's entire outstanding credit window
const CHUNK: usize = 16 * 1024;
pub const MAX_ENTRIES: usize = 512;

#[derive(Debug, Clone)]
pub struct Entry {
    pub address: String,
    pub name: String,
    pub created_ms: u64,
    pub pid: u32,
    pub cwd: String,
    pub command: String,
}

pub fn numeric_address(address: &str) -> Option<u32> {
    let number = address.strip_prefix('t')?.parse::<u32>().ok()?;
    (number > 0 && address == format!("t{number}")).then_some(number)
}

fn entry(record: &Value) -> Option<Entry> {
    let address = record["address"].as_str()?;
    numeric_address(address)?;
    if record["state"] != "live" {
        return None;
    }
    let pid = u32::try_from(record["pid"].as_u64()?)
        .ok()
        .filter(|n| *n > 0)?;
    let display = |s: &str| {
        s.chars()
            .filter(|c| !c.is_control())
            .take(256)
            .collect::<String>()
    };
    Some(Entry {
        address: address.into(),
        name: display(record["name"].as_str().unwrap_or(address)),
        created_ms: record["created_ms"].as_u64()?,
        pid,
        cwd: display(record["cwd"].as_str().unwrap_or_default()),
        command: display(
            record["argv"]
                .get(0)
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
    })
}

pub fn core_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn reply(bytes: &[u8], id: u32) -> Result<Value> {
    let mut message: Value =
        serde_json::from_slice(bytes).context("Invalid Herdr JSON-RPC response")?;
    ensure!(
        message["jsonrpc"] == "2.0" && message["id"].as_u64() == Some(u64::from(id)),
        "Unexpected Herdr JSON-RPC response"
    );
    if let Some(error) = message.get("error") {
        let reason = error["message"].as_str().unwrap_or("request failed");
        bail!(
            "Herdr: {}",
            reason
                .chars()
                .filter(|c| !c.is_control())
                .take(240)
                .collect::<String>()
        );
    }
    message
        .get_mut("result")
        .map(Value::take)
        .context("Missing Herdr RPC result")
}

async fn frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    channel: u32,
    kind: u8,
    bytes: &[u8],
) -> Result<()> {
    let mut header = [0; 9];
    header[..4].copy_from_slice(&channel.to_le_bytes());
    header[4] = kind;
    header[5..].copy_from_slice(&u32::try_from(bytes.len())?.to_le_bytes());
    w.write_all(&header).await?;
    w.write_all(bytes).await?;
    w.flush().await?;
    Ok(())
}

struct Incoming<R> {
    io: BufReader<R>,
    // Keep partial frames across select! cancellation when keyboard input or a resize arrives.
    pending: Vec<u8>,
}

impl<R: AsyncRead + Unpin> Incoming<R> {
    async fn fill(&mut self, len: usize) -> Result<()> {
        while self.pending.len() < len {
            let block = self.io.fill_buf().await?;
            ensure!(!block.is_empty(), "Herdr connection ended in a frame");
            let count = block.len().min(len - self.pending.len());
            self.pending.extend_from_slice(&block[..count]);
            self.io.consume(count);
        }
        Ok(())
    }

    async fn next(&mut self, maximum: usize, channel: u32) -> Result<(u8, Vec<u8>)> {
        self.fill(9).await?;
        ensure!(
            self.pending[..4] == channel.to_le_bytes(),
            "Unexpected Herdr stream channel"
        );
        let kind = self.pending[4];
        let size = u32::from_le_bytes(self.pending[5..9].try_into().unwrap()) as usize;
        ensure!(
            size <= maximum,
            "Herdr frame exceeds the viewer's size limit"
        );
        self.fill(9 + size).await?;
        let payload = self.pending.split_off(9);
        self.pending.clear();
        Ok((kind, payload))
    }
}

pub struct Client<R, W> {
    io: Incoming<R>,
    writer: W,
    pub core: String,
    automatic: bool,
    last_id: u32,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Client<R, W> {
    pub async fn connect(reader: R, mut writer: W) -> Result<Self> {
        let mut io = BufReader::new(reader);
        writer.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"hello\",\"params\":{\"generation\":1,\"framed\":true}}\n").await?;
        writer.flush().await?;
        let mut line = Vec::new();
        loop {
            let block = io.fill_buf().await?;
            ensure!(
                !block.is_empty(),
                "hc connect closed before the Herdr hello"
            );
            let end = block.iter().position(|&b| b == b'\n');
            let count = end.map_or(block.len(), |i| i + 1);
            ensure!(line.len() + count <= 64 * 1024, "Oversized Herdr hello");
            line.extend_from_slice(&block[..count]);
            io.consume(count);
            if end.is_some() {
                break;
            }
        }
        let info = reply(&line, 1)?;
        ensure!(
            info["generation"] == 1 && info["framed"] == true,
            "Unsupported Herdr wire protocol"
        );
        let methods = info["methods"]
            .as_array()
            .context("Missing Herdr methods")?;
        ensure!(
            ["terminal.list", "terminal.attach"]
                .iter()
                .all(|m| methods.iter().any(|v| v == m)),
            "Herdr does not advertise interactive terminal attachment"
        );
        let core = info["core"]["id"]
            .as_str()
            .filter(|s| core_id(s))
            .context("Invalid Herdr Core identity")?
            .to_owned();
        let automatic = info["features"]
            .as_array()
            .is_some_and(|items| items.iter().any(|s| s == "automatic_input"));
        Ok(Self {
            io: Incoming {
                io,
                pending: vec![],
            },
            writer,
            core,
            automatic,
            last_id: 1,
        })
    }

    pub fn verify_core(&self, expected: &str) -> Result<()> {
        ensure!(
            self.core == expected,
            "Herdr Core identity changed; choose the terminal again"
        );
        Ok(())
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.last_id = self
            .last_id
            .checked_add(1)
            .context("Herdr request ID exhausted")?;
        let request = serde_json::to_vec(
            &json!({"jsonrpc":"2.0","id":self.last_id,"method":method,"params":params}),
        )?;
        frame(&mut self.writer, 0, 0, &request).await?;
        let (kind, payload) = self.io.next(MAX_RPC, 0).await?;
        ensure!(kind == 0, "Unexpected Herdr control frame");
        reply(&payload, self.last_id)
    }

    pub async fn list(&mut self) -> Result<Vec<Entry>> {
        let result = self.rpc("terminal.list", json!({})).await?;
        let records = result["terminals"]
            .as_array()
            .context("Invalid Herdr terminal listing")?;
        let mut seen = std::collections::BTreeSet::new();
        let mut entries = Vec::new();
        for e in records.iter().filter_map(entry) {
            ensure!(
                seen.insert(e.address.clone()),
                "Herdr returned a duplicate terminal address"
            );
            ensure!(
                entries.len() < MAX_ENTRIES,
                "Too many Herdr terminals to display"
            );
            entries.push(e);
        }
        Ok(entries)
    }

    pub async fn attach(mut self, expected: &Entry, cols: u16, rows: u16) -> Result<Viewer<R, W>> {
        ensure!(
            numeric_address(&expected.address).is_some(),
            "Invalid Herdr terminal address"
        );
        ensure!(cols > 0 && rows > 0, "Invalid Herdr terminal grid");
        let result=self.rpc("terminal.attach",json!({"terminal":expected.address,"cols":cols,"rows":rows,"observe":false,"history":0})).await?;
        let actual =
            entry(&result["terminal"]).context("Herdr terminal is missing or no longer live")?;
        ensure!(
            actual.address == expected.address
                && actual.pid == expected.pid
                && actual.created_ms == expected.created_ms,
            "Herdr attached a different terminal or program"
        );
        let channel = result["channel"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|&n| n > 0)
            .context("Invalid Herdr viewer channel")?;
        let automatic = self.automatic && result["automatic_input"] == true;
        Ok(Viewer {
            io: self.io,
            writer: self.writer,
            channel,
            automatic,
        })
    }
}

pub struct Viewer<R, W> {
    io: Incoming<R>,
    writer: W,
    channel: u32,
    automatic: bool,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Viewer<R, W> {
    /// Consume one whole stream frame. CREDIT is granted by the caller after forwarding DATA.
    pub async fn next_data(&mut self) -> Result<Option<Vec<u8>>> {
        let (kind, payload) = self.io.next(MAX_DATA, self.channel).await?;
        match kind {
            0 => Ok(Some(payload)),
            2 if payload.is_empty() => Ok(None),
            2 => {
                let info: Value = serde_json::from_slice(&payload)?;
                let reason = info["reason"].as_str().unwrap_or("unknown");
                if reason == "exited" {
                    Ok(None)
                } else {
                    bail!(
                        "Herdr terminal stream closed ({})",
                        reason
                            .chars()
                            .filter(|c| !c.is_control())
                            .take(100)
                            .collect::<String>()
                    )
                }
            }
            _ => bail!("Unexpected Herdr viewer frame {kind}"),
        }
    }
    pub async fn input(&mut self, bytes: &[u8]) -> Result<()> {
        self.send(bytes, false).await
    }
    pub async fn automatic_input(&mut self, bytes: &[u8]) -> Result<()> {
        self.send(bytes, true).await
    }
    async fn send(&mut self, bytes: &[u8], automatic: bool) -> Result<()> {
        let kind = if automatic && self.automatic { 6 } else { 0 };
        for chunk in bytes.chunks(CHUNK) {
            frame(&mut self.writer, self.channel, kind, chunk).await?;
        }
        Ok(())
    }
    pub async fn resize(&mut self, cols: u16, rows: u16) -> Result<()> {
        ensure!(cols > 0 && rows > 0, "Invalid Herdr terminal grid");
        frame(
            &mut self.writer,
            self.channel,
            3,
            &[cols.to_le_bytes(), rows.to_le_bytes()].concat(),
        )
        .await
    }
    pub async fn credit(&mut self, bytes: usize) -> Result<()> {
        frame(
            &mut self.writer,
            self.channel,
            1,
            &u32::try_from(bytes)?.to_le_bytes(),
        )
        .await
    }
    pub async fn close(&mut self) -> Result<()> {
        frame(&mut self.writer, self.channel, 2, &[]).await
    }
}

/// An hc record's address can outlive its program when "keep" is enabled.
/// Linux boot and process-start ticks distinguish an OS-recycled PID as well.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    pub core: String,
    pub created_ms: u64,
    pub pid: u32,
    pub boot: String,
    pub ticks: u64,
}

pub fn boot_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
}

impl Target {
    pub fn verify_birth(&self, boot: &str, ticks: Option<u64>) -> Result<()> {
        ensure!(
            self.boot == boot && ticks == Some(self.ticks),
            "Herdr program generation changed; choose the terminal again"
        );
        Ok(())
    }
    pub fn generation(&self) -> String {
        format!(
            "{}/{}/{}/{}/{}",
            self.core, self.created_ms, self.pid, self.boot, self.ticks
        )
    }
    pub fn parse(address: &str, generation: &str) -> Option<Self> {
        numeric_address(address)?;
        let mut parts = generation.split('/');
        let target = Self {
            core: parts.next()?.to_owned(),
            created_ms: parts.next()?.parse().ok()?,
            pid: parts.next()?.parse().ok()?,
            boot: parts.next()?.to_owned(),
            ticks: parts.next()?.parse().ok()?,
        };
        (core_id(&target.core)
            && target.pid > 0
            && boot_id(&target.boot)
            && target.generation() == generation)
            .then_some(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, duplex};
    #[tokio::test]
    async fn fragmented_messages_survive_cancelled_reads_and_reject_oversize_before_allocating() {
        let (mut server, stream) = duplex(64);
        let mut reader = Incoming {
            io: BufReader::new(stream),
            pending: Vec::new(),
        };
        let header = [2u32.to_le_bytes().as_slice(), &[0], &4u32.to_le_bytes()].concat();
        server.write_all(&header[..3]).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(15), reader.next(32, 2))
                .await
                .is_err()
        );
        assert_eq!(reader.pending.len(), 3);
        server.write_all(&header[3..]).await.unwrap();
        server.write_all(&[0, 27, 128, 255]).await.unwrap();
        assert_eq!(
            reader.next(32, 2).await.unwrap(),
            (0, vec![0, 27, 128, 255])
        );
        server.write_all(&header).await.unwrap();
        assert!(
            reader
                .next(3, 2)
                .await
                .unwrap_err()
                .to_string()
                .contains("size limit")
        );
        assert!(reader.pending.capacity() < 1024);
    }

    #[test]
    fn reference_binds_canonical_local_address_core_record_and_linux_program() {
        let target = Target {
            core: "a".repeat(32),
            created_ms: 187,
            pid: 112,
            boot: "d4156dc9-a3ad-4d94-ad77-0303577e25cc".into(),
            ticks: 95,
        };
        let generation = target.generation();
        assert_eq!(Target::parse("t7", &generation), Some(target));
        for name in ["t0", "t07", "t+7", "west/t7", "build", "t7; ls"] {
            assert!(Target::parse(name, &generation).is_none());
        }
        assert!(Target::parse("t7", &format!("{generation}/extra")).is_none());
        assert!(Target::parse("t7", &generation.replace("/112/", "/0112/")).is_none());
    }

    #[tokio::test]
    async fn automatic_replies_use_the_negotiated_kind_and_keep_arbitrary_binary_bytes() {
        for automatic in [false, true] {
            let (stream, peer) = duplex(128);
            let (reader, writer) = tokio::io::split(stream);
            let mut viewer = Viewer {
                io: Incoming {
                    io: BufReader::new(reader),
                    pending: vec![],
                },
                writer,
                channel: 9,
                automatic,
            };
            viewer.automatic_input(&[0xff, 0, 0x1b]).await.unwrap();
            viewer.input(&[7]).await.unwrap();
            let mut peer = Incoming {
                io: BufReader::new(peer),
                pending: vec![],
            };
            assert_eq!(
                peer.next(1024, 9).await.unwrap(),
                (if automatic { 6 } else { 0 }, vec![0xff, 0, 0x1b])
            );
            assert_eq!(peer.next(1024, 9).await.unwrap(), (0, vec![7]));
        }
    }
}
