use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{parse, refresh_wake, State};

#[derive(Debug, PartialEq, Eq)]
enum WindowResult {
    Expired,
    FiltersChanged,
    Stopped,
}

#[cfg(unix)]
fn next_frame(
    stream: &mut std::os::unix::net::UnixStream,
    buffer: &mut parse::NdjsonBuffer,
    queued: &mut VecDeque<String>,
    deadline: Instant,
) -> Result<Option<String>, String> {
    use std::io::Read;

    if let Some(frame) = queued.pop_front() {
        return Ok(Some(frame));
    }
    let mut chunk = [0u8; 4096];
    loop {
        if Instant::now() >= deadline {
            return Ok(None);
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err(buffer.parse_eof().unwrap_err()),
            Ok(read) => {
                queued.extend(buffer.push(&chunk[..read])?);
                if let Some(frame) = queued.pop_front() {
                    return Ok(Some(frame));
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted
                        | std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(format!("Herdr event socket read failed: {error}")),
        }
    }
}

#[cfg(unix)]
fn subscribe_window(
    stream: &mut std::os::unix::net::UnixStream,
    state: &Arc<Mutex<State>>,
    wake: &refresh_wake::Wake,
    stop: &AtomicBool,
    generation: u64,
    request: &str,
    window: Duration,
) -> Result<WindowResult, String> {
    use std::io::Write;

    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(|error| format!("could not bound Herdr event reads: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(1)))
        .map_err(|error| format!("could not bound Herdr event writes: {error}"))?;
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("Herdr event subscription write failed: {error}"))?;

    let mut buffer = parse::NdjsonBuffer::default();
    let mut queued = VecDeque::new();
    let ack_deadline = Instant::now() + Duration::from_secs(3);
    let ack = next_frame(stream, &mut buffer, &mut queued, ack_deadline)?
        .ok_or_else(|| "Herdr event subscription acknowledgement timed out".to_string())?;
    parse::parse_subscription_ack(&ack)?;

    refresh_wake::request(wake);
    if let Ok(mut guard) = state.lock() {
        guard.watcher_err.clear();
    }
    let deadline = Instant::now() + window;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(WindowResult::Stopped);
        }
        let current_generation = state
            .lock()
            .map_err(|_| "Herdr event watcher state is unavailable".to_string())?
            .inventory_generation;
        if current_generation != generation {
            return Ok(WindowResult::FiltersChanged);
        }
        let read_deadline = (Instant::now() + Duration::from_millis(100)).min(deadline);
        let Some(frame) = next_frame(stream, &mut buffer, &mut queued, read_deadline)? else {
            if Instant::now() >= deadline {
                return Ok(WindowResult::Expired);
            }
            continue;
        };
        match parse::parse_stream_event(&frame)? {
            parse::StreamEvent::Relevant => refresh_wake::request(wake),
            parse::StreamEvent::Ignore => {}
            parse::StreamEvent::Reconcile(why) => {
                refresh_wake::request(wake);
                return Err(why);
            }
        }
    }
}

#[cfg(unix)]
fn current_inventory(state: &Arc<Mutex<State>>) -> Result<(u64, Vec<String>), String> {
    let guard = state
        .lock()
        .map_err(|_| "Herdr event watcher state is unavailable".to_string())?;
    Ok((guard.inventory_generation, guard.pane_ids.clone()))
}

#[cfg(unix)]
fn sleep_or_stop(stop: &AtomicBool, duration: Duration) {
    let until = Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) && Instant::now() < until {
        std::thread::sleep((until - Instant::now()).min(Duration::from_millis(100)));
    }
}

#[cfg(unix)]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

#[cfg(unix)]
enum ConnectProgress {
    Connected,
    Retry,
    Stopped,
}

#[cfg(unix)]
fn wait_for_connect(
    fd: std::os::fd::RawFd,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<ConnectProgress, String> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(ConnectProgress::Stopped);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("Herdr event socket connection timed out".into());
        }
        let timeout_ms = remaining.as_millis().clamp(1, 100) as libc::c_int;
        let mut descriptor = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if ready < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("Herdr event socket connection failed: {error}"));
        }
        if ready == 0 {
            continue;
        }
        if descriptor.revents & libc::POLLNVAL != 0 {
            return Err("Herdr event socket connection failed: invalid descriptor".into());
        }
        if descriptor.revents & (libc::POLLOUT | libc::POLLERR | libc::POLLHUP) == 0 {
            continue;
        }
        let mut socket_error: libc::c_int = 0;
        let mut option_len = std::mem::size_of_val(&socket_error) as libc::socklen_t;
        let checked = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut socket_error as *mut libc::c_int).cast(),
                &mut option_len,
            )
        };
        if checked < 0 {
            return Err(format!(
                "Herdr event socket connection failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        if socket_error == libc::EAGAIN {
            sleep_or_stop(stop, Duration::from_millis(10));
            return Ok(if stop.load(Ordering::Relaxed) {
                ConnectProgress::Stopped
            } else {
                ConnectProgress::Retry
            });
        }
        if matches!(
            socket_error,
            libc::EINPROGRESS | libc::EALREADY | libc::EINTR
        ) {
            sleep_or_stop(stop, Duration::from_millis(10));
            continue;
        }
        if socket_error != 0 {
            return Err(format!(
                "Herdr event socket connection failed: {}",
                std::io::Error::from_raw_os_error(socket_error)
            ));
        }

        let mut peer: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        let mut peer_len = std::mem::size_of_val(&peer) as libc::socklen_t;
        let checked = unsafe {
            libc::getpeername(
                fd,
                (&mut peer as *mut libc::sockaddr_un).cast(),
                &mut peer_len,
            )
        };
        if checked == 0 {
            return Ok(ConnectProgress::Connected);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOTCONN) {
            sleep_or_stop(stop, Duration::from_millis(10));
            return Ok(if stop.load(Ordering::Relaxed) {
                ConnectProgress::Stopped
            } else {
                ConnectProgress::Retry
            });
        }
        return Err(format!("Herdr event socket connection failed: {error}"));
    }
}

