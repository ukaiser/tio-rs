# tio-rs: Rust rewrite with a more comfortable Ratatui interface

## 1. Direction

Build a serial-device tool, not a general-purpose terminal multiplexer. Preserve
what makes tio useful—quick connections, sensible defaults, reliable reconnects,
and scriptable byte I/O—while making everyday controls visible and discoverable.

Use **Rust + Ratatui** for a compact, keyboard-first interface. Keep a plain
terminal mode and non-interactive operation; a full-screen interface must never
become a requirement for shell pipelines or automation.

Initial assumptions:

- Binary name: `tio-rs`, installed alongside `tio` without replacing it.
- Linux first; macOS follows after the serial and terminal design is validated.
- One active serial session per process for the first release.
- Default serial configuration: **115200, 8N1, no flow control**.
- Aim for practical compatibility, not immediate, bug-for-bug parity.
- Plan only: this document does not implement the application.

## 2. What the existing application does

Baseline: the local `../tio` checkout identifies itself as **tio 3.9** in
`../tio/meson.build`. This assessment is based on source and documentation
inspection, not a live session with serial hardware.

| Area | Existing behavior | Rewrite implication |
| --- | --- | --- |
| Serial configuration | Baud/data/stop/parity/flow settings, arbitrary baud rates, RS-485 | Capability-aware serial module; never silently approximate unsupported settings |
| Connection management | Wait for absent devices; reconnect after unplugging; direct/new/latest strategies | Explicit connection state and identity selection |
| Discovery | Device path, topology ID, uptime, driver, description; by-id/by-path listings | Searchable picker with stable identities and useful metadata |
| Interaction | Immediate key forwarding; `Ctrl-T` prefix commands | Preserve forwarding; add discoverable controls without stealing target-shell keys |
| Input/output | Normal/hex/line input; normal/hex/hexN output; mappings; delays; local echo | Separate byte processing from presentation |
| Observability | RX/TX counts, timestamps, logs, connection alerts | Persistent status and optional details pane |
| Profiles | INI defaults, named profiles, regex profiles, includes, CLI overrides | Import existing settings; show effective values and their origin |
| Automation | Lua read/write/expect, line control, connect hooks | Later compatibility milestone, using the same session ownership rules |
| Sharing | Unix/TCP IPv4/IPv6 listeners; external command I/O | Later headless-first milestone; define writer arbitration and security |
| Transfers | XMODEM-1K/CRC and YMODEM send paths | Later milestone with progress, cancellation, and exclusive serial ownership |

### Source observations that affect the design

- `../tio/src/tty.c` is about 3,000 lines and combines discovery, reconnects,
  keyboard commands, serial settings, presentation, and forwarding. Do not
  translate that file into one large Rust module.
- `../tio/src/main.c` uses an input thread and a connection loop. The rewrite
  should make session ownership explicit rather than share mutable global options.
- `../tio/src/tty.c::handle_command_sequence` prints menus and settings into the
  same terminal stream as device output. Use overlays and a separate event list.
- The current line-input implementation has editing/history behavior beyond what
  parts of the man page describe. Characterize the code rather than assuming
  every documented limitation is current.
- Documentation and implementation disagree in important places: the Lua
  implementation exposes `tio.read`, `tio.readline`, `tio.ttysearch`, and a
  Lua-pattern-based `tio.expect`; the man page describes older names and return
  values. Regex profile substitution uses `%mN` in the implementation.
- XMODEM receive appears in the command menu, but that branch calls
  `xymodem_send`. Do not treat that menu entry as verified working receive support.
- Existing topology IDs are short hashes of platform-specific topology paths.
  Preserve their derivation only after fixture tests, and detect ambiguity rather
  than assuming a short ID cannot collide.
- `../tio/TODO` lists raw file sending, socket-client connections, split I/O,
  WebSockets, and native Windows work as future ideas—not existing parity targets.

## 3. Comfortable, but still a serial terminal

### Main session layout

```text
┌ tio-rs · esp32       /dev/ttyUSB0   115200 8N1   CONNECTED ─────┐
│                                                             │
│  Device terminal output                                     │
│  > boot: ready                                              │
│  >                                                          │
│                                                             │
│                                                             │
├ Optional compose line: help_                    Enter sends ─┤
│ RX 12.4 KiB  TX 128 B │ Echo off │ Log off │ Follow │ NORMAL   │
└ Ctrl-T Space: commands   Ctrl-T ?: help   Ctrl-T q: quit ──────┘
```

