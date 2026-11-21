# Windows to Linux folder mount acceptance test

This test grants an SSH profile read-only access to a temporary Windows directory through w11term’s folder-sharing API. An unprivileged account on a disposable Linux guest mounts it under `~/windows/` using stock SSHFS in passive mode. The test checks Linux’s mount table for the read-only flag and the identifier of the mount that w11term started.

Linux reads binary data, compares a 98,321-byte file under a Unicode name against its Windows SHA-256 digest, and tries to write. A sibling’s data must stay inaccessible through both an NTFS junction and a hard link. A second mount request at the same name must preserve the first mount. A nonempty Linux destination and a symlink to an empty directory must also be refused.

The original interactive shell must keep its PID through explicit unmount and dropping a mount handle. The test then disconnects the SSH transport and checks that the mount disappears. A fresh pane opens the same temporary profile and grant store, remounts the saved folder, and forgets it. Linux must see the mount disappear again while its new shell remains usable. Reopening the profile must show an empty grant list.

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