#[cfg(unix)]
fn connect_unix(
    path: &std::path::Path,
    stop: &AtomicBool,
) -> Result<Option<std::os::unix::net::UnixStream>, String> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixStream;

    if stop.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let path_bytes = path.as_os_str().as_bytes();
    if path_bytes.contains(&0) {
        return Err("Herdr event socket path contains a null byte".into());
    }
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if path_bytes.len() >= address.sun_path.len() {
        return Err("Herdr event socket path is too long".into());
    }
    address.sun_family = libc::AF_UNIX as _;
    for (slot, byte) in address.sun_path.iter_mut().zip(path_bytes) {
        *slot = *byte as libc::c_char;
    }
    let address_len = std::mem::offset_of!(libc::sockaddr_un, sun_path) + path_bytes.len() + 1;
    let address_len = libc::socklen_t::try_from(address_len)
        .map_err(|_| "Herdr event socket address is too long".to_string())?;
    #[cfg(target_os = "macos")]
    {
        address.sun_len = u8::try_from(address_len)
            .map_err(|_| "Herdr event socket address is too long".to_string())?;
    }

    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        if Instant::now() >= deadline {
            return Err("Herdr event socket connection timed out".into());
        }
        let raw_fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if raw_fd < 0 {
            return Err(format!(
                "Herdr event socket creation failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: `socket` returned a new descriptor and `OwnedFd` closes it
        // on every return path until ownership is transferred to `UnixStream`.
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let descriptor_flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        if descriptor_flags < 0
            || unsafe {
                libc::fcntl(
                    fd.as_raw_fd(),
                    libc::F_SETFD,
                    descriptor_flags | libc::FD_CLOEXEC,
                )
            } < 0
        {
            return Err(format!(
                "Herdr event socket setup failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        let status_flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if status_flags < 0
            || unsafe {
                libc::fcntl(
                    fd.as_raw_fd(),
                    libc::F_SETFL,
                    status_flags | libc::O_NONBLOCK,
                )
            } < 0
        {
            return Err(format!(
                "Herdr event socket setup failed: {}",
                std::io::Error::last_os_error()
            ));
        }
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }

        let connected = unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                address_len,
            )
        };
        if connected < 0 {
            let error = std::io::Error::last_os_error();
            match error.raw_os_error() {
                Some(libc::EAGAIN) => {
                    sleep_or_stop(stop, Duration::from_millis(10));
                    continue;
                }
                Some(libc::EINPROGRESS | libc::EALREADY | libc::EINTR) => {
                    match wait_for_connect(fd.as_raw_fd(), stop, deadline)? {
                        ConnectProgress::Connected => {}
                        ConnectProgress::Retry => continue,
                        ConnectProgress::Stopped => return Ok(None),
                    }
                }
                _ => return Err(format!("Herdr event socket connection failed: {error}")),
            }
        }

        let stream = UnixStream::from(fd);
        stream
            .set_nonblocking(false)
            .map_err(|error| format!("could not restore blocking Herdr event socket: {error}"))?;
        return Ok(Some(stream));
    }
}

#[cfg(unix)]
fn watch_unix(
    state: Arc<Mutex<State>>,
    wake: refresh_wake::Wake,
    refresh: f64,
    stop: Arc<AtomicBool>,
) {
    let window = Duration::from_secs_f64(refresh.clamp(1.0, 30.0));
    let mut backoff = Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        let attempt = (|| {
            let raw_status = super::herdr_text(&["status", "server", "--json"])?;
            let target = parse::parse_status(&raw_status)?;
            let (generation, pane_ids) = current_inventory(&state)?;
            let Some(mut stream) = connect_unix(&target.path, &stop)? else {
                return Ok(WindowResult::Stopped);
            };
            let request = parse::subscription_request(&pane_ids)?;
            subscribe_window(
                &mut stream,
                &state,
                &wake,
                &stop,
                generation,
                &request,
                window,
            )
        })();
        match attempt {
            Ok(WindowResult::Stopped) => return,
            Ok(WindowResult::FiltersChanged | WindowResult::Expired) => {
                backoff = Duration::from_secs(1);
            }
            Err(error) => {
                if let Ok(mut guard) = state.lock() {
                    guard.watcher_err = error;
                }
                refresh_wake::request(&wake);
                sleep_or_stop(&stop, backoff);
                backoff = (backoff * 2).min(Duration::from_secs(8));
            }
        }
    }
}

