use std::{
    io::{self, Read, Write},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct Settings {
    pub target: String,
    pub baud: u32,
    pub data_bits: serialport::DataBits,
    pub stop_bits: serialport::StopBits,
    pub parity: serialport::Parity,
    pub flow: serialport::FlowControl,
    pub reconnect: bool,
}

#[derive(Debug)]
pub enum Command {
    Write { generation: u64, bytes: Vec<u8> },
    SetBaud(u32),
    Drain,
    Break,
    Shutdown,
}

#[derive(Debug)]
pub enum Event {
    Connected { generation: u64, baud: u32 },
    Waiting(String),
    Received(Vec<u8>),
    Sent(usize),
    Drained,
    BaudChanged(u32),
    Notice(String),
    Finished(String),
}

pub struct Session {
    pub commands: SyncSender<Command>,
    pub events: Receiver<Event>,
}

impl Session {
    pub fn spawn(settings: Settings) -> Self {
        let (commands, command_rx) = mpsc::sync_channel(64);
        let (event_tx, events) = mpsc::sync_channel(128);
        thread::spawn(move || run(settings, command_rx, event_tx));
        Self { commands, events }
    }
}

fn run(mut settings: Settings, commands: Receiver<Command>, events: SyncSender<Event>) {
    let mut generation = 0u64;
    let mut previous_error = String::new();
    loop {
        match commands.try_recv() {
            Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => return,
            Ok(_) | Err(TryRecvError::Empty) => {}
        }
        let opened = serialport::new(&settings.target, settings.baud)
            .data_bits(settings.data_bits)
            .stop_bits(settings.stop_bits)
            .parity(settings.parity)
            .flow_control(settings.flow)
            .timeout(Duration::from_millis(20))
            .open();
        let mut port = match opened {
            Ok(port) => port,
            Err(error) => {
                let message = format!("{}: {error}", settings.target);
                if !settings.reconnect {
                    let _ = events.send(Event::Finished(message));
                    return;
                }
                if message != previous_error {
                    if events.send(Event::Waiting(message.clone())).is_err() {
                        return;
                    }
                    previous_error = message;
                }
                // Keep the shutdown path responsive while waiting for a device.
                let deadline = Instant::now() + Duration::from_secs(1);
                while Instant::now() < deadline {
                    match commands.recv_timeout(Duration::from_millis(50)) {
                        Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        _ => {}
                    }
                }
                continue;
            }
        };
        previous_error.clear();
        generation = generation.wrapping_add(1);
        if events
            .send(Event::Connected {
                generation,
                baud: settings.baud,
            })
            .is_err()
        {
            return;
        }
        let mut buf = [0u8; 4096];
        let reason = loop {
            // Limit command batches so incoming device data is not starved.
            for _ in 0..16 {
                match commands.try_recv() {
                    Ok(Command::Shutdown) | Err(TryRecvError::Disconnected) => return,
                    Err(TryRecvError::Empty) => break,
                    Ok(Command::Write {
                        generation: g,
                        bytes,
                    }) if g == generation => {
                        let mut written = 0;
                        while written < bytes.len() {
                            match port.write(&bytes[written..]) {
                                Ok(0) => break,
                                Ok(n) => written += n,
                                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                                Err(e) => {
                                    let _ =
                                        events.send(Event::Notice(format!("Write failed: {e}")));
                                    break;
                                }
                            }
                        }
                        if written > 0 && events.send(Event::Sent(written)).is_err() {
                            return;
                        }
                        if written != bytes.len()
                            && events
                                .send(Event::Notice(
                                    "Write incomplete; unsent bytes discarded".into(),
                                ))
                                .is_err()
                        {
                            return;
                        }
                    }
                    Ok(Command::Write { .. }) => {
                        let _ = events.send(Event::Notice(
                            "Stale input discarded after reconnect".into(),
                        ));
                    }
                    Ok(Command::SetBaud(baud)) => match port.set_baud_rate(baud) {
                        Ok(()) => {
                            settings.baud = baud;
                            if events.send(Event::BaudChanged(baud)).is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            if events
                                .send(Event::Notice(format!("Could not set baud rate: {e}")))
                                .is_err()
                            {
                                return;
                            }
                        }
                    },
                    Ok(Command::Drain) => match port.flush() {
                        Ok(()) => {
                            if events.send(Event::Drained).is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            if events
                                .send(Event::Notice(format!("Drain failed: {e}")))
                                .is_err()
                            {
                                return;
                            }
                        }
                    },
                    Ok(Command::Break) => {
                        let result = port.set_break().and_then(|()| {
                            thread::sleep(Duration::from_millis(250));
                            port.clear_break()
                        });
                        if let Err(e) = result {
                            let _ = events.send(Event::Notice(format!("Break failed: {e}")));
                        }
                    }
                }
            }
            match port.read(&mut buf) {
                Ok(0) => break "Device closed".to_owned(),
                Ok(n) => {
                    if events.send(Event::Received(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => break format!("Device disconnected: {e}"),
            }
        };
        drop(port);
        if !settings.reconnect {
            let _ = events.send(Event::Finished(reason));
            return;
        }
        if events.send(Event::Waiting(reason)).is_err() {
            return;
        }
    }
}