The terminal owns almost all available space. Hide the compose line in normal
input mode. Put advanced information in overlays or an optional side pane, not
permanent dashboard panels. On small screens collapse status fields first;
retain the connection state and the command/help hint.

### Start and connect

- `tio-rs` opens a picker with **Devices**, **Profiles**, and **Recent** sections.
- `tio-rs /dev/ttyUSB0` and `tio-rs esp32` connect immediately.
- Picker supports search, arrows, Enter, refresh, and a manual path field for
  devices—including PTYs—that enumeration does not discover.
- Show device description, path, by-id identity, driver, and topology ID where
  available. Refresh without jumping the selection to another device.
- Opening the picker must not open ports or change modem lines just to inspect
  them. Connect only on an explicit selection.
- Permission, port-in-use, unsupported-setting, and absent-device errors need
  distinct messages and useful next actions; never suggest running the whole TUI
  as root as the default solution.

### Keyboard routing: a non-negotiable rule

While the terminal has focus, **unprefixed keys go to the device**, including
`Ctrl-C`, `Ctrl-D`, Escape, Tab, function keys, and arrow keys. Bare `q`, `/`, or
`?` are never application shortcuts in terminal focus.

Keep the familiar, configurable `Ctrl-T` prefix:

| Sequence | Action |
| --- | --- |
| `Ctrl-T Space` | Searchable command palette |
| `Ctrl-T ?` | Help and complete shortcut list |
| `Ctrl-T d` | Devices/profiles picker |
| `Ctrl-T c` | Effective settings and serial-settings editor |
| `Ctrl-T e` | Toggle local echo |
| `Ctrl-T f` | Start/stop capture, with visible destination |
| `Ctrl-T i` / `Ctrl-T o` | Change input/output mode |
| `Ctrl-T t` | Timestamp control |
| `Ctrl-T s` / `Ctrl-T L` | Statistics / modem-line details |
| `Ctrl-T g` / `Ctrl-T p` / `Ctrl-T b` | Toggle line / pulse line / send break |
| `Ctrl-T /` | Search scrollback |
| `Ctrl-T [` | Scrollback navigation and selection |
| `Ctrl-T q` | Quit; confirm only when a transfer or unsaved edit requires it |
| `Ctrl-T Ctrl-T` | Send literal prefix byte |

- In overlays/navigation focus, keys control the interface and are not forwarded.
  Escape returns to the session; show the current focus/mode explicitly.
- Show a small command hint when the prefix is pressed. Do not add a timeout that
  unexpectedly sends a partial command to the device. Escape cancels the prefix.
- Keep legacy shortcuts where implemented; list deferred commands honestly.
- If prefix interception is disabled, warn that keyboard controls are unavailable;
  document external signal-based termination and terminal recovery.
- Mouse capture is off by default so native terminal/tmux selection still works.
  Add opt-in mouse navigation only after keyboard behavior is solid.

### Daily-use improvements

1. **Command palette:** actions include current values, availability, and shortcuts.
   Search “baud”, “echo”, “log”, or “reset” instead of memorizing menus.
2. **Settings editor:** validated fields for baud, framing, flow, newline behavior,
   and delays. Explicit Apply; show reconnect requirements and hardware failures.
   An unsuccessful apply must not display the requested value as active.
3. **Compose mode:** editable line, history, and selectable CR/LF/CRLF terminator.
   Separate UI text from bytes already sent; normal mode stays immediate.
4. **Hex mode:** offsets, hex bytes, and ASCII column; validated byte composition.
   Keep existing `hexN` formatting available in plain mode.
5. **Scrollback:** bounded history, search, selection/export, and clear distinction
   between following live output and reading history. Pause follow, not reception.
6. **Reconnect:** persistent “Disconnected—waiting for …” status; history remains
   visible. Do not queue typed commands for replay after reconnect by default.
7. **Logging:** unmistakable recording indicator, destination, and surfaced write
   errors. Application notifications never become received device bytes.
8. **Paste:** recognize bracketed paste locally. Confirm multiline/large pastes
   before transmission; never interpret pasted prefix bytes as UI commands.
   Forward target bracketed-paste markers only if the target requested them.
9. **Accessibility:** respect `NO_COLOR`, provide text labels for state, avoid
   mandatory animations, and work at 80×24 and smaller useful layouts.

## 4. Terminal emulation is part of the rewrite

