//! Console + file logging. The file sink matters because the tool is meant to be
//! started minimized / at login, where the console is not visible.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

static SINK: OnceLock<Mutex<std::fs::File>> = OnceLock::new();

pub fn init(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = SINK.set(Mutex::new(file));
    }
}

pub fn line(msg: &str) {
    let stamp = crate::platform::now_hms();
    // writeln! rather than println!: writing to a closed pipe returns an error
    // instead of panicking, which on a long-running daemon would be a silent
    // death. The log file below is the record that actually matters.
    let _ = writeln!(std::io::stdout(), "[{}] {}", stamp, msg);
    let _ = std::io::stdout().flush();
    if let Some(sink) = SINK.get() {
        if let Ok(mut file) = sink.lock() {
            let _ = writeln!(file, "[{}] {}", stamp, msg);
            let _ = file.flush();
        }
    }
}
