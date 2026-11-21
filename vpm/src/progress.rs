//! Progress of the parallel fetch/unpack phase.
//!
//! On a terminal (and without `--plain`) a full-screen view is drawn on
//! the alternate screen: one line per package being fetched or unpacked,
//! and an overall bar. Otherwise plain lines are printed as packages
//! finish.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::sys;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Waiting,
    Fetching,
    Unpacking,
    Done,
    Failed,
}

struct Item {
    label: String,
    phase: Phase,
    cur: u64,
    total: u64,
    note: String,
}

struct State {
    title: String,
    items: Vec<Item>,
    /// bytes of work done / expected (download + unpacked content)
    work: u64,
    work_total: u64,
    done: usize,
    started: Instant,
}

pub struct Progress {
    st: Arc<Mutex<State>>,
    fancy: bool,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn bar(cur: u64, total: u64, width: usize) -> String {
    let frac = if total == 0 { 1.0 } else { (cur as f64 / total as f64).min(1.0) };
    let full = (frac * width as f64).round() as usize;
    format!("[{}{}]", "#".repeat(full), "-".repeat(width - full))
}

fn trunc(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else {
        s.chars().take(w.saturating_sub(1)).collect::<String>() + "~"
    }
}

impl Progress {
    /// `items`: (label, expected work in bytes). `fancy`: full screen.
    pub fn new(title: &str, items: Vec<(String, u64)>, fancy: bool) -> Progress {
        let work_total = items.iter().map(|i| i.1).sum();
        let st = State {
            title: title.to_string(),
            items: items
                .into_iter()
                .map(|(label, total)| Item { label, phase: Phase::Waiting, cur: 0, total, note: String::new() })
                .collect(),
            work: 0,
            work_total,
            done: 0,
            started: Instant::now(),
        };
        let st = Arc::new(Mutex::new(st));
        let stop = Arc::new(AtomicBool::new(false));
        let mut p = Progress { st: st.clone(), fancy, stop: stop.clone(), thread: None };
        if fancy {
            print!("\x1b[?1049h\x1b[?25l");
            let _ = std::io::stdout().flush();
            p.thread = Some(std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    draw(&st);
                    std::thread::sleep(Duration::from_millis(100));
                }
            }));
        }
        p
    }

    pub fn set_phase(&self, i: usize, phase: Phase, total: Option<u64>) {
        let mut st = self.st.lock().unwrap();
        let it = &mut st.items[i];
        it.phase = phase;
        if let Some(t) = total {
            it.total = t;
        }
        if phase == Phase::Fetching || phase == Phase::Unpacking {
            it.cur = 0;
        }
        if !self.fancy && phase == Phase::Fetching {
            println!("fetching {}", st.items[i].label);
        }
    }

    pub fn advance(&self, i: usize, n: u64) {
        let mut st = self.st.lock().unwrap();
        st.items[i].cur += n;
        st.work += n;
    }

    /// Marks an item done (with a note) and prints the plain line.
    pub fn finish_item(&self, i: usize, ok: bool, note: String) {
        let mut st = self.st.lock().unwrap();
        st.items[i].phase = if ok { Phase::Done } else { Phase::Failed };
        st.items[i].note = note;
        st.done += 1;
        if !self.fancy {
            let (d, n) = (st.done, st.items.len());
            let it = &st.items[i];
            println!("({d}/{n}) {} {}: {}", if ok { "unpacked" } else { "FAILED" }, it.label, it.note);
        }
    }

    pub fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
            draw(&self.st);
            print!("\x1b[?25h\x1b[?1049l");
            let _ = std::io::stdout().flush();
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if self.thread.is_some() {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
            print!("\x1b[?25h\x1b[?1049l");
            let _ = std::io::stdout().flush();
        }
    }
}

fn draw(st: &Mutex<State>) {
    let (rows, cols) = sys::term_size(1).unwrap_or((24, 80));
    let (rows, cols) = (rows as usize, cols.max(40) as usize);
    let st = st.lock().unwrap();
    let mut out = String::from("\x1b[H");
    let line = |s: String, out: &mut String| {
        out.push_str(&trunc(&s, cols));
        out.push_str("\x1b[K\r\n");
    };
    line(format!("vpm: {}", st.title), &mut out);
    line(String::new(), &mut out);
    let active: Vec<&Item> =
        st.items.iter().filter(|i| matches!(i.phase, Phase::Fetching | Phase::Unpacking)).collect();
    let room = rows.saturating_sub(6).max(1);
    let lw = st.items.iter().map(|i| i.label.chars().count()).max().unwrap_or(10).min(cols / 2);
    for it in active.iter().take(room) {
        let what = if it.phase == Phase::Fetching { "fetch " } else { "unpack" };
        let pct = if it.total == 0 { 100 } else { (it.cur * 100 / it.total.max(1)).min(100) };
        line(
            format!(
                "  {what} {:<lw$} {} {:>3}%  {}",
                trunc(&it.label, lw),
                bar(it.cur, it.total, 20),
                pct,
                human(it.cur)
            ),
            &mut out,
        );
    }
    if active.len() > room {
        line(format!("  ... and {} more", active.len() - room), &mut out);
    }
    let failed = st.items.iter().filter(|i| i.phase == Phase::Failed).count();
    line(String::new(), &mut out);
    let bw = cols.saturating_sub(50).clamp(10, 50);
    line(
        format!(
            "  {} {}/{} packages, {} / {}{}  {:.1}s",
            bar(st.work, st.work_total, bw),
            st.done,
            st.items.len(),
            human(st.work),
            human(st.work_total),
            if failed > 0 { format!(", {failed} FAILED") } else { String::new() },
            st.started.elapsed().as_secs_f64()
        ),
        &mut out,
    );
    out.push_str("\x1b[J");
    let mut so = std::io::stdout().lock();
    let _ = so.write_all(out.as_bytes());
    let _ = so.flush();
}
