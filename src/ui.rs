use crate::{
    input::{encode_key, is_prefix},
    session::{Command, Event, Session, Settings},
};
use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent, KeyCode,
        KeyEventKind,
    },
    execute,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use std::{io::stdout, num::NonZeroU16, sync::mpsc::TrySendError, time::Duration};

#[derive(PartialEq, Eq)]
enum Mode {
    Picker,
    Terminal,
    Help,
    Palette,
    Baud,
    Scroll,
    Paste,
}

struct App {
    settings: Settings,
    session: Option<Session>,
    parser: vt100::Parser,
    mode: Mode,
    devices: Vec<String>,
    filter: String,
    selected: usize,
    prefix: bool,
    connected: Option<u64>,
    status: String,
    rx: u64,
    tx: u64,
    echo: bool,
    baud_input: String,
    paste: String,
    quit: bool,
    fatal: Option<String>,
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(stdout(), DisableBracketedPaste);
        let _ = ratatui::try_restore();
    }
}

pub fn run(settings: Settings, picker: bool) -> Result<()> {
    let mut terminal = ratatui::try_init().context("initializing terminal")?;
    let _guard = TerminalGuard;
    execute!(stdout(), EnableBracketedPaste)?;
    let mut app = App::new(settings, picker);
    while !app.quit {
        app.poll_session();
        let size = terminal.size()?;
        let rows = size.height.saturating_sub(2).max(1);
        let cols = size.width.max(1);
        if app.parser.screen().size() != (nz(rows), nz(cols)) {
            app.parser.set_size(nz(rows), nz(cols));
        }
        terminal.draw(|frame| app.draw(frame))?;
        if event::poll(Duration::from_millis(30))? {
            match event::read()? {
                TermEvent::Key(key) if key.kind != KeyEventKind::Release => app.key(key),
                TermEvent::Paste(text) if app.mode == Mode::Terminal => {
                    if text.len() > 1024 * 1024 {
                        app.status = "Paste rejected: exceeds 1 MiB".into();
                    } else {
                        app.paste = text;
                        app.mode = Mode::Paste;
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(session) = &app.session {
        let _ = session.commands.try_send(Command::Shutdown);
    }
    if let Some(message) = app.fatal {
        anyhow::bail!(message);
    }
    Ok(())
}

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).expect("nonzero")
}

impl App {
    fn new(settings: Settings, picker: bool) -> Self {
        let mut app = Self {
            settings,
            session: None,
            parser: vt100::Parser::new(nz(22), nz(80), 2000),
            mode: if picker { Mode::Picker } else { Mode::Terminal },
            devices: vec![],
            filter: String::new(),
            selected: 0,
            prefix: false,
            connected: None,
            status: "Select a device".into(),
            rx: 0,
            tx: 0,
            echo: false,
            baud_input: String::new(),
            paste: String::new(),
            quit: false,
            fatal: None,
        };
        if picker {
            app.refresh();
        } else {
            app.connect();
        }
        app
    }
    fn refresh(&mut self) {
        match serialport::available_ports() {
            Ok(ports) => {
                self.devices = ports.into_iter().map(|p| p.port_name).collect();
                self.devices.sort();
                self.status = format!(
                    "{} device(s); type a path or filter; F5 refresh",
                    self.devices.len()
                );
            }
            Err(e) => {
                self.status = format!("Discovery failed: {e}; type a path manually");
            }
        }
        self.selected = 0;
    }
    fn filtered(&self) -> Vec<&str> {
        self.devices
            .iter()
            .filter(|p| p.to_lowercase().contains(&self.filter.to_lowercase()))
            .map(String::as_str)
            .collect()
    }
    fn connect(&mut self) {
        if let Some(old) = self.session.take() {
            let _ = old.commands.try_send(Command::Shutdown);
        }
        self.connected = None;
        self.status = format!("Connecting to {}...", self.settings.target);
        self.session = Some(Session::spawn(self.settings.clone()));
        self.mode = Mode::Terminal;
    }
    fn poll_session(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        // Bound work per frame without dropping bytes.
        for _ in 0..128 {
            let Ok(event) = session.events.try_recv() else {
                break;
            };
            match event {
                Event::Connected { generation, baud } => {
                    self.connected = Some(generation);
                    self.settings.baud = baud;
                    self.status = "Connected".into();
                }
                Event::Waiting(reason) => {
                    self.connected = None;
                    self.status = format!("Waiting: {reason}");
                }
                Event::Received(bytes) => {
                    self.rx += bytes.len() as u64;
                    self.parser.process(&bytes);
                }
                Event::Sent(n) => self.tx += n as u64,
                Event::Drained => {}
                Event::BaudChanged(baud) => {
                    self.settings.baud = baud;
                    self.status = format!("Baud set to {baud}");
                }
                Event::Notice(message) => self.status = message,
                Event::Finished(reason) => {
                    self.connected = None;
                    self.status = reason.clone();
                    if !self.settings.reconnect {
                        self.fatal = Some(reason);
                        self.quit = true;
                    }
                }
            }
        }
    }
    fn send(&mut self, bytes: Vec<u8>) {
        let (Some(generation), Some(session)) = (self.connected, &self.session) else {
            self.status = "Disconnected: input not sent".into();
            return;
        };
        match session.commands.try_send(Command::Write {
            generation,
            bytes: bytes.clone(),
        }) {
            Ok(()) => {
                if self.echo {
                    self.parser.process(&bytes);
                }
            }
            Err(TrySendError::Full(_)) => self.status = "Input queue full: input not sent".into(),
            Err(TrySendError::Disconnected(_)) => self.status = "Serial worker stopped".into(),
        }
    }
    fn command(&mut self, cmd: Command) {
        if let Some(session) = &self.session {
            if session.commands.try_send(cmd).is_err() {
                self.status = "Could not queue command".into();
            }
        } else {
            self.status = "Not connected".into();
        }
    }
    fn key(&mut self, key: event::KeyEvent) {
        match self.mode {
            Mode::Picker => match key.code {
                KeyCode::Esc => {
                    if self.session.is_some() {
                        self.mode = Mode::Terminal
                    } else {
                        self.quit = true
                    }
                }
                KeyCode::F(5) => self.refresh(),
                KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down => {
                    self.selected = (self.selected + 1).min(self.filtered().len().saturating_sub(1))
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.selected = 0;
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.selected = 0;
                }
                KeyCode::Enter => {
                    let target = self
                        .filtered()
                        .get(self.selected)
                        .map(|s| (*s).to_owned())
                        .or_else(|| (!self.filter.is_empty()).then(|| self.filter.clone()));
                    if let Some(target) = target {
                        self.settings.target = target;
                        self.filter.clear();
                        self.connect();
                    }
                }
                _ => {}
            },
            Mode::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
                    self.mode = Mode::Terminal;
                }
            }
            Mode::Palette => match key.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.mode = Mode::Terminal;
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.selected = 0;
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.selected = 0;
                }
                KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down => {
                    self.selected =
                        (self.selected + 1).min(self.palette_items().len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some((_, c)) = self.palette_items().get(self.selected).copied() {
                        self.filter.clear();
                        self.action(c);
                    }
                }
                _ => {}
            },
            Mode::Baud => match key.code {
                KeyCode::Esc => self.mode = Mode::Terminal,
                KeyCode::Backspace => {
                    self.baud_input.pop();
                }
                KeyCode::Char(c) if c.is_ascii_digit() && self.baud_input.len() < 10 => {
                    self.baud_input.push(c)
                }
                KeyCode::Enter => match self.baud_input.parse::<u32>() {
                    Ok(n) if n > 0 => {
                        self.command(Command::SetBaud(n));
                        self.status = "Applying baud...".into();
                        self.mode = Mode::Terminal;
                    }
                    _ => self.status = "Enter a positive baud rate".into(),
                },
                _ => {}
            },
            Mode::Scroll => {
                let screen = self.parser.screen_mut();
                let offset = screen.scrollback();
                match key.code {
                    KeyCode::Up => screen.set_scrollback(offset.saturating_add(1)),
                    KeyCode::Down => screen.set_scrollback(offset.saturating_sub(1)),
                    KeyCode::PageUp => screen.set_scrollback(offset.saturating_add(20)),
                    KeyCode::PageDown => screen.set_scrollback(offset.saturating_sub(20)),
                    KeyCode::Esc | KeyCode::Char('q') => {
                        screen.set_scrollback(0);
                        self.mode = Mode::Terminal;
                    }
                    _ => {}
                }
            }
            Mode::Paste => match key.code {
                KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
                    let mut bytes = std::mem::take(&mut self.paste).into_bytes();
                    if self.parser.screen().bracketed_paste() {
                        let mut wrapped = b"\x1b[200~".to_vec();
                        wrapped.append(&mut bytes);
                        wrapped.extend_from_slice(b"\x1b[201~");
                        bytes = wrapped;
                    }
                    self.mode = Mode::Terminal;
                    self.send(bytes);
                }
                KeyCode::Esc | KeyCode::Char('n' | 'N') => {
                    self.paste.clear();
                    self.mode = Mode::Terminal;
                }
                _ => {}
            },
            Mode::Terminal => {
                if self.prefix {
                    self.prefix = false;
                    if is_prefix(key) {
                        self.send(vec![20]);
                    } else if let KeyCode::Char(c) = key.code {
                        self.action(c);
                    } else if key.code != KeyCode::Esc {
                        self.status = "Unknown prefix command; Ctrl-T ? for help".into();
                    }
                } else if is_prefix(key) {
                    self.prefix = true;
                } else if let Some(bytes) =
                    encode_key(key, self.parser.screen().application_cursor())
                {
                    self.send(bytes);
                }
            }
        }
    }
    fn palette_items(&self) -> Vec<(&'static str, char)> {
        [
            ("Baud rate", 'c'),
            ("Devices", 'd'),
            ("Toggle local echo", 'e'),
            ("Scrollback", '['),
            ("Send break", 'b'),
            ("Help", '?'),
            ("Quit", 'q'),
        ]
        .into_iter()
        .filter(|(name, _)| name.to_lowercase().contains(&self.filter.to_lowercase()))
        .collect()
    }
    fn action(&mut self, c: char) {
        match c {
            '?' => self.mode = Mode::Help,
            ' ' => {
                self.filter.clear();
                self.selected = 0;
                self.mode = Mode::Palette;
            }
            'd' => {
                self.filter.clear();
                self.refresh();
                self.mode = Mode::Picker;
            }
            'c' => {
                self.baud_input = self.settings.baud.to_string();
                self.mode = Mode::Baud;
            }
            'e' => {
                self.echo = !self.echo;
                self.status = format!("Local echo {}", if self.echo { "on" } else { "off" });
            }
            '[' => self.mode = Mode::Scroll,
            'b' => self.command(Command::Break),
            'q' => self.quit = true,
            _ => self.status = "Unknown prefix command; Ctrl-T ? for help".into(),
        }
    }
    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);
        let state = if self.connected.is_some() {
            "CONNECTED"
        } else {
            "DISCONNECTED"
        };
        frame.render_widget(
            Paragraph::new(format!(
                " tio-rs | {} | {} | {}",
                self.settings.target, self.settings.baud, state
            ))
            .style(Style::default().add_modifier(Modifier::BOLD)),
            chunks[0],
        );
        self.draw_screen(frame, chunks[1]);
        let hint = if self.prefix {
            "prefix: Space commands · ? help · q quit"
        } else if self.mode == Mode::Scroll {
            "SCROLL: ↑/↓ PgUp/PgDn · Esc follow"
        } else {
            "Ctrl-T Space commands · Ctrl-T ? help"
        };
        frame.render_widget(
            Paragraph::new(format!(
                " RX {} TX {} | echo {} | {} | {}",
                self.rx,
                self.tx,
                if self.echo { "on" } else { "off" },
                self.status,
                hint
            )),
            chunks[2],
        );
        match self.mode {
            Mode::Picker => {
                let mut lines: Vec<ListItem> = self.filtered().iter().enumerate().map(|(i, p)| ListItem::new(format!("{} {p}", if i == self.selected { ">" } else { " " }))).collect();
                if lines.is_empty() { lines.push(ListItem::new("No matches. Type a device path and press Enter.")); }
                self.overlay(frame, "Devices — type to filter or enter a path", &format!("{}\nF5 refresh · Enter connect · Esc back", self.filter), Some(lines));
            }
            Mode::Help => self.overlay(frame, "Help", "Keys go to the device unless prefixed by Ctrl-T.\n\nCtrl-T Space  command palette\nCtrl-T c      change baud rate\nCtrl-T d      device picker\nCtrl-T e      toggle local echo\nCtrl-T [      scrollback\nCtrl-T b      send break\nCtrl-T q      quit\nCtrl-T Ctrl-T send literal Ctrl-T\n\nEsc closes this window.", None),
            Mode::Palette => {
                let items = self.palette_items().iter().enumerate().map(|(i, (name, c))| ListItem::new(format!("{} {}  (Ctrl-T {})", if i == self.selected { ">" } else { " " }, name, c))).collect();
                self.overlay(frame, "Commands", &format!("Search: {}", self.filter), Some(items));
            }
            Mode::Baud => self.overlay(frame, "Baud rate", &format!("{}\n\nEnter applies · Esc cancels\n{}", self.baud_input, self.status), None),
            Mode::Paste => self.overlay(frame, "Confirm paste", &format!("Send {} bytes to the device?\nEnter/y sends · Esc/n cancels", self.paste.len()), None),
            _ => {}
        }
    }
    fn draw_screen(&self, frame: &mut Frame, area: Rect) {
        let screen = self.parser.screen();
        let buf = frame.buffer_mut();
        for y in 0..area.height {
            for x in 0..area.width {
                let Some(cell) = screen.cell(y, x) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue;
                }
                let mut style = Style::default()
                    .fg(color(cell.fgcolor()))
                    .bg(color(cell.bgcolor()));
                if cell.bold() {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if cell.italic() {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.inverse() {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                if let Some(target) = buf.cell_mut((area.x + x, area.y + y)) {
                    target
                        .set_symbol(if cell.has_contents() {
                            cell.contents()
                        } else {
                            " "
                        })
                        .set_style(style);
                }
            }
        }
        if self.mode == Mode::Terminal && !screen.hide_cursor() && screen.scrollback() == 0 {
            let (row, col) = screen.cursor_position();
            if row < area.height && col < area.width {
                frame.set_cursor_position((area.x + col, area.y + row));
            }
        }
    }
    fn overlay(
        &self,
        frame: &mut Frame,
        title: &str,
        text: &str,
        items: Option<Vec<ListItem<'_>>>,
    ) {
        let area = frame.area();
        let width = area.width.saturating_sub(2).min(70);
        let height = area
            .height
            .saturating_sub(2)
            .min(if items.is_some() { 16 } else { 18 });
        let rect = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, rect);
        let block = Block::default().title(title).borders(Borders::ALL);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        if let Some(items) = items {
            let chunks = Layout::vertical([
                Constraint::Length(if self.mode == Mode::Picker { 2 } else { 1 }),
                Constraint::Min(0),
            ])
            .split(inner);
            frame.render_widget(Paragraph::new(text), chunks[0]);
            frame.render_widget(List::new(items), chunks[1]);
        } else {
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), inner);
        }
    }
}

