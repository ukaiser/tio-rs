#![cfg(unix)]

use std::{
    ffi::CStr,
    fs::File,
    io::{Read, Write},
    os::fd::{FromRawFd, RawFd},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use tio_rs::session::{Command as SessionCommand, Event, Session, Settings};

fn pty() -> (File, String) {
    let mut master = -1;
    let mut slave = -1;
    // openpty initializes both file descriptors and the slave name.
    let mut name = [0i8; 256];
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            name.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(result, 0, "openpty: {}", std::io::Error::last_os_error());
    let path = unsafe { CStr::from_ptr(name.as_ptr()) }
        .to_str()
        .unwrap()
        .to_owned();
    unsafe { libc::close(slave) };
    (unsafe { File::from_raw_fd(master) }, path)
}

fn settings(target: String) -> Settings {
    Settings {
        target,
        baud: 115200,
        data_bits: serialport::DataBits::Eight,
        stop_bits: serialport::StopBits::One,
        parity: serialport::Parity::None,
        flow: serialport::FlowControl::None,
        reconnect: false,
    }
}

fn readable(fd: RawFd) -> bool {
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if unsafe { libc::poll(&mut pollfd, 1, 50) > 0 && pollfd.revents & libc::POLLIN != 0 } {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn session_round_trips_binary_and_counts_sent_bytes() {
    use std::os::fd::AsRawFd;
    let (mut master, path) = pty();
    let session = Session::spawn(settings(path));
    let generation = match session.events.recv_timeout(Duration::from_secs(3)).unwrap() {
        Event::Connected { generation, .. } => generation,
        other => panic!("unexpected event: {other:?}"),
    };
    let tx = vec![0, 1, 3, 13, 10, 127, 128, 255];
    session
        .commands
        .send(SessionCommand::Write {
            generation,
            bytes: tx.clone(),
        })
        .unwrap();
    assert!(readable(master.as_raw_fd()));
    let mut received = vec![0; tx.len()];
    master.read_exact(&mut received).unwrap();
    assert_eq!(received, tx);
    assert!(matches!(
        session.events.recv_timeout(Duration::from_secs(3)).unwrap(),
        Event::Sent(8)
    ));
    master.write_all(&tx).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut bytes = Vec::new();
    while bytes.len() < tx.len() && Instant::now() < deadline {
        if let Ok(Event::Received(chunk)) = session.events.recv_timeout(Duration::from_millis(100))
        {
            bytes.extend(chunk);
        }
    }
    assert_eq!(bytes, tx);
    session.commands.send(SessionCommand::Shutdown).unwrap();
}

#[test]
fn no_reconnect_reports_open_failure() {
    let session = Session::spawn(settings("/nonexistent/tio-rs-test-port".into()));
    assert!(matches!(
        session.events.recv_timeout(Duration::from_secs(3)).unwrap(),
        Event::Finished(_)
    ));
}

#[test]
fn pipeline_sends_exact_bytes_and_exits_at_eof() {
    use std::os::fd::AsRawFd;
    let (mut master, path) = pty();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tio-rs"))
        .args(["--ui", "none", "--no-reconnect", &path])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let data = vec![0, 1, 2, 3, 10, 13, 127, 128, 255];
    child.stdin.take().unwrap().write_all(&data).unwrap();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    assert!(readable(master.as_raw_fd()));
    let mut result = vec![0; data.len()];
    master.read_exact(&mut result).unwrap();
    assert_eq!(result, data);
    let output = rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn reconnects_to_replaced_target_without_replaying_old_generation() {
    use std::{
        os::{fd::AsRawFd, unix::fs::symlink},
        sync::atomic::{AtomicUsize, Ordering},
    };
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let directory = std::env::temp_dir().join(format!(
        "tio-rs-pty-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&directory).unwrap();
    let target = directory.join("port");
    let (old_master, old_path) = pty();
    symlink(old_path, &target).unwrap();
    let mut options = settings(target.to_string_lossy().into_owned());
    options.reconnect = true;
    let session = Session::spawn(options);
    let old = match session.events.recv_timeout(Duration::from_secs(3)).unwrap() {
        Event::Connected { generation, .. } => generation,
        other => panic!("unexpected {other:?}"),
    };
    let (mut new_master, new_path) = pty();
    let replacement = directory.join("replacement");
    symlink(new_path, &replacement).unwrap();
    std::fs::rename(replacement, &target).unwrap();
    drop(old_master);
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut new_generation = None;
    while Instant::now() < deadline {
        if let Ok(Event::Connected { generation, .. }) =
            session.events.recv_timeout(Duration::from_millis(100))
        {
            new_generation = Some(generation);
            break;
        }
    }
    let new = new_generation.expect("did not reconnect");
    assert_ne!(old, new);
    session
        .commands
        .send(SessionCommand::Write {
            generation: old,
            bytes: b"stale".to_vec(),
        })
        .unwrap();
    session
        .commands
        .send(SessionCommand::Write {
            generation: new,
            bytes: b"fresh".to_vec(),
        })
        .unwrap();
    assert!(readable(new_master.as_raw_fd()));
    let mut bytes = [0; 5];
    new_master.read_exact(&mut bytes).unwrap();
    assert_eq!(&bytes, b"fresh");
    session.commands.send(SessionCommand::Shutdown).unwrap();
    std::fs::remove_dir_all(directory).unwrap();
}
