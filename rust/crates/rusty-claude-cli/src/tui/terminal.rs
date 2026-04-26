use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

/// Tracks terminal dimensions with periodic re-query.
///
/// This avoids calling crossterm::terminal::size() on every render
/// while still catching SIGWINCH resizes within the polling interval.
pub struct TerminalSize {
    cached_width: AtomicU16,
    cached_height: AtomicU16,
    last_checked: std::sync::Mutex<Instant>,
    poll_interval: Duration,
}

impl TerminalSize {
    /// Create a new TerminalSize with the default polling interval (1s).
    pub fn new() -> Self {
        let (w, h) = Self::query_size().unwrap_or((80, 24));
        Self {
            cached_width: AtomicU16::new(w),
            cached_height: AtomicU16::new(h),
            last_checked: std::sync::Mutex::new(Instant::now()),
            poll_interval: Duration::from_secs(1),
        }
    }

    /// Get the current terminal width, re-querying if enough time has passed.
    pub fn width(&self) -> u16 {
        self.maybe_refresh();
        self.cached_width.load(Ordering::Relaxed)
    }

    /// Get the current terminal height, re-querying if enough time has passed.
    #[allow(dead_code)]
    pub fn height(&self) -> u16 {
        self.maybe_refresh();
        self.cached_height.load(Ordering::Relaxed)
    }

    /// Force a refresh on the next call.
    pub fn invalidate(&self) {
        if let Ok(mut last) = self.last_checked.lock() {
            *last = Instant::now()
                .checked_sub(Duration::from_secs(3600))
                .unwrap_or(Instant::now());
        }
    }

    fn maybe_refresh(&self) {
        let should_refresh = self
            .last_checked
            .lock()
            .map(|last| last.elapsed() >= self.poll_interval)
            .unwrap_or(false);
        if should_refresh {
            if let Ok((w, h)) = Self::query_size() {
                self.cached_width.store(w, Ordering::Relaxed);
                self.cached_height.store(h, Ordering::Relaxed);
            }
            if let Ok(mut last) = self.last_checked.lock() {
                *last = Instant::now();
            }
        }
    }

    fn query_size() -> std::io::Result<(u16, u16)> {
        crossterm::terminal::size()
    }
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_returns_reasonable_value() {
        let ts = TerminalSize::new();
        // In test environment, crossterm might return 80
        let w = ts.width();
        assert!(w > 0, "terminal width should be positive");
    }

    #[test]
    fn invalidate_forces_refresh() {
        let ts = TerminalSize::new();
        let w1 = ts.width();
        ts.invalidate();
        let w2 = ts.width();
        assert!(w1 > 0);
        assert!(w2 > 0);
    }
}
