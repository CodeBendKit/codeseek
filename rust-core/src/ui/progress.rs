use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

/// A lightweight animated progress bar (spinner + status text).
/// Runs a background thread that redraws a single terminal line.
pub struct ProgressBar {
    done: Arc<AtomicBool>,
    files: Arc<AtomicUsize>,
    funcs: Arc<AtomicUsize>,
    phase: Arc<std::sync::Mutex<String>>,
    start: Instant,
}

impl ProgressBar {
    /// Create and start the spinner.
    pub fn start(phase: &str) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let files = Arc::new(AtomicUsize::new(0));
        let funcs = Arc::new(AtomicUsize::new(0));
        let phase = Arc::new(std::sync::Mutex::new(phase.to_string()));
        let start = Instant::now();

        let d = done.clone();
        let f = files.clone();
        let fc = funcs.clone();
        let p = phase.clone();
        let s = start;

        std::thread::spawn(move || {
            let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut i = 0;
            while !d.load(Ordering::Relaxed) {
                let files = f.load(Ordering::Relaxed);
                let funcs = fc.load(Ordering::Relaxed);
                let phase = p.lock().unwrap().clone();
                let elapsed = s.elapsed().as_secs();

                let files_str = if files > 0 { format!("{} files", files) } else { String::new() };
                let funcs_str = if funcs > 0 { format!("{} symbols", funcs) } else { String::new() };
                let time_str = format!("{}s", elapsed);

                let parts: Vec<&str> = [files_str.as_str(), funcs_str.as_str(), time_str.as_str()]
                    .iter()
                    .filter(|s| !s.is_empty())
                    .copied()
                    .collect();

                eprint!(
                    "\x1b[2K\r  {} {} {}",
                    spinner[i % spinner.len()],
                    phase,
                    parts.join("  │  ")
                );

                i += 1;
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            // Final clear line
            eprint!("\x1b[2K\r");
        });

        Self { done, files, funcs, phase, start }
    }

    /// Update the phase text.
    pub fn set_phase(&self, text: &str) {
        *self.phase.lock().unwrap() = text.to_string();
    }

    /// Update file count.
    pub fn set_files(&self, n: usize) {
        self.files.store(n, Ordering::Relaxed);
    }

    /// Update function/symbol count.
    pub fn set_funcs(&self, n: usize) {
        self.funcs.store(n, Ordering::Relaxed);
    }

    /// Stop the spinner and print the final summary.
    pub fn finish(self, msg: &str) {
        self.done.store(true, Ordering::Relaxed);
        let elapsed = self.start.elapsed().as_secs();
        let files = self.files.load(Ordering::Relaxed);
        let funcs = self.funcs.load(Ordering::Relaxed);
        eprintln!("  \x1b[32m✓\x1b[0m {} ({} files, {} symbols, {}s)", msg, files, funcs, elapsed);
    }
}
