//! `*_command` secrets: run at request time. A single-element array is a
//! shell command line run with `sh -c` (so `~` expansion and pipes keep
//! working); an array of two or more elements is executed directly as argv,
//! without a shell, so elements containing spaces survive intact. A non-zero
//! exit or empty output is an auth error.

use crate::{Result, error::Error};

pub(super) fn exec_command(cmd: &[String], kind: &str) -> Result<String> {
    let joined = cmd.join(" ");
    let auth_error = |message: String| Error::Auth {
        kind: kind.into(),
        message,
    };
    let mut command = match cmd {
        [] => return Err(auth_error("command is empty".into())),
        [line] => {
            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg(line);
            command
        }
        [program, args @ ..] => {
            let mut command = std::process::Command::new(program);
            command.args(args);
            command
        }
    };
    let output = command
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

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cmd: &[&str]) -> Result<String> {
        exec_command(
            &cmd.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "token_command",
        )
    }

    #[test]
    fn single_element_runs_as_a_shell_line() {
        // Pipes (and ~ expansion) are why one element stays a shell line.
        assert_eq!(run(&["echo a | tr a b"]).unwrap(), "b");
    }

    #[test]
    fn multi_element_argv_keeps_spaces_intact() {
        // Under `sh -c "<joined>"` the two words of the last element would be
        // split into separate printf arguments and the secret would be
        // "op://vault/GrafanaProd/token".
        assert_eq!(
            run(&["printf", "%s", "op://vault/Grafana Prod/token"]).unwrap(),
            "op://vault/Grafana Prod/token"
        );
    }

    #[test]
    fn multi_element_runs_the_program_directly() {
        // `sh -c "printf [%s] a b"` would print "[a][b]"; direct argv keeps
        // "a b" one argument.
        assert_eq!(run(&["printf", "[%s]", "a b"]).unwrap(), "[a b]");
    }
}
