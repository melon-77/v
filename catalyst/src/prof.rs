//! `CATALYST_PROF=1`: the time spent between marks, on stderr, for
//! measuring where a run goes. Without the variable a mark costs one check.

use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static CLOCK: OnceLock<Option<Mutex<(Instant, Instant)>>> = OnceLock::new();

/// Prints the time since the previous mark (and since the first), then `label`.
pub fn mark(label: &str) {
    let clock = CLOCK.get_or_init(|| {
        std::env::var_os("CATALYST_PROF").map(|_| {
            let now = Instant::now();
            Mutex::new((now, now))
        })
    });
    if let Some(m) = clock {
        let mut g = m.lock().unwrap();
        let now = Instant::now();
        eprintln!(
            "prof: {:8.1} ms {:8.1} ms  {label}",
            (now - g.1).as_secs_f64() * 1000.0,
            (now - g.0).as_secs_f64() * 1000.0
        );
        g.1 = now;
    }
}
