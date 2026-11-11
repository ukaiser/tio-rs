use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use serialport::{DataBits, FlowControl, Parity, StopBits};
use std::{
    io::{self, IsTerminal, Read, Write},
    sync::mpsc,
    thread,
    time::Duration,
};
use tio_rs::session::{Command, Event, Session, Settings};

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
enum Ui {
    Auto,
    Tui,
    Plain,
    None,
}

#[derive(Parser, Debug)]
#[command(
    version,
    about = "A keyboard-friendly serial terminal",
    long_about = "Connect to a serial device using a Ratatui interface or byte-oriented plain mode."
)]
struct Args {
    /// Device path, e.g. /dev/ttyUSB0
    target: Option<String>,
    /// Baud rate
    #[arg(short = 'b', long = "baudrate", default_value_t = 115200)]
    baud: u32,
    /// Data bits
    #[arg(short = 'd', long, default_value_t = 8, value_parser = clap::value_parser!(u8).range(5..=8))]
    databits: u8,
    /// Stop bits
    #[arg(short = 's', long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=2))]
    stopbits: u8,
    /// Parity
    #[arg(short = 'p', long, default_value = "none", value_parser = ["none", "odd", "even"])]
    parity: String,
    /// Flow control
    #[arg(short = 'f', long, default_value = "none", value_parser = ["none", "hard", "soft"])]
    flow: String,
    /// Exit if the port cannot be opened or disconnects
    #[arg(short = 'n', long)]
    no_reconnect: bool,
    /// Select interface (auto uses TUI when stdin and stdout are terminals)
    #[arg(long, value_enum, default_value_t = Ui::Auto)]
    ui: Ui,
    /// List available serial ports
    #[arg(short, long)]
    list: bool,
    /// Keep receiving after stdin EOF in plain/headless mode
    #[arg(long)]
    stay: bool,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("tio-rs: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    if args.list {
        for port in serialport::available_ports().context("listing serial ports")? {
            println!("{}", port.port_name);
        }
        return Ok(());
    }
    anyhow::ensure!(args.baud > 0, "baud rate must be positive");
    let tty = io::stdin().is_terminal() && io::stdout().is_terminal();
    let ui = match args.ui {
        Ui::Auto if tty => Ui::Tui,
        Ui::Auto => Ui::None,
        other => other,
    };
    if ui == Ui::Tui {
        anyhow::ensure!(
            tty,
            "TUI requires terminal stdin and stdout; use --ui none for pipes"
        );
    }
    if ui != Ui::Tui {
        anyhow::ensure!(
            args.target.is_some(),
            "a device path is required outside the TUI"
        );
    }
    let picker = args.target.is_none();
    let settings = Settings {
        target: args.target.unwrap_or_default(),
        baud: args.baud,
        data_bits: match args.databits {
            5 => DataBits::Five,
            6 => DataBits::Six,
            7 => DataBits::Seven,
            _ => DataBits::Eight,
        },
        stop_bits: if args.stopbits == 2 {
            StopBits::Two
        } else {
            StopBits::One
        },
        parity: match args.parity.as_str() {
            "odd" => Parity::Odd,
            "even" => Parity::Even,
            _ => Parity::None,
        },
        flow: match args.flow.as_str() {
            "hard" => FlowControl::Hardware,
            "soft" => FlowControl::Software,
            _ => FlowControl::None,
        },
        reconnect: !args.no_reconnect,
    };
    if ui == Ui::Tui {
        tio_rs::ui::run(settings, picker)
    } else {
        plain(settings, ui == Ui::Plain && tty, args.stay)
    }
}

enum Input {
    Data(Vec<u8>),
    Eof,
    Error(io::Error),
}
struct RawGuard(bool);
impl Drop for RawGuard {
    fn drop(&mut self) {
        if self.0 {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

fn plain(settings: Settings, interactive: bool, stay: bool) -> Result<()> {
    if interactive {
        crossterm::terminal::enable_raw_mode()?;
        eprintln!("Connected mode: Ctrl-T q quits, Ctrl-T Ctrl-T sends Ctrl-T");
    }
    let _guard = RawGuard(interactive);
    let session = Session::spawn(settings);
    let (tx, rx) = mpsc::sync_channel::<Input>(16);
    let mut started = false;
    let mut connected = None;
    let mut pending = 0usize;
    let mut queued: Option<(u64, Vec<u8>)> = None;
    let mut eof = false;
    let mut draining = false;
    let mut prefix = false;
    loop {
        while let Ok(event) = session.events.try_recv() {
            match event {
                Event::Connected { generation, .. } => {
                    connected = Some(generation);
                    if !started {
                        started = true;
                        let tx = tx.clone();
                        thread::spawn(move || {
                            let mut stdin = io::stdin().lock();
                            let mut buf = [0u8; 4096];
                            loop {
                                match stdin.read(&mut buf) {
                                    Ok(0) => {
                                        let _ = tx.send(Input::Eof);
                                        break;
                                    }
                                    Ok(n) => {
                                        if tx.send(Input::Data(buf[..n].to_vec())).is_err() {
                                            break;
                                        }
                                    }
                                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                                    Err(e) => {
                                        let _ = tx.send(Input::Error(e));
                                        break;
                                    }
                                }
                            }
                        });
                    }
                }
                Event::Waiting(reason) => {
                    connected = None;
                    if pending > 0 || queued.is_some() {
                        anyhow::bail!("{reason}; pending input was not confirmed");
                    }
                    eprintln!("tio-rs: waiting: {reason}");
                }
                Event::Received(bytes) => {
                    let mut out = io::stdout().lock();
                    out.write_all(&bytes)?;
                    out.flush()?;
                }
                Event::Sent(n) => pending = pending.saturating_sub(n),
                Event::Drained => {
                    let _ = session.commands.try_send(Command::Shutdown);
                    return Ok(());
                }
                Event::Notice(message) => anyhow::bail!("{message}"),
                Event::Finished(reason) => anyhow::bail!("{reason}"),
                Event::BaudChanged(_) => {}
            }
        }
        if eof && pending == 0 && queued.is_none() && !stay && !draining && connected.is_some() {
            match session.commands.try_send(Command::Drain) {
                Ok(()) => draining = true,
                Err(mpsc::TrySendError::Full(_)) => {}
                Err(mpsc::TrySendError::Disconnected(_)) => anyhow::bail!("serial worker stopped"),
            }
        }
        if connected.is_none() {
            thread::sleep(Duration::from_millis(10));
            continue;
        }
        if let Some((generation, bytes)) = queued.take() {
            let len = bytes.len();
            match session
                .commands
                .try_send(Command::Write { generation, bytes })
            {
                Ok(()) => pending += len,
                Err(mpsc::TrySendError::Full(Command::Write { generation, bytes })) => {
                    queued = Some((generation, bytes));
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(_) => anyhow::bail!("serial worker stopped"),
            }
        }
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(Input::Data(mut bytes)) => {
                if interactive {
                    let mut filtered = Vec::with_capacity(bytes.len());
                    for b in bytes {
                        if prefix {
                            prefix = false;
                            if b == b'q' {
                                let _ = session.commands.try_send(Command::Shutdown);
                                return Ok(());
                            }
                            if b == 20 {
                                filtered.push(20);
                            }
                        } else if b == 20 {
                            prefix = true;
                        } else {
                            filtered.push(b);
                        }
                    }
                    bytes = filtered;
                }
                if !bytes.is_empty() {
                    queued = Some((connected.expect("checked"), bytes));
                }
            }
            Ok(Input::Eof) => eof = true,
            Ok(Input::Error(e)) => return Err(e.into()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => eof = true,
        }
    }
}
