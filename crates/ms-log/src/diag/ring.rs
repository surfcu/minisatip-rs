//! In-memory flight recorder: the last `N` log messages.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

/// Largest capacity accepted from `--log-ring`.
const MAX_CAPACITY: usize = 1_000_000;

struct State {
    capacity: usize,
    lines: VecDeque<String>,
}

static RING: Mutex<Option<State>> = Mutex::new(None);

fn lock() -> std::sync::MutexGuard<'static, Option<State>> {
    RING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts the recorder. `capacity == 0` disables it.
pub fn start(capacity: usize) {
    let capacity = capacity.min(MAX_CAPACITY);
    *lock() = (capacity > 0).then(|| State {
        capacity,
        lines: VecDeque::with_capacity(capacity.min(4096)),
    });
}

/// Disables the recorder and drops its contents.
pub fn stop() {
    *lock() = None;
}

/// Whether the recorder is collecting lines.
pub fn is_active() -> bool {
    lock().is_some()
}

/// Number of lines the recorder can hold.
pub fn capacity() -> usize {
    lock().as_ref().map_or(0, |state| state.capacity)
}

/// Appends one message, overwriting the oldest line when full.
pub fn push(message: &str) {
    let mut guard = lock();
    let Some(state) = guard.as_mut() else {
        return;
    };
    if state.lines.len() == state.capacity {
        state.lines.pop_front();
    }
    state.lines.push_back(message.to_owned());
}

/// The newest `lines` messages, oldest first.
///
/// Uses `try_lock`: the panic hook must not deadlock when the panicking thread
/// holds the ring lock, and a missed dump is better than a hang.
pub fn dump(lines: usize) -> Vec<String> {
    let guard = match RING.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return Vec::new(),
    };
    let Some(state) = guard.as_ref() else {
        return Vec::new();
    };
    let keep = lines.min(state.lines.len());
    state
        .lines
        .iter()
        .skip(state.lines.len() - keep)
        .cloned()
        .collect()
}