Ratatui draws widgets; it does **not** emulate the terminal connected to the
serial device. A text paragraph with ANSI stripping is insufficient for shell
prompts, carriage-return progress, cursor movement, colors, or remote editors.

Use three deliberately different output views:

- **Terminal:** feed received bytes into a maintained VT parser/screen model,
  then render its cells and cursor inside a Ratatui rectangle.
- **Monitor:** append-oriented text with timestamps and escaped controls; useful
  for firmware logs and search. Do not pretend this is a full terminal emulator.
- **Hex:** byte-exact inspection independent of UTF-8 or terminal interpretation.

Evaluate `vt100` with a compatible `tui-term` release in the first spike. Check
Ratatui version alignment, resizing, alternate screen, scrollback, cursor modes,
Unicode, terminal replies, and keyboard encoding before adopting it. If unsuitable,
choose another maintained parser or implement only a thin cell-rendering adapter;
do not write a new terminal emulator.

Requirements:

- Never write device escape sequences directly to the host terminal in TUI mode.
- Incremental decoding must handle UTF-8 and escape sequences split across reads.
  Keep raw bytes independently; invalid UTF-8 must not corrupt capture or hex view.
- Encode special keys according to the emulated terminal's supported modes.
  Plain mode remains the byte-transparent escape hatch for unsupported protocols.
- Support a documented, tested VT subset; do not advertise full xterm compatibility.
- Handle supported device-status queries internally where appropriate. Block host
  clipboard operations such as OSC 52 and other external side effects by default.
- Resizing changes the local screen model; a raw serial connection does not carry
  SSH-style window-size updates. Do not invent resize bytes to send to the device.
- Put timestamps in monitor view/gutters or formatted logs, not inside the remote
  terminal grid where they would break cursor addressing.
- Bound both scrollback lines and retained bytes, including unterminated lines
  and incomplete escape sequences. Switching views need not reconstruct unlimited
  history; describe the retained window.

## 5. Architecture

Start with **one Cargo package**, a library for testable behavior, and a small
binary. Split into workspace crates only when there is a concrete benefit.

```text
src/
  main.rs                 startup, exit codes, terminal lifetime
  lib.rs
  cli.rs                  arguments and UI/headless selection
  config.rs               profiles, validation, precedence, import
  device/                 discovery, identity, target resolution
  serial/                 platform access, capabilities, settings, modem lines
  session/                connection lifecycle, ordered I/O, operation ownership
  stream/                 RX/TX mapping, pacing, timestamps, byte accounting
  display/                terminal model, monitor history, hex projection
  capture.rs              raw capture and formatted logs
  app.rs                  input focus, actions, state transitions, effects
  tui/                    Ratatui layout, views, overlays, terminal guard
  plain.rs                transparent interactive and pipeline frontend
  transfer/               later: X/YMODEM operations
  automation/             later: Lua compatibility
  sharing/                later: sockets and external command bridge
```

### Deep modules and seams

| Module | Small interface | Complexity it owns |
| --- | --- | --- |
| Device catalog | Discover devices; resolve target and reconnect policy | OS metadata, aliases, topology IDs, filters, ambiguity |
| Serial | Open/configure; read/write; supported controls | OS handles, capabilities, exclusivity, partial writes, cleanup |
| Session | Submit commands; observe state/events | Reconnects, ordered writes, cancellation, operation ownership |
| Stream | Transform RX/TX bytes with explicit settings | Mapping order, incremental state, pacing, timestamps |
| Display | Ingest bytes; inspect a bounded view | VT state, monitor records, hex projection, scrollback |
| Capture | Start/write/stop | Destinations, append policy, stripping, buffering, failures |
| App | Reduce event to new state and requested effects | Focus, forms, command availability, user intent |

The serial seam has a real hardware adapter and a deterministic in-memory test
adapter. The device catalog has an OS adapter and fixture-backed tests. Avoid
creating a trait for every file or forwarding function.

### Runtime and ownership

Use Tokio for serial I/O, timers, cancellation, and later socket operations;
Ratatui rendering and Crossterm event handling belong to the frontend. Validate
this combination in the initial spike before committing to dependency versions.

- A single session owner orders all writes and controls the serial connection.
  No UI widget, script, or transfer gets its own competing serial reader.
- Connection states: `Idle → Resolving → Waiting → Connecting → Connected`;
  disconnects return to `Waiting` if enabled, otherwise end with a reason.
  Configuration/permission errors are surfaced, not retried silently forever.
