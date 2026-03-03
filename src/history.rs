use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;

use chrono::Local;

pub struct HistoryWriter {
    file: File,
}

impl HistoryWriter {
    /// Creates a timestamped log file in `~/.localchat/history/`.
    /// Returns `None` if the directory or file cannot be created.
    pub fn new(my_name: &str, peer_name: &str) -> Option<Self> {
        let dir = history_dir()?;
        fs::create_dir_all(&dir).ok()?;

        let filename = format!(
            "{}_{}.txt",
            Local::now().format("%Y-%m-%d_%H-%M-%S"),
            peer_name.replace(' ', "_")
        );

        let mut file = File::create(dir.join(&filename)).ok()?;
        let _ = writeln!(file, "╔══════════════════════════════════════════╗");
        let _ = writeln!(file, "║           LocalChat Session Log          ║");
        let _ = writeln!(file, "╚══════════════════════════════════════════╝");
        let _ = writeln!(file, "  Participants : {} ↔ {}", my_name, peer_name);
        let _ = writeln!(
            file,
            "  Started      : {}",
            Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        let _ = writeln!(file, "  Encryption   : X25519 + ChaCha20-Poly1305");
        let _ = writeln!(file, "──────────────────────────────────────────────");
        let _ = writeln!(file);

        Some(HistoryWriter { file })
    }

    pub fn write_message(&mut self, timestamp: &str, sender: &str, content: &str) {
        let _ = writeln!(self.file, "[{}] {}: {}", timestamp, sender, content);
    }

    pub fn write_event(&mut self, event: &str) {
        let _ = writeln!(
            self.file,
            "── {} ─ {} ──",
            event,
            Local::now().format("%H:%M:%S")
        );
    }
}

fn history_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".localchat").join("history"))
}

/// Returns a human-readable path to the history directory.
pub fn history_dir_display() -> String {
    history_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.localchat/history".to_string())
}
