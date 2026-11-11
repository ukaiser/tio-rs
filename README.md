# tio-rs

A work-in-progress Rust serial terminal with a keyboard-first Ratatui interface.
This is the initial implementation of [PLAN.md](PLAN.md), **not yet a full tio
replacement**. Tested so far with Unix pseudo-terminals, not physical hardware.

## Run

Requires Rust 1.88 or newer and Cargo.

```sh
cargo run --                         # device picker
cargo run -- /dev/ttyUSB0            # TUI, 115200 8N1
cargo run -- -b 9600 /dev/ttyUSB0
cargo run -- --list
cargo run -- --ui plain /dev/ttyUSB0 # transparent terminal, Ctrl-T q exits
printf 'hello\r\n' | cargo run -- --ui none /dev/ttyUSB0
```

The TUI requires terminal stdin and stdout. `--ui auto` (the default) selects
headless mode when either is redirected; headless mode requires a device path.
`--ui none` does not interpret keyboard commands. Input EOF drains pending
serial output and exits; use `--stay` to keep receiving after EOF. Device bytes
are written to stdout and diagnostics to stderr. `--no-reconnect` fails on an
absent device or disconnect; otherwise the selected path is retried.

In the picker, type to filter devices, use arrows and Enter to connect, or type
an unlisted device path and press Enter. F5 refreshes. Esc quits or returns to
the current session.

## TUI keys

All ordinary terminal keys, including Ctrl-C and arrows, go to the device.
Application commands start with **Ctrl-T**:

| Shortcut | Action |
| --- | --- |
| Ctrl-T Space | Searchable command palette |
| Ctrl-T ? | Help |
| Ctrl-T d | Device picker |
| Ctrl-T c | Change baud rate |
| Ctrl-T e | Toggle local echo |
| Ctrl-T [ | Scrollback; arrows/Page Up/Page Down, Esc returns to live view |
| Ctrl-T b | Send break |
| Ctrl-T q | Quit |
| Ctrl-T Ctrl-T | Send literal Ctrl-T |

Bracketed paste is confirmed before transmission. The TUI interprets device
output with a VT screen model instead of sending device escape sequences to the
host terminal. Plain mode forwards device bytes directly and is more suitable
for terminal behaviors not supported by the screen model.

## Current limitations

- Only baud can be changed in-session; data bits, stop bits, parity and flow
  control are startup flags. Unsupported settings are rejected by the serial
  driver. Mark/space parity and RS-485 are not implemented.
- Discovery currently shows paths only; enter paths manually if discovery misses
  a device. There are no profiles, stable topology IDs, or auto-new/latest modes.
- Reconnection follows the specified path. A path that is reused by a different
  device can therefore reconnect to the wrong device: prefer `/dev/serial/by-id/`
  where available.
- Logging, hex/line modes, timestamps, modem-line controls, mapping, X/YMODEM,
  Lua, sockets, and tio config import are not implemented yet.
- Terminal emulation and modified-key encoding are partial. Device status
  queries are not answered; use plain mode for applications needing them.
  Mouse reporting and terminal resize notifications to the device are not sent.
- Scrollback is bounded to 2,000 rows. Output displayed while scrolled back is
  retained only within that bound. Paste is limited to 1 MiB and requires
  confirmation; local echo is an approximation and is not wire confirmation.
- Only normal/error exits and the Ratatui panic hook restore the TUI terminal;
  signal handling and suspend/resume are not implemented. If interrupted,
  `stty sane` may be needed. SIGKILL cannot be handled.
- No hardware validation or performance guarantee yet; serial drivers and
  adapters vary. Do not rely on this tool for lossless safety-critical capture.

Run `cargo test` for unit and PTY integration tests. PTYs cannot validate modem
lines, physical flow control, or hardware disconnect behavior.

## License

GPL-2.0-or-later; see [LICENSE](LICENSE). The plan and implementation were
informed by the GPL-licensed tio source in the adjacent checkout.
# tio-rs
