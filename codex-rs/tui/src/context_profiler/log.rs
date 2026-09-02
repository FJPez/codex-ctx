//! Append-only JSONL sink for profiler records.

use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use super::RecordedEvent;

pub(crate) struct ProfilerLog {
    file: Mutex<File>,
}

impl ProfilerLog {
    /// One file per process; the pid keeps same-second starts from sharing a trace.
    pub(crate) fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let filename = format!(
            "context-profiler-{}-{}.jsonl",
            chrono::Utc::now().format("%Y%m%d-%H%M%S"),
            std::process::id()
        );

        let mut opts = OpenOptions::new();
        opts.create(true).append(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }

        Ok(Self::with_file(opts.open(dir.join(filename))?))
    }

    pub(crate) fn with_file(file: File) -> Self {
        Self {
            file: Mutex::new(file),
        }
    }

    pub(crate) fn write(&self, record: &RecordedEvent) -> std::io::Result<()> {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        let mut guard = match self.file.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.write_all(line.as_bytes())?;
        guard.flush()
    }
}
