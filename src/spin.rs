//! Braille spinner on stderr for slow steps. No-op when stderr isn't a TTY.

use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

pub struct Spinner {
    stop: Arc<AtomicBool>,
    t: Option<JoinHandle<()>>,
}

pub fn start(msg: &str) -> Spinner {
    let stop = Arc::new(AtomicBool::new(false));
    if !std::io::stderr().is_terminal() {
        return Spinner { stop, t: None };
    }
    let flag = stop.clone();
    let msg = msg.to_string();
    let t = std::thread::spawn(move || {
        const F: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        // Hold off briefly — a cache-hit load finishes before we ever draw.
        std::thread::sleep(Duration::from_millis(150));
        let mut i = 0usize;
        while !flag.load(Ordering::Relaxed) {
            eprint!("\r\x1b[36m{}\x1b[0m {}", F[i % F.len()], msg);
            i += 1;
            std::thread::sleep(Duration::from_millis(80));
        }
        eprint!("\r{}\r", " ".repeat(msg.len() + 4));
    });
    Spinner { stop, t: Some(t) }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.t.take() {
            let _ = t.join();
        }
    }
}
