//! Bounded synchronous work behind the MCP transport.
use std::{
    fs::File,
    io,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub(crate) struct Budget {
    deadline: Instant,
    pub(crate) cancelled: CancellationToken,
}

impl Budget {
    pub(crate) fn new(milliseconds: u64, cancelled: CancellationToken) -> Self {
        Self {
            deadline: Instant::now() + Duration::from_millis(milliseconds),
            cancelled,
        }
    }

    pub(crate) fn remaining(&self) -> io::Result<Duration> {
        if self.cancelled.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "operation cancelled",
            ));
        }
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "operation deadline exceeded"))
    }

    pub(crate) fn lock(&self, file: &File) -> io::Result<()> {
        self.acquire(file, false)
    }
    pub(crate) fn lock_shared(&self, file: &File) -> io::Result<()> {
        self.acquire(file, true)
    }

    fn acquire(&self, file: &File, shared: bool) -> io::Result<()> {
        loop {
            let remaining = self.remaining()?;
            match if shared {
                file.try_lock_shared()
            } else {
                file.try_lock()
            } {
                Ok(()) => return Ok(()),
                Err(std::fs::TryLockError::WouldBlock) => {
                    std::thread::sleep(remaining.min(Duration::from_millis(10)))
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        }
    }
}

/// Git discovery never inherits protocol stdin/stdout and cannot wait for a prompt.
pub(crate) fn git_output(command: &mut std::process::Command) -> io::Result<std::process::Output> {
    use std::{io::Read, process::Stdio};
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    fn capture(stream: impl Read) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        stream.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 1024 * 1024 {
            return Err(io::Error::other("Git output exceeds 1 MiB"));
        }
        Ok(bytes)
    }
    let out = std::thread::spawn(move || capture(stdout));
    let err = std::thread::spawn(move || capture(stderr));
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            result => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(result.err().unwrap_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Git discovery exceeded five seconds",
                    )
                }));
            }
        }
    };
    let stdout = out
        .join()
        .map_err(|_| io::Error::other("Git stdout reader failed"))??;
    let stderr = err
        .join()
        .map_err(|_| io::Error::other("Git stderr reader failed"))??;
    Ok(std::process::Output {
        status: status?,
        stdout,
        stderr,
    })
}
