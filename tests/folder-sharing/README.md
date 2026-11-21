# Windows to Linux folder mount pilot

This fixture tests the transport proposed for per-profile folder sharing. A native Windows w11term session logs into a disposable Linux guest. Its interactive shell stays open while a second SSH channel starts the guest’s stock SSHFS process in passive mode. SSHFS sends SFTP requests over that channel to Windows. Linux reads a binary fixture through an ordinary FUSE mount path, attempts a write, unmounts, and checks the original shell.

The Windows test serves a fixed file from its temporary directory. Running it does not select a directory for sharing in w11term. General directory traversal, writable exports, reconnect and the profile UI are separate acceptance cases.

## Run the fixture

The **Windows folder mount pilot** GitHub Actions workflow builds the guest from Ubuntu packages and runs the test on Windows under QEMU software emulation. The guest SSH listener is forwarded to a Windows loopback port. The runner generates a fresh SSH identity and checks the guest host key against the key on its serial console.

To reproduce with your own test machines:

1. On a disposable x86-64 Ubuntu 24.04 builder, run `sudo bash tests/folder-sharing/guest/build.sh /tmp/sshfs-guest`. Copy its output directory to the Windows test machine.
2. In the Windows checkout, run `cargo test --locked --lib --no-run`. Cargo prints the path of the library test executable.
3. In PowerShell 7.4 or later, run the command below with QEMU and OpenSSH's `ssh-keygen.exe` available. Substitute the actual executable path printed by Cargo.

```powershell
./tests/folder-sharing/run-windows.ps1 -GuestDirectory C:\fixtures\sshfs-guest -TestExecutable 'C:\checkout\target\debug\deps\w11term-<hash>.exe'
```

The runner exits with the test outcome and shuts down its guest. See the [SSHFS manual](https://github.com/libfuse/sshfs/blob/ba33bab77006cfb81cbd6ecc44a8fc2728d24793/sshfs.rst#L150-L154) for the passive-mode protocol.
