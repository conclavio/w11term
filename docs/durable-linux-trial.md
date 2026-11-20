# Linux PTY recovery trial

The Windows workspace keeps the layout. On Linux, zmx 1.0.1 can keep the foreground editor running and reconstruct its alternate screen when the attaching client disappears. The isolated PTY test recovered the original Vim process and unsaved buffer at 107 × 33 cells. Its first new keystroke stayed in insert mode. The capture contained no replay of the earlier clipboard write, notification, sound request or standalone bell. The application had to redraw its static kitty image. [Linux run](https://github.com/conclavio/w11term/actions/runs/44789901404)

An ordinary `zmx attach NAME` creates a session if it cannot find one. The client needs a random name and a generation label bound to the verified host and user. Check that label before resuming and again before accepting input. Supplying `/bin/false` as the fallback command on resume prevents an expiry race from silently creating an interactive replacement shell. Each remote session has its own generation. [zmx usage](https://zmx.sh/getting-started)

Run `sh tests/durable/run-linux.sh` on Linux with Vim and Python 3. The test uses a temporary home and socket directory. The separately hosted Windows tests exercise the SSH transport and GUI. Host-wide discovery may warrant a Herdr Core adapter later. Known references in a saved workspace are sufficient for this release.
