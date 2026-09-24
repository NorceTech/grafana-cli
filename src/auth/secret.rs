//! `*_command` secrets: run as `sh -c "<args joined by spaces>"` at request
//! time; a non-zero exit or empty output is an auth error.

use crate::{Result, error::Error};

pub(super) fn exec_command(cmd: &[String], kind: &str) -> Result<String> {
    let joined = cmd.join(" ");
    let auth_error = |message: String| Error::Auth {
        kind: kind.into(),
        message,
    };
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&joined)
        .output()
        .map_err(|e| auth_error(format!("spawn '{joined}': {e}")))?;
    if !output.status.success() {
        return Err(auth_error(format!(
            "'{joined}' exited with {}",
            output.status.code().unwrap_or(-1)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err(auth_error(format!("'{joined}' produced empty output")));
    }
    Ok(stdout)
}
