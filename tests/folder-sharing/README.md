# Windows to Linux folder mount acceptance test

This test grants an SSH profile access to temporary Windows directories through w11term’s folder-sharing API. An unprivileged account on a disposable Linux guest mounts them under `~/windows/` using stock SSHFS in passive mode. w11term checks Linux’s mount table for the selected access mode, owner and mount generation.

Linux reads binary data, compares a 98,321-byte file under a Unicode name against its Windows SHA-256 digest, and tries to write. A sibling’s data must stay inaccessible through both an NTFS junction and a hard link. A second mount request at the same name must preserve the first mount. A nonempty Linux destination and a symlink to an empty directory must also be refused.

After unmounting, the test replaces the Windows directory at its saved path. The manager must report the replacement and leave Linux unmounted. Restoring the original directory must allow the saved grant to mount again.

The original interactive shell must keep its PID through explicit unmount and dropping a mount handle. The test saves a second folder, turns off **Mount on connect** for it, and checks that its live Linux mount keeps the same mount record and readable contents. Disconnecting the SSH transport must release both mounts.

On a fresh SSH connection, only the enabled folder mounts automatically. The test mounts the disabled folder manually, reads its Windows file, and verifies that its saved preference is still off. It then unmounts the folder and turns the preference on. The folder must stay unmounted until the next connection, where it receives a new mount generation. Forgetting both folders must leave an empty grant list. Each interactive shell must keep its PID while its mounts change.

The writable fixture creates and edits a file, moves it between directories, renames a directory and removes both. Windows checks the bytes while a Linux writer still holds its descriptor. A second SSH shell keeps that descriptor through disconnect and remount. The stale descriptor must fail, and a fresh append must succeed exactly once. Forgetting a single folder from another pane must stop its open writer while a sibling continues. Revoking the profile from another Windows process must then stop the sibling. A neighboring read-only grant and an outside hard link retain their original bytes.

The native UI test on the same guest explicitly enables write access, turns off **Mount on connect** on the saved row, opens the Files drawer at its Linux path, and checks binary transfers in both directions through the originating SSH connection. The Windows loopback UI test also saves a new folder with **Mount on connect** off and checks its state after restarting w11term.

The profile-wide revocation test gives two profiles on the same SSH account three distinct folders. Linux keeps a file open after reading its first byte. A second Windows process revokes one profile through the Settings API. Linux tries to read 64 MiB farther into that open file and tries new filenames in both revoked folders. Those reads must fail, both mounts must disappear, and the other profile must still read new data from its original mount. Both shells keep their PIDs. Once both SSH connections have closed, the test revokes the remaining profile offline. Reopening it must show no saved folders or mounts until a fresh selection authorizes sharing again.

## Run the fixture

The **Windows folder mount pilot** GitHub Actions workflow builds two guests from Ubuntu packages and runs them under QEMU on Windows. Both use the same Ubuntu GA kernel. The Ubuntu 26.04 userspace exercises the complete mount lifecycle. The Ubuntu 24.04 userspace checks that w11term identifies its FUSE 3.14 helper and refuses to start a mount while keeping the shell usable. Each guest's SSH listener is forwarded to a Windows loopback port. The runner generates a fresh SSH identity per boot and checks the host key against the guest serial console.

To reproduce with your own test machines:

1. On a disposable x86-64 Ubuntu 24.04 builder, run `sudo bash tests/folder-sharing/guest/build.sh /tmp/sshfs-guest resolute`. Copy its output directory to the Windows test machine. To reproduce the older-helper check, build the `noble` suite and pass `-LegacyFuse` to the Windows script.
2. In the Windows checkout, run `cargo test --locked --lib --no-run`. Cargo prints the path of the library test executable.
3. In PowerShell 7.4 or later, run the command below with QEMU and OpenSSH's `ssh-keygen.exe` available. Substitute the actual executable path printed by Cargo.

```powershell
./tests/folder-sharing/run-windows.ps1 -GuestDirectory C:\fixtures\sshfs-guest -TestExecutable 'C:\checkout\target\debug\deps\w11term-<hash>.exe'
```

The runner exits with the test outcome and shuts down its guest. See the [SSHFS manual](https://github.com/libfuse/sshfs/blob/ba33bab77006cfb81cbd6ecc44a8fc2728d24793/sshfs.rst#L150-L154) for the passive-mode protocol.