- Operation state is separate: `Interactive`, `Transfer`, `Script`, or `Exec`.
  Exclusive operations gate keyboard/socket writes and own the RX consumer until
  completion/cancellation. A settings overlay alone does not stop reception.
- Commands carry completion results. TX statistics count successful wire writes,
  not queued bytes; RX statistics count received bytes before display transforms.
- Use connection generations to reject stale events and pending writes after
  reconnect. Never resume a transfer or replay a partial command automatically.
- RX processing feeds capture and bounded display state without depending on a
  draw call. Publish coalescible invalidations/snapshots, not one UI event per byte.
- Bound queues and use batched I/O. Coalesce redraw notifications, not serial
  payloads. Define backpressure and cancellation for every producer.
- If capture cannot keep up or the disk fills, stop that capture with an explicit
  error; strict headless capture should exit nonzero. Never silently claim a
  complete recording. Hardware without flow control cannot guarantee no loss.
- Cap redraws, for example at 30 FPS while active; sleep when idle. Input and
  disconnect handling must not wait for the next full-screen render.
- Use monotonic time for deadlines/delays and wall time only for human timestamps.
- RAII guards plus best-effort panic/signal cleanup restore raw mode, cursor,
  mouse/paste modes, and alternate screen. Cover suspend/resume on Unix. Document
  that forced termination such as SIGKILL cannot run cleanup.

### Candidate dependencies

Versions and compatibility are to be verified during implementation, not assumed
from this plan.

| Purpose | Candidate |
| --- | --- |
| Interface | `ratatui`, `crossterm` |
| Runtime / serial | `tokio`, `tokio-serial` and underlying `serialport` |
| Unix-specific controls | `nix`/`rustix`, with narrowly scoped `libc` only where necessary |
| Terminal model | `vt100` + compatible `tui-term`, subject to spike |
| CLI / configuration | `clap`, `serde`, `toml`, XDG path helper |
| Errors / diagnostics | `thiserror`, `anyhow`, `tracing` to a file/stderr—not the TUI stream |
| Later Lua compatibility | `mlua` with an explicitly chosen Lua version |
| Verification | Ratatui `TestBackend`, `insta`, `proptest`, Unix PTY fixtures |

Do not assume the serial crate covers mark/space parity, arbitrary baud rates,
RS-485, line pulses, or every discovery field. Check each on each supported OS;
extend the serial implementation where justified or reject the setting clearly.
CTS/DSR/DCD/RI are normally status inputs; show them as read-only unless the
actual hardware/platform exposes a supported operation.

## 6. CLI, profiles, and data contracts

### Frontend selection

```sh
tio-rs                            # picker in a TTY
tio-rs /dev/ttyUSB0 -b 115200      # immediate TUI session
tio-rs esp32                      # named profile
tio-rs --ui plain /dev/ttyUSB0     # no full-screen interface
tio-rs --list                     # script-friendly discovery
printf 'status\r\n' | tio-rs --ui none /dev/ttyUSB0
```

Use `--ui auto|tui|plain|none`. Auto selects TUI only when stdin and stdout are
terminals with usable terminal capabilities. Explicit TUI with redirected handles
fails clearly rather than consuming piped data or emitting cursor controls.
Discovery/help/version never initialize the TUI. Plain interactive mode reads
terminal bytes directly rather than round-tripping through decoded key events.

For headless operation:

- Device data goes to stdout; diagnostics go to stderr. No UI, prefixes, or ANSI
  decorations are injected into the data stream.
- Default to raw I/O without transforms; explicit mapping/format flags can change
  the stream. Label formatted output as such.
- At stdin EOF, drain successful writes and exit by default, matching the main
  send-pipeline use case. Add explicit `--stay` and response-wait/idle-timeout
  options for workflows that need RX after EOF; test their exit semantics.
- Distinguish success, configuration error, connection/I/O failure, and timeout
  with documented exit codes. Actual SIGINT ends a headless process; typed Ctrl-C
  in an interactive raw serial session goes to the device.

### Configuration

- Native config: `$XDG_CONFIG_HOME/tio-rs/config.toml`, falling back to
  `$HOME/.config/tio-rs/config.toml`.
- Precedence: built-in defaults < global config < selected profile < CLI flags <
  explicitly applied session changes. Show provenance in the settings overlay.
- Runtime changes are temporary unless the user explicitly saves a profile.
  Validate first, write atomically, and never overwrite the original tio config.
