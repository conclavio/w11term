# Typing latency benchmark

This setup compares visible typing latency through a loopback SSH connection. No remote VPS, local shell, shell startup, line editor, or network path outside this machine is involved.

## Start

From the project root:

```powershell
.\bench\setup.ps1
.\bench\start-server.ps1
.\bench\launch.ps1 typometer
.\bench\launch.ps1 w11term
```

The setup downloads [Typometer 1.0.1](https://github.com/pavelfatin/typometer/releases/tag/v1.0.1) and a portable Eclipse Temurin Java 8 JRE into ignored `.local/tools`. It verifies pinned archive checksums, builds release executables, and records environment versions and binary hashes. It does not install Java system-wide or change PATH.

The SSH server listens exclusively on `127.0.0.1:22222`, accepts SSH `none` authentication for `bench`, and never launches a process. It echoes printable ASCII immediately, supports Backspace/Delete (Typometer erases reference patterns and measured rows), and uses a steady bar cursor. Ctrl+L clears the screen; Ctrl+C or Ctrl+D disconnects. There is no per-keystroke logging. TCP_NODELAY is enabled in the echo server and w11term's SSH client.

The server creates its own persistent test host key, client fingerprint pin, and scoped OpenSSH `known_hosts` in `.local/benchmark`. It does not use the `hostinger` identity or change the user's SSH trust database. Alternate port: `start-server.ps1 -Port 22223` (stop the running server first).

## Compare the clients

Launch one candidate at a time:

```powershell
.\bench\launch.ps1 w11term
.\bench\launch.ps1 wezterm
.\bench\launch.ps1 windows-terminal
```

Additional comparisons:

```powershell
.\bench\launch.ps1 wezterm-openssh
.\bench\launch.ps1 windows-terminal-preview
```

| Candidate | Path being measured |
| --- | --- |
| w11term | Native input, embedded russh, loopback echo, VT engine, GPU renderer |
| WezTerm | Native input, built-in SSH client, loopback echo, terminal renderer |
| Windows Terminal | Input, ConPTY/OpenSSH, loopback echo, terminal renderer |
| WezTerm / OpenSSH | Input, local PTY/OpenSSH, loopback echo, terminal renderer |

Treat these as complete client configurations. Windows Terminal's local console/SSH process is part of its result. The additional WezTerm/OpenSSH case helps distinguish differences associated with that path; it does not isolate a single stage or establish a renderer-only ranking.

WezTerm uses only `bench/wezterm.lua`. Windows Terminal gets a dedicated additive [profile fragment](https://learn.microsoft.com/en-us/windows/terminal/json-fragment-extensions) at `%LOCALAPPDATA%\Microsoft\Windows Terminal\Fragments\w11term-benchmark\benchmark.json`, shared by stable and preview. Existing settings files and defaults are preserved. The fragment can be removed after closing its benchmark windows to remove that profile.

Both comparison profiles use Cascadia Mono 12 pt (w11term uses 16 DIPs), an opaque dark background, and a bar cursor. WezTerm and Windows Terminal start at 110 columns by 36 rows. w11term derives its grid from window size; resize to a similar grid and record the actual size. Font metrics, padding, and chrome mean equal outer window sizes do not necessarily mean equal grids. Keep default renderer/frame-rate behavior and record any subsequent tuning as a separate configuration.

## Run Typometer

1. Put the candidate terminal and Typometer on the **same primary monitor**, with unchanged refresh rate and scaling. Keep the echo input row visible and uncovered. Move Typometer aside if it overlaps that row.
2. Leave other terminals idle, finish builds, use the same power mode, and avoid changing machine load during comparisons.
3. Use one 200-character warm-up run. For recorded runs set **Chars: 1000**, **Delay: 50 ms**, **Async: off**, **Intermediate pauses: off**, **Native API: on**. Use those same parameters for every candidate.
4. Set a distinct measurement title, e.g. `w11term-release-sync50-run1`. Click Typometer's **Benchmark** button, then focus the echo terminal when prompted. Do not type, resize, or move windows during the run. Typometer inserts dots and erases them between rows.
5. Repeat five times per candidate, rotating candidate order to reduce drift. Keep warm-up and recorded measurements separate.
6. For rapid-input testing, run a second series with **Async: on**, **Delay: 10 ms**. Label and analyze that series separately. Async results have different measurement behavior and should not be pooled with synchronous results.
7. Export **raw CSV** using Typometer's export action. The working directory is `.local/benchmark/results`; save each series there. Keep measurement titles unique.

Typometer requires a clean reference pattern. If detection fails, confirm the row is blank (Ctrl+L), the cursor is a bar, the font is monospace, the terminal is on the primary monitor, and its text is unobscured. If Native API fails, test the fallback with Native API off for **every** candidate and label it as a separate series. Java 8 and this older tool may behave differently under mixed-DPI or multi-monitor configurations; do not mix configurations silently.

## Analyze

```powershell
.\bench\summarize.ps1 .\.local\benchmark\results\comparison.csv | Format-Table -AutoSize
```

Each exported measurement is a separate CSV column. The script reports sample count, median, p95, p99, mean, and maximum in milliseconds. Percentiles use nearest rank. Empty cells in unequal-length columns are ignored; valid samples, including stalls and zeros, are retained. Compare individual runs before pooling. Do not subtract independent percentile values to infer the cost of a pipeline stage.

Environment snapshots in the results directory contain OS, CPU/GPU, terminal versions, and hashes of the release binaries and Cargo.lock. Re-run `bench/record-environment.ps1` after rebuilding. Record monitor refresh/scaling, power mode, actual terminal dimensions, and test parameters alongside each CSV.

This measures injected-input-to-screen-capture latency, not physical keyboard-to-photon latency. It is not a scrolling, graphics throughput, or VT conformance benchmark. The current parser-only `--benchmark` remains a separate test. See [Typometer's measurement description](https://pavelfatin.com/typometer/) for its sampling method and synchronous/asynchronous distinction.

## Stop

Close the benchmark terminal windows and Typometer, then:

```powershell
.\bench\stop-server.ps1
```

The stop script checks the executable path before terminating the recorded PID. All downloaded tools, keys, logs, server state, and results stay under ignored `.local/`.
