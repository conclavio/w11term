# RustMUSH SSH session resume, version 1

Enable the RustMUSH option in an SSH profile (`player.rustmush`). When the game implements this protocol, w11term can resume the same authenticated character in a live pane after an unexpected disconnect. The game determines whether the pilot still owns a seat. If the gateway lacks the extension or the lease expires, the normal SSH welcome/login remains interactive. Players can use the game's ordinary `BOARD` flow if eligible.

## Connection and identity

Each opt-in game pane opens a dedicated SSH connection. The same verified destination can be the instance's direct SSH endpoint or the instance-specific route through sshdoorway. To automatically return to the same instance through the doorway, save the service selector in the profile's SSH username. A different doorway process generation may handle the return. An unqualified interactive doorway menu can still be used as an ordinary login when the game subsystem is unavailable before splice.

The client verifies SSH host identity normally on every connection. A lease belongs to one live pane, destination identity and verified host-key fingerprint. Splitting/duplicating the pane starts a new lease. Closing it, reopening a saved workspace, ending the process, changing the verified fingerprint or reaching its disconnect deadline discards the lease. Durable Linux shell profiles use their own protocol.

SSH password and keyboard-interactive authentication happen outside the terminal screen. RustMUSH's legacy in-game `connect <account> <password>` welcome command echoes its arguments into the terminal, so an active VT recorder can capture a password typed there. Resume tickets always travel on the private SSH subsystem.

## Wire format

1. Before `shell`, the client requests its current PTY dimensions and sends an SSH environment request named `RUSTMUSH_RESUME_V1`. Its value is `issue:<pane>` on first use or `redeem:<pane>` on an eligible reconnect. `pane` is exactly 32 lowercase hex digits generated for this live pane. This public nonce correlates the primary channel to the control exchange. It conveys no authority. An environment request acknowledgement does not advertise support.
2. The client requests the ordinary shell. Independently it opens a second SSH session channel on the same authenticated connection and requests subsystem `rustmush-resume-v1` with want-reply. Only after explicit subsystem success does the client send a request. The server matches the nonce to the primary channel on this transport. It allows at most one active control exchange per primary channel and preserves the separate SFTP channel.
3. Each frame is a four-byte unsigned **big-endian byte length** followed by exactly that many bytes of UTF-8 JSON. The permitted length is 1–1024 bytes. Lengths are checked before allocation. Each channel carries one request, an optional authentication notice and one final response. The client closes that channel after completion. Unknown versions, malformed frames, unexpected success types and invalid tickets fail the exchange.

| Direction | JSON |
| --- | --- |
| Client, first issue | `{"v":1,"op":"issue","pane":"<pane>"}` |
| Client, reconnect | `{"v":1,"op":"redeem","pane":"<pane>","ticket":"<ticket>"}` |
| Server, optional intermediate notice | `{"v":1,"status":"auth_required"}` |
| Server, first issue complete | `{"v":1,"status":"issued","ticket":"<ticket>","grace_seconds":300}` |
| Server, resume complete | `{"v":1,"status":"redeemed","ticket":"<replacement>","grace_seconds":300}` |
| Server, unsuccessful final result | `{"v":1,"status":"expired"}`, `"denied"`, `"busy"`, or `"unavailable"` in the same status field |

A ticket is 32 server-generated random bytes encoded as 43 canonical unpadded base64url characters. `grace_seconds` is an integer from 1 through 300, inclusive. The client starts its local deadline at the **first detected disconnect** and preserves it across failed SSH attempts. The server enforces its own actual detach deadline.

## Gateway lifecycle

- On issue, wait until ordinary account authentication and character admission have succeeded. Bind a digest of the ticket to the instance, live pane nonce, authenticated account, owned character and exact game session. An account selector or a `play` SSH username by itself proves no identity. Bound the number of live and detached leases.
- On unexpected loss, let the usual game lifecycle detach the old connection, clear held input and apply its linkdead and seat rules. End eligibility on deliberate termination, account/character revocation or reaping. A gate must not create a new game session and then report that it resumed the old one.
- On redeem, check independent account authentication and current character ownership. If the normal in-band welcome needs input, promptly send one `auth_required`. Reserve the exact detached lease atomically. Refuse concurrent or mismatched claims. Establish a fresh control epoch and rehydrate the TUI at the current PTY dimensions from authoritative state. Carry only explicitly retained, non-executable view state. A fresh screen and live events must not replay previous keystrokes, bell, clipboard or OSC sound operations. Send `redeemed` with a newly generated ticket only after successful world admission and fresh view preparation. Release failed reservations. A lost final response can require an ordinary login.
- Keep issuance, redemption, sideband replies and tickets out of terminal bytes, account commands, diagnostic messages and persistent client settings. The client records game VT output only. Its per-pane event journal reports `game-resume` with one fixed `status` value. The sideband is independent of SSH `extended data` and all OSC handling.

## Timeouts and fallback

Negotiating the channel and obtaining the first redeem reply share a three-second budget, capped by the remaining lease lifetime. While redeem is undecided the client discards gameplay input and rejects raw send requests. Resize and new terminal output continue. A valid `auth_required` immediately unlocks input to the ordinary welcome flow. The final response may then wait until the earlier of the lease deadline and 300 seconds. A first issue can wait up to 300 seconds for login with input continuously enabled. Unsupported, silent, refused, expired, malformed and busy exchanges all leave the regular terminal usable. The client reports a restored session only on `redeemed`.

## Verification

`cargo test --test game_resume` starts loopback SSH peers, drops a real transport and checks pane identity, current PTY dimensions, ticket rotation, welcome authentication, suppression of pending input, fallback, and separation from the VT recording, screen and event journal. `cargo test --lib ssh::game_resume` checks the frame cap, credential format, host-key change and first-disconnect expiry.
