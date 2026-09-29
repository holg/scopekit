//! Waking the event loop from other threads.

use std::fmt;
use std::sync::Arc;

/// Asks the running app for a redraw from any thread: an emulator's serial
/// reader, a build, a network client. Cheap to clone; wake-ups that arrive
/// faster than frames are drawn coalesce.
///
/// ```
/// use std::sync::atomic::{AtomicUsize, Ordering};
/// use std::sync::Arc;
///
/// let count = Arc::new(AtomicUsize::new(0));
/// let seen = count.clone();
/// let waker = scopekit::Waker::new(move || { seen.fetch_add(1, Ordering::Relaxed); });
/// let w = waker.clone();
/// std::thread::spawn(move || w.wake()).join().unwrap();
/// assert_eq!(count.load(Ordering::Relaxed), 1);
/// ```
#[derive(Clone)]
pub struct Waker {
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Waker {
    /// A waker that calls `f`. scopekit makes the real ones; this is for
    /// custom drivers and tests.
    pub fn new(f: impl Fn() + Send + Sync + 'static) -> Waker {
        Waker { wake: Arc::new(f) }
    }

    /// A waker that does nothing.
    pub fn noop() -> Waker {
        Waker::new(|| {})
    }

    /// Request a redraw as soon as possible.
    pub fn wake(&self) {
        (self.wake)();
    }
}

impl fmt::Debug for Waker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Waker")
    }
}