pub fn watch(
    state: Arc<Mutex<State>>,
    wake: refresh_wake::Wake,
    refresh: f64,
    stop: Arc<AtomicBool>,
) {
    #[cfg(unix)]
    watch_unix(state, wake, refresh, stop);
    #[cfg(not(unix))]
    {
        let _ = (wake, refresh, stop);
        if let Ok(mut guard) = state.lock() {
            guard.watcher_err = "Herdr event sockets are unavailable on this platform".into();
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn fragmented_ndjson_and_inventory_change_rearm_a_fake_socket() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("opscope-herdr-{}-{suffix}", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        let (event_tx, event_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert!(request.contains("pane.agent_status_changed"));
            stream
                .write_all(br#"{"id":"opscope-herdr-panes-events","result":{"type":"sub"#)
                .unwrap();
            std::thread::sleep(Duration::from_millis(20));
            stream.write_all(b"scription_started\"}}\n").unwrap();
            event_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            stream.write_all(b"{\"event\":\"pane.cre").unwrap();
            std::thread::sleep(Duration::from_millis(20));
            stream
                .write_all(b"ated\",\"data\":{\"pane_id\":\"p2\"}}\n")
                .unwrap();
            let mut byte = [0u8; 1];
            let _ = stream.read(&mut byte);
        });
        let state = Arc::new(Mutex::new(State::default()));
        let wake = refresh_wake::new();
        let stop = Arc::new(AtomicBool::new(false));
        let mut stream = std::os::unix::net::UnixStream::connect(&path).unwrap();
        let reader_state = Arc::clone(&state);
        let reader_wake = Arc::clone(&wake);
        let reader_stop = Arc::clone(&stop);
        let (result_tx, result_rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let result = subscribe_window(
                &mut stream,
                &reader_state,
                &reader_wake,
                &reader_stop,
                0,
                &parse::subscription_request(&["p1".into()]).unwrap(),
                Duration::from_secs(3),
            );
            let _ = result_tx.send(format!("{result:?}"));
            result
        });
        let ack_wake = refresh_wake::wait_for(&wake, Duration::from_secs(2));
        assert_eq!(
            ack_wake,
            Ok(true),
            "watcher result: {:?}",
            result_rx.try_recv()
        );
        event_tx.send(()).unwrap();
        assert_eq!(
            refresh_wake::wait_for(&wake, Duration::from_secs(2)),
            Ok(true)
        );
        state.lock().unwrap().inventory_generation = 1;
        assert_eq!(reader.join().unwrap(), Ok(WindowResult::FiltersChanged));
        server.join().unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn fake_socket_eof_after_ack_is_visible() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("opscope-herdr-eof-{}-{suffix}", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            stream
                .write_all(
                    b"{\"id\":\"opscope-herdr-panes-events\",\"result\":{\"type\":\"subscription_started\"}}\n",
                )
                .unwrap();
        });
        let state = Arc::new(Mutex::new(State::default()));
        let wake = refresh_wake::new();
        let stop = AtomicBool::new(false);
        let mut stream = std::os::unix::net::UnixStream::connect(&path).unwrap();
        let error = subscribe_window(
            &mut stream,
            &state,
            &wake,
            &stop,
            0,
            &parse::subscription_request(&[]).unwrap(),
            Duration::from_secs(3),
        )
        .unwrap_err();
        assert!(error.contains("socket closed"));
        assert_eq!(
            refresh_wake::wait_for(&wake, Duration::from_secs(1)),
            Ok(true)
        );
        server.join().unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn full_listener_backlog_makes_connect_fail_within_the_bound() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "opscope-herdr-full-{}-{suffix}",
            std::process::id()
        ));
        let listener = UnixListener::bind(&path).unwrap();
        // SAFETY: `listener` owns a valid listening Unix-domain descriptor.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
        let mut queued = Vec::new();
        let mut timed_out = false;
        for _ in 0..256 {
            let started = Instant::now();
            match connect_unix(&path, &AtomicBool::new(false)) {
                Ok(Some(stream)) => queued.push(stream),
                Ok(None) => panic!("an unstopped connection attempt stopped"),
                Err(error) => {
                    assert!(started.elapsed() <= CONNECT_TIMEOUT + Duration::from_secs(1));
                    assert!(error.contains("timed out"), "{error}");
                    timed_out = true;
                    break;
                }
            }
        }
        assert!(timed_out, "the listener never filled its backlog");

        drop(queued);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn stopped_and_missing_socket_connections_return_without_blocking() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "opscope-herdr-missing-{}-{suffix}",
            std::process::id()
        ));

        assert!(connect_unix(&path, &AtomicBool::new(true))
            .unwrap()
            .is_none());
        let error = connect_unix(&path, &AtomicBool::new(false)).unwrap_err();
        assert!(error.contains("connection failed"), "{error}");
    }
}
