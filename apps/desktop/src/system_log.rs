use std::{collections::VecDeque, time::Instant};

const MAX_ENTRIES: usize = 1_000;
const MAX_BYTES: usize = 256 * 1024;
const MAX_MESSAGE_BYTES: usize = 8 * 1024;

pub struct SystemLog {
    started: Instant,
    entries: VecDeque<String>,
    bytes: usize,
    last_message: Option<(String, String)>,
}

impl Default for SystemLog {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            entries: VecDeque::new(),
            bytes: 0,
            last_message: None,
        }
    }
}

impl SystemLog {
    pub fn record(&mut self, source: &str, message: &str) {
        if message.trim().is_empty() {
            return;
        }
        let elapsed = self.started.elapsed().as_secs();
        let mut end = message.len().min(MAX_MESSAGE_BYTES);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        let truncated = &message[..end];
        if self
            .last_message
            .as_ref()
            .is_some_and(|(last_source, last_message)| {
                last_source == source && last_message == truncated
            })
        {
            return;
        }
        self.last_message = Some((source.to_owned(), truncated.to_owned()));
        let suffix = if end < message.len() { "…" } else { "" };
        let entry = format!(
            "[+{:02}:{:02}:{:02}] [{}] {}{}",
            elapsed / 3600,
            elapsed / 60 % 60,
            elapsed % 60,
            source,
            truncated,
            suffix
        );
        self.bytes += entry.len();
        self.entries.push_back(entry);
        while self.entries.len() > MAX_ENTRIES || self.bytes > MAX_BYTES {
            if let Some(entry) = self.entries.pop_front() {
                self.bytes -= entry.len();
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
        self.last_message = None;
    }

    pub fn text(&self) -> String {
        self.entries
            .iter()
            .rev()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_recent_events_and_truncates_unicode_safely() {
        let mut log = SystemLog::default();
        log.record("应用", &"断".repeat(MAX_MESSAGE_BYTES));
        assert!(log.text().ends_with('…'));
        for i in 0..=MAX_ENTRIES {
            log.record("应用", &format!("事件 {i}"));
        }
        assert_eq!(log.entries.len(), MAX_ENTRIES);
        assert!(!log.text().contains("事件 0\n"));
        assert!(log.text().contains("事件 1000"));
        assert!(log.text().lines().next().unwrap().ends_with("事件 1000"));
        log.record("应用", "事件 1000");
        assert_eq!(log.entries.len(), MAX_ENTRIES);
        log.clear();
        assert!(log.text().is_empty());
        assert_eq!(log.bytes, 0);
    }
}