- Keep recent targets and UI preferences separate from connection profiles.
- Add an explicit tio INI import command in the compatibility milestone. Import
  includes with cycle detection and preserve `%mN` substitutions where supported.
- POSIX regex and Rust regex are not interchangeable. Define the supported
  profile-pattern subset; reject unsupported imports with actionable diagnostics.
- Importing must not execute Lua or shell commands. Require approval before an
  imported profile first runs an automatic script or command.
- Unsupported keys/options produce warnings or errors—not silently successful
  configurations. Do not claim full CLI/config compatibility before testing it.

### Bytes, mappings, and captures

Use unambiguous names internally: **RX = device → application** and
**TX = application → device**. Existing `I*`/`O*` flags need a documented mapping
to those directions based on observed behavior, not their ambiguous labels.

- Capture raw RX before presentation transforms; optional TX capture is separate
  or stored as direction-tagged records. Never mix sent bytes into an unlabeled
  received-byte capture.
- Formatted logs may add timestamps and strip escapes; label them differently
  from raw captures. Do not promise the old text-log format in the first release.
- Apply TX mappings/pacing once through the session pipeline, with operation
  policies explicit. Transfer protocols bypass text mappings, echo, and pacing.
- Compatibility fixtures must pin mapping precedence and combined flags,
  especially transformations currently performed by termios versus user code.
- Keep local echo separate from physical RX; UI and logs must not count it twice.

## 7. Scope and delivery milestones

Each milestone ends in a usable, testable slice. Do not implement every parity
feature before validating the interaction model.

### M0 — Risk spikes and behavior fixtures

- Connect a PTY-backed serial session and a physical USB-UART where available.
- Render a terminal model inside Ratatui; test remote shell arrows, Ctrl-C,
  carriage-return updates, ANSI colors, alternate screen, paste, and resizing.
- Prove prefix routing, literal prefix forwarding, and terminal restoration.
- Audit serial capabilities and candidate crate/version compatibility.
- Capture existing tio behavior for reconnect selection, profiles, mappings,
  Lua return values, and pipe EOF handling. Record intentional differences.

**Exit:** terminal/input approach and dependency choices are validated, with no
unresolved blocker to a usable serial shell inside the TUI.

### M1 — First usable release: serial session plus friendly controls

- Cargo project, CLI, session state machine, serial ownership, Linux support.
- Direct path connection and picker; basic profile selection with native config.
- Common framing/flow settings with capability errors for unsupported values.
- Default reconnect to the selected target, preserving the requested identity.
- Ratatui terminal pane, compact status, help, command palette, settings editor.
- Normal input, local echo, safe paste, basic bounded scrollback/follow control.
- Plain interactive mode and byte-clean headless send/receive operation.
- Actionable errors and terminal cleanup on normal/error exits.

**Exit:** select a device, connect, use a serial shell, change supported settings,
unplug/replug, and quit cleanly without knowing hidden shortcuts. PTY CI passes.

### M2 — Everyday embedded-development workflow

- Editable line composer with history and newline selection.
- Hex input/output, monitor view, scrollback search/export.
- Raw capture, formatted logs, timestamps, accurate RX/TX statistics.
- DTR/RTS controls, timed pulses, break, and readable status-line state.
- Mapping pipeline, character/line pacing, capture failure handling.
- Saved profiles, recent targets, compact layouts, accessibility polish.

**Exit:** routine shell, boot-log, and binary-protocol workflows work without
leaving the session or polluting device output with application messages.

### M3 — Advanced connection and configuration compatibility

- Direct/new/latest strategies, exclusion filters, stable by-id/by-path identity.
- Verified topology-ID compatibility and collision handling on Linux.
- Explicit tio INI import, includes, named/pattern profiles, CLI parity inventory.
- Arbitrary baud, mark/space parity, and Linux RS-485 where supported.
- macOS discovery/serial support with a published capability matrix.

**Exit:** tested compatibility for the supported profile/CLI subset and clear
reports for unsupported platform features; no silent setting substitutions.

### M4 — Transfers and automation

- XMODEM-CRC/1K and YMODEM sending with progress, retries, timeouts, and cancel.
- Protocol fixtures ensure control bytes never leak into normal TX/RX handling.
- Lua compatibility based on actual `script.c` behavior and example scripts:
  reads, writes, Lua-pattern expect, line control, discovery, connect hooks.
- Define timeout compatibility explicitly: current script reads/expect can use
  per-read inactivity timeouts, which differ from a total operation deadline.
