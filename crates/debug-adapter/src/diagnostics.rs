//! File-backed tool diagnostics avoid pipe deadlocks and retain startup errors.
use crate::error;
use nya_domain::{Error, Result};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    process::Command,
};

pub(crate) struct Diagnostics {
    stdout: File,
    stderr: File,
}

impl Diagnostics {
    pub(crate) fn capture(command: &mut Command) -> Result<Self> {
        let stdout = tempfile::tempfile().map_err(error)?;
        let stderr = tempfile::tempfile().map_err(error)?;
        command.stdout(stdout.try_clone().map_err(error)?);
        command.stderr(stderr.try_clone().map_err(error)?);
        Ok(Self { stdout, stderr })
    }

    pub(crate) fn failure(&mut self, cause: impl std::fmt::Display) -> Error {
        fn tail(file: &mut File) -> std::io::Result<String> {
            let length = file.metadata()?.len();
            file.seek(SeekFrom::Start(length.saturating_sub(4096)))?;
            let mut bytes = Vec::new();
            file.take(4096).read_to_end(&mut bytes)?;
            Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
        }
        let stderr = tail(&mut self.stderr).unwrap_or_default();
        let stdout = tail(&mut self.stdout).unwrap_or_default();
        Error(format!("{cause}\n{stderr}\n{stdout}").trim().to_owned())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn captures_tool_errors_without_blocking_on_verbose_output() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "printf 'probe startup failed\\n'; head -c 131072 /dev/zero >&2; printf 'Selected transport is not supported by the device\\n' >&2; exit 1",
        ]);
        let mut diagnostics = Diagnostics::capture(&mut command).unwrap();
        let status = command.status().unwrap();
        assert!(!status.success());
        let failure = diagnostics.failure(format!("server exited: {status}"));
        assert!(failure.0.contains("server exited:"));
        assert!(failure.0.contains("Selected transport is not supported"));
        assert!(failure.0.contains("probe startup failed"));
        assert!(failure.0.len() < 9000);
    }
}
