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
fn watch_unix(
    state: Arc<Mutex<State>>,
    wake: refresh_wake::Wake,
    refresh: f64,
    stop: Arc<AtomicBool>,
) {
    use std::os::unix::net::UnixStream;

    let window = Duration::from_secs_f64(refresh.clamp(1.0, 30.0));
    let mut backoff = Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        let attempt = (|| {
            let raw_status = super::herdr_text(&["status", "server", "--json"])?;
            let target = parse::parse_status(&raw_status)?;
            let (generation, pane_ids) = current_inventory(&state)?;
            let mut stream = UnixStream::connect(&target.path)
                .map_err(|error| format!("Herdr event socket connection failed: {error}"))?;
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
}
