//! Byte budget limiting the amount of data in flight between pipeline stages.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

pub struct Budget {
    total: usize,
    avail: Mutex<usize>,
    cv: Condvar,
}

impl Budget {
    pub fn new(total: usize) -> Self {
        let total = total.max(1);
        Self { total, avail: Mutex::new(total), cv: Condvar::new() }
    }

    /// Blocks until `n` bytes (capped at the total) are available and takes them.
    /// Returns the amount taken, or `None` if `cancel` was raised while waiting.
    pub fn acquire(&self, n: usize, cancel: &AtomicBool) -> Option<usize> {
        let n = n.min(self.total);
        let mut avail = self.avail.lock().unwrap();
        while *avail < n {
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            avail = self.cv.wait_timeout(avail, Duration::from_millis(50)).unwrap().0;
        }
        *avail -= n;
        Some(n)
    }

    pub fn release(&self, n: usize) {
        if n == 0 {
            return;
        }
        let mut avail = self.avail.lock().unwrap();
        *avail = (*avail + n).min(self.total);
        self.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn caps_requests_and_blocks_until_release() {
        let b = Arc::new(Budget::new(100));
        let cancel = AtomicBool::new(false);
        assert_eq!(b.acquire(500, &cancel), Some(100));
        let b2 = b.clone();
        let t = std::thread::spawn(move || b2.acquire(10, &AtomicBool::new(false)));
        std::thread::sleep(Duration::from_millis(20));
        b.release(100);
        assert_eq!(t.join().unwrap(), Some(10));
    }

    #[test]
    fn cancel_unblocks() {
        let b = Budget::new(1);
        let cancel = AtomicBool::new(false);
        b.acquire(1, &cancel);
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(b.acquire(1, &cancel), None);
    }
}
