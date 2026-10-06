//! curl transport: used on macOS and Linux, where curl is part of the base
//! system, and selectable on Windows with `transport = curl`.
//!
//! The request body goes through a temporary file rather than the child's stdin,
//! which keeps the plumbing simple and avoids any chance of a pipe deadlock.

use super::Endpoint;
use std::path::PathBuf;
use std::process::Command;

struct TempFile(PathBuf);

impl TempFile {
    fn new(suffix: &str, contents: Option<&[u8]>) -> Result<TempFile, String> {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "freespeak-{}-{}-{suffix}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        path.push(unique);
        if let Some(bytes) = contents {
            std::fs::write(&path, bytes)
                .map_err(|e| format!("could not write {}: {e}", path.display()))?;
        }
        Ok(TempFile(path))
    }

    fn path(&self) -> &PathBuf {
        &self.0
    }

    fn read(&self) -> Result<String, String> {
        std::fs::read_to_string(&self.0)
            .map_err(|e| format!("could not read {}: {e}", self.0.display()))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn post(
    endpoint: &Endpoint,
    headers: &[(String, String)],
    body: &[u8],
) -> Result<(u16, String), String> {
    let body_file = TempFile::new("body", Some(body))?;
    let response_file = TempFile::new("response", None)?;

    let mut command = Command::new("curl");
    command
        .arg("--silent")
        .arg("--show-error")
        .arg("--location")
        .arg("--request")
        .arg("POST")
        .arg("--connect-timeout")
        .arg("15")
        .arg("--max-time")
        .arg("120")
        .arg("--output")
        .arg(response_file.path())
        .arg("--write-out")
        .arg("%{http_code}")
        .arg("--data-binary")
        .arg(format!("@{}", body_file.path().display()));
    for (name, value) in headers {
        command.arg("--header").arg(format!("{name}: {value}"));
    }
    command.arg(&endpoint.url);

    let output = command.output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "curl was not found on PATH (install it, or set `transport = winhttp`)".to_string()
        } else {
            format!("could not run curl: {e}")
        }
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "curl failed ({}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }

    let status: u16 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| "curl did not report an HTTP status".to_string())?;

    Ok((status, response_file.read()?))
}