fn color(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(n) => Color::Indexed(n),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::mpsc;

    fn app() -> (App, mpsc::Receiver<Command>) {
        let settings = Settings {
            target: String::new(),
            baud: 115200,
            data_bits: serialport::DataBits::Eight,
            stop_bits: serialport::StopBits::One,
            parity: serialport::Parity::None,
            flow: serialport::FlowControl::None,
            reconnect: true,
        };
        let mut app = App::new(settings, true);
        let (tx, rx) = mpsc::sync_channel(16);
        let (_event_tx, event_rx) = mpsc::sync_channel(16);
        app.session = Some(Session {
            commands: tx,
            events: event_rx,
        });
        app.mode = Mode::Terminal;
        app.connected = Some(42);
        (app, rx)
    }
    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }
    fn write_bytes(rx: &mpsc::Receiver<Command>) -> Vec<u8> {
        match rx.try_recv().unwrap() {
            Command::Write {
                generation: 42,
                bytes,
            } => bytes,
            other => panic!("unexpected {other:?}"),
        }
    }
    #[test]
    fn prefix_commands_never_leak_into_serial_stream() {
        let (mut app, rx) = app();
        app.key(key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert_eq!(write_bytes(&rx), b"q");
        app.key(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(write_bytes(&rx), [3]);
        app.key(key(KeyCode::Char('t'), KeyModifiers::CONTROL));
        app.key(key(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(app.mode == Mode::Help);
        assert!(rx.try_recv().is_err());
        app.key(key(KeyCode::Esc, KeyModifiers::NONE));
        app.key(key(KeyCode::Char('t'), KeyModifiers::CONTROL));
        app.key(key(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(write_bytes(&rx), [20]);
    }
    #[test]
    fn paste_is_not_interpreted_as_shortcuts() {
        let (mut app, rx) = app();
        app.mode = Mode::Paste;
        app.paste = "\u{14}q".to_owned();
        app.key(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(write_bytes(&rx), [20, b'q']);
        assert!(!app.quit);
    }
    #[test]
    fn renders_tiny_terminals_without_panicking() {
        let (mut app, _rx) = app();
        for (w, h) in [(80, 24), (10, 4), (2, 2)] {
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            for mode in [
                Mode::Terminal,
                Mode::Help,
                Mode::Picker,
                Mode::Palette,
                Mode::Baud,
                Mode::Paste,
            ] {
                app.mode = mode;
                terminal.draw(|frame| app.draw(frame)).unwrap();
            }
        }
    }
}