- Exclusive operation ownership; UI remains responsive and clearly shows the
  active script/transfer. Bounded buffers and cancellable waits; interrupt hooks
  for CPU-bound Lua. Scripts are trusted code, not a security sandbox.
- XMODEM receive only as a separately verified feature, not assumed parity.

**Exit:** deterministic protocol tests and real-device smoke tests pass;
cancellation returns the connection to a known state.

### M5 — Sharing, external commands, and release hardening

- Unix/TCP IPv4/IPv6 listeners and `--exec`, using the session's I/O ownership.
- Define multi-client ordering, disconnect behavior, and slow-client limits.
  No unbounded buffering while the serial device is absent.
- Default network listeners to loopback. Require explicit opt-in for external
  exposure, warning that raw TCP has neither authentication nor encryption.
- Safe Unix-socket permissions, cleanup, and child-process termination/reaping.
- Complete compatibility documentation, shell completions, man page, packaging,
  performance runs, release binaries, and license/dependency review.

**Exit:** every claimed feature has tests, unsupported features are explicit,
and supported-platform release artifacts pass smoke tests.

### Explicitly deferred

Multiple simultaneous sessions/tabs, native Windows support, custom terminal
emulation, WebSockets, socket-client targets, split I/O, multi-port monitoring,
plugin systems, firmware flashing, and protocol decoding. Raw file send can be a
small later addition but is not a prerequisite for parity with this checkout.

## 8. Verification and acceptance criteria

### Automated testing

- **State transitions:** connect/fail/retry/quit, no-reconnect, stale-generation
  events, settings apply failure, disconnect during write or exclusive operation.
- **Byte processing:** all 256 byte values, split UTF-8/escape sequences, mapping
  combinations, partial writes, local echo accounting, pacing with a fake clock.
- **Input routing:** unprefixed Ctrl-C/arrows/Escape reach the target; overlays,
  palette searches, prefix commands, and paste cannot accidentally transmit UI text.
- **Display:** Ratatui `TestBackend` snapshots at 80×24 and smaller sizes; terminal,
  monitor, hex, picker, reconnect, and error states. Snapshots do not replace byte
  and state assertions.
- **PTY integration:** byte-exact round trips, stdin EOF, formatted/raw output,
  disconnect/reconnect through a replaceable target symlink, capture failures,
  cancellation, and terminal restoration. PTYs do not prove hardware features.
- **Compatibility:** golden fixtures for existing CLI/config examples, topology
  paths/IDs, `%mN` expansion, mappings, Lua behavior, and documented deviations.
- **Robustness:** property/fuzz tests for malformed streams, incomplete ANSI,
  invalid hex input, giant lines, include cycles, and repeated hot-plug events.

### Real hardware and performance checks

- USB-UART unplug/replug, renumbering with a stable identity, busy-port behavior,
  common and nonstandard baud rates, supported parity/flow, DTR/RTS and RS-485.
- Linux CI from M1; macOS CI when M3 is introduced. Hardware-specific behavior
  is reported as unverified until exercised on actual hardware.
- Sustain 115200 and 921600 baud 8N1-equivalent traffic with a PTY generator;
  verify capture byte counts and hashes independently of rendering.
- Proposed responsiveness target: command overlays react within 100 ms under
  sustained traffic on a documented reference machine.
- Long-running soak confirms bounded memory and responsive reconnect/quit.
  Idle sessions should not continuously redraw or busy-poll.

### Release gate

`cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
and the supported-feature test matrix pass. Complete a dependency/license audit,
manual terminal-recovery checklist, and hardware capability checklist before
claiming feature parity.

## 9. Licensing and implementation policy

This project is licensed **GPL-2.0-or-later**, as is the inspected tio source
(`../tio/LICENSE`). Review the implementation's provenance and dependency
licenses before distribution, and retain any required attribution.

## 10. Recommended first implementation slice

Implement **M0, then M1**. Start with one PTY and one USB-UART rather than porting
all existing options. The first demo should show:

1. Open `tio-rs`, find a device, and press Enter.
2. See a working serial shell inside Ratatui.
3. Press `Ctrl-T Space`, search “baud”, and apply a validated setting.
4. Forward Ctrl-C and arrow keys correctly to the target.
5. Unplug/replug while the interface remains responsive and history stays visible.
6. Quit with the host terminal restored; run a byte-clean pipeline using the same
   session implementation without initializing Ratatui.

That proves the central promise: **tio's quick serial workflow, with controls you
can find and understand without sacrificing reliable terminal I/O.**
