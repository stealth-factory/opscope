use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

pub type Wake = Arc<(Mutex<bool>, Condvar)>;

pub fn new() -> Wake {
    Arc::new((Mutex::new(false), Condvar::new()))
}

pub fn request(wake: &Wake) {
    let (lock, cond) = &**wake;
    if let Ok(mut pending) = lock.lock() {
        *pending = true;
        cond.notify_all();
    }
}

pub fn wait_for(wake: &Wake, ceiling: Duration) -> Result<bool, ()> {
    let (lock, cond) = &**wake;
    let pending = lock.lock().map_err(|_| ())?;
    let mut pending = if *pending {
        pending
    } else {
        cond.wait_timeout(pending, ceiling).map_err(|_| ())?.0
    };
    let requested = *pending;
    *pending = false;
    Ok(requested)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn a_manual_wake_interrupts_the_idle_wait() {
        let wake = new();
        let waiter = Arc::clone(&wake);
        let thread = thread::spawn(move || wait_for(&waiter, Duration::from_secs(30)));
        request(&wake);
        assert_eq!(thread.join().unwrap(), Ok(true));
    }

    #[test]
    fn an_idle_wait_expires_at_its_ceiling() {
        let wake = new();
        assert_eq!(wait_for(&wake, Duration::from_millis(1)), Ok(false));
    }

    #[test]
    fn bursts_coalesce_into_one_pending_wake() {
        let wake = new();
        request(&wake);
        request(&wake);
        request(&wake);
        assert_eq!(wait_for(&wake, Duration::from_secs(1)), Ok(true));
        assert_eq!(wait_for(&wake, Duration::from_millis(1)), Ok(false));
    }
}
