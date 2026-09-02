//! Child process execution with process-group kill on timeout.
//!
//! bun/vitest spawn their own worker children; killing only the direct child
//! orphans grandchildren, so every spawn gets its own process group (setsid)
//! and timeouts kill the whole group.

use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Debug)]
pub struct RunOutput {
    pub timed_out: bool,
    /// None when killed by a signal.
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}

impl RunOutput {
    pub fn success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }

    /// Last ~40 lines of stderr (or stdout as fallback) for diagnostics.
    pub fn diagnostic_tail(&self) -> String {
        let text = if self.stderr.trim().is_empty() { &self.stdout } else { &self.stderr };
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(40);
        lines[start..].join("\n")
    }
}

pub async fn run_with_timeout(mut command: Command, timeout: Duration) -> anyhow::Result<RunOutput> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    command.kill_on_drop(true);
    #[cfg(unix)]
    {
        // Own process group so we can kill the whole tree.
        command.process_group(0);
    }

    let start = Instant::now();
    let mut child = command.spawn()?;
    let pid = child.id();

    let mut stdout_pipe = child.stdout.take().expect("stdout piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped");
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf).await;
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf).await;
        buf
    });

    let timed_out = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => {
            let status = status?;
            let elapsed = start.elapsed();
            let stdout = String::from_utf8_lossy(&stdout_task.await.unwrap_or_default()).into_owned();
            let stderr = String::from_utf8_lossy(&stderr_task.await.unwrap_or_default()).into_owned();
            return Ok(RunOutput {
                timed_out: false,
                exit_code: status.code(),
                stdout,
                stderr,
                elapsed,
            });
        }
        Err(_) => true,
    };

    // Timeout: kill the whole process group, then reap.
    #[cfg(unix)]
    if let Some(pid) = pid {
        unsafe {
            libc::killpg(pid as i32, libc::SIGKILL);
        }
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
    let stdout = String::from_utf8_lossy(&stdout_task.await.unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_task.await.unwrap_or_default()).into_owned();
    Ok(RunOutput {
        timed_out,
        exit_code: None,
        stdout,
        stderr,
        elapsed: start.elapsed(),
    })
}
