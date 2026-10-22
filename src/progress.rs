//! `--progress-json`: one JSON object per line (NDJSON) on stderr for a progress bar in a GUI.
//!
//! Events (schema version 1, see README.md): `phase`, `iter` and a final `done`. Every event
//! carries `t`, the seconds since the start. Without `enable` nothing is written.

use serde_json::{json, Value};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// schema version, reported in the `done` event
pub const VERSION: u32 = 1;

static ON: AtomicBool = AtomicBool::new(false);
static START: OnceLock<Instant> = OnceLock::new();

/// switch the event stream on; `t` counts from `t0`
pub fn enable(t0: Instant) {
    START.get_or_init(|| t0);
    ON.store(true, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// seconds since the start, rounded to 0.01
fn t() -> f64 {
    let s = START.get().map_or(0.0, |t0| t0.elapsed().as_secs_f64());
    (s * 100.0).round() / 100.0
}

/// write one event as a single line and flush it (no fragments over a pipe)
pub fn emit(mut ev: Value) {
    if !enabled() {
        return;
    }
    ev["t"] = json!(t());
    let line = format!("{ev}\n");
    let mut e = std::io::stderr().lock();
    let _ = e.write_all(line.as_bytes());
    let _ = e.flush();
}

/// `{"ev":"phase","phase":NAME,"t":..}`
pub fn phase(name: &str) {
    emit(json!({"ev": "phase", "phase": name}));
}

/// rough time to the end of the negotiation: the mean iteration time times the iterations
/// still allowed, capped by the time left to the deadline (--time-limit); `None` without data
pub fn eta(
    mean_iter_s: f64,
    iters_left: usize,
    deadline: Option<Instant>,
    now: Instant,
) -> Option<f64> {
    if !mean_iter_s.is_finite() {
        return None;
    }
    let mut e = mean_iter_s * iters_left as f64;
    if let Some(d) = deadline {
        e = e.min(d.saturating_duration_since(now).as_secs_f64());
    }
    Some((e * 10.0).round() / 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn eta_is_capped_by_the_deadline() {
        let now = Instant::now();
        assert_eq!(eta(2.0, 10, None, now), Some(20.0));
        assert_eq!(
            eta(2.0, 10, Some(now + Duration::from_secs(5)), now),
            Some(5.0)
        );
        assert_eq!(
            eta(2.0, 10, Some(now), now + Duration::from_secs(1)),
            Some(0.0)
        );
        assert_eq!(eta(f64::NAN, 3, None, now), None);
    }
}
