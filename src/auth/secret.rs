//! `*_command` secrets: run at request time. A single-element array is a
//! shell command line run with `sh -c` (so `~` expansion and pipes keep
//! working); an array of two or more elements is executed directly as argv,
//! without a shell, so elements containing spaces survive intact. Every run
//! gets a null stdin and is bounded by `timeout`; a non-zero exit, empty
//! output, output with control characters after trimming, or a timeout is an
//! auth error naming the command, and a failure also quotes the end of the
//! command's stderr.

use crate::{Result, error::Error};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// How much of a failed command's stderr the auth error quotes, in chars.
const STDERR_CHARS: usize = 200;

pub(super) async fn exec_command(cmd: &[String], kind: &str, timeout: Duration) -> Result<String> {
    let joined = cmd.join(" ");
    let auth_error = |message: String| Error::Auth {
        kind: kind.into(),
        message,
    };
    let mut command = match cmd {
        [] => return Err(auth_error("command is empty".into())),
        [line] => {
            let mut command = Command::new("sh");
            command.arg("-c").arg(line);
            command
        }
        [program, args @ ..] => {
            let mut command = Command::new(program);
            command.args(args);
            command
        }
    };
    // A null stdin: a helper must never consume (or block on) ours. A timeout
    // leaves the future dropped, so kill_on_drop stops a runaway child.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| {
            auth_error(format!(
                "'{joined}' timed out after {}",
                humantime::format_duration(timeout)
            ))
        })?
        .map_err(|e| auth_error(format!("spawn '{joined}': {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim_end();
        let mut message = format!(
            "'{joined}' exited with {}",
            output.status.code().unwrap_or(-1)
        );
        if !stderr.is_empty() {
            message.push_str(": ");
            message.push_str(tail(stderr, STDERR_CHARS));
        }
        return Err(auth_error(message));
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return Err(auth_error(format!("'{joined}' produced empty output")));
    }
    if stdout.chars().any(char::is_control) {
        return Err(auth_error(format!(
            "'{joined}' output contains control characters"
        )));
    }
    Ok(stdout)
}

/// The last `max` chars of `text`; the cut never splits a char.
fn tail(text: &str, max: usize) -> &str {
    let skip = text.chars().count().saturating_sub(max);
    text.char_indices()
        .nth(skip)
        .map_or(text, |(start, _)| &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;

    // Generous for fast commands: only the timeout test needs a small one.
    const BUDGET: Duration = Duration::from_secs(10);

    async fn run(cmd: &[&str]) -> Result<String> {
        exec_command(
            &cmd.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            "token_command",
            BUDGET,
        )
        .await
    }

    #[tokio::test]
    async fn single_element_runs_as_a_shell_line() {
        // Pipes (and ~ expansion) are why one element stays a shell line.
        assert_eq!(run(&["echo a | tr a b"]).await.unwrap(), "b");
    }

    #[tokio::test]
    async fn multi_element_argv_keeps_spaces_intact() {
        // Under `sh -c "<joined>"` the two words of the last element would be
        // split into separate printf arguments and the secret would be
        // "op://vault/GrafanaProd/token".
        assert_eq!(
            run(&["printf", "%s", "op://vault/Grafana Prod/token"])
                .await
                .unwrap(),
            "op://vault/Grafana Prod/token"
        );
    }

    #[tokio::test]
    async fn multi_element_runs_the_program_directly() {
        // `sh -c "printf [%s] a b"` would print "[a][b]"; direct argv keeps
        // "a b" one argument.
        assert_eq!(run(&["printf", "[%s]", "a b"]).await.unwrap(), "[a b]");
    }

    #[tokio::test]
    async fn a_command_that_never_finishes_times_out() {
        let cmd: Vec<String> = ["sleep", "30"].map(String::from).to_vec();

        let e = exec_command(&cmd, "token_command", Duration::from_millis(100))
            .await
            .unwrap_err();

        assert!(
            matches!(&e, Error::Auth { kind, .. } if kind == "token_command"),
            "{e:?}"
        );
        let message = e.to_string();
        assert!(message.contains("sleep 30"), "{message}");
        assert!(message.contains("timed out"), "{message}");
        assert!(message.contains("100ms"), "{message}");
    }

    // The failure message names the command, its exit code and the end of
    // its stderr: a 403-char stderr is clipped to its last 200 chars (which
    // the "exited with 3" text alone never shows).
    #[tokio::test]
    async fn a_failure_message_quotes_the_end_of_stderr() {
        let e = run(&[
            "sh",
            "-c",
            "printf 'y%.0s' $(seq 400) >&2; printf 'END\\n' >&2; exit 3",
        ])
        .await
        .unwrap_err();

        let message = e.to_string();
        assert!(message.contains("exited with 3"), "{message}");
        let (_, quote) = message
            .split_once("exited with 3: ")
            .expect("stderr is quoted");
        assert_eq!(quote.chars().count(), 200, "{message}");
        assert!(quote.ends_with("END"), "{message}");
    }

    #[tokio::test]
    async fn output_with_control_characters_is_rejected() {
        // printf turns the two characters \ and n into a real newline inside
        // the secret; trim() cannot remove it because it is not at an end.
        let e = run(&["printf", "a\\nb"]).await.unwrap_err();

        assert!(
            matches!(&e, Error::Auth { kind, .. } if kind == "token_command"),
            "{e:?}"
        );
        assert!(e.to_string().contains("control characters"), "{e}");
    }

    #[tokio::test]
    async fn an_empty_command_list_is_an_auth_error() {
        let e = exec_command(&[], "client_secret_command", BUDGET)
            .await
            .unwrap_err();

        assert!(matches!(e, Error::Auth { .. }), "{e:?}");
    }

    #[test]
    fn tail_keeps_the_last_max_chars_without_splitting_one() {
        assert_eq!(tail("abcdef", 4), "cdef");
        assert_eq!(tail("abc", 4), "abc");
        assert_eq!(tail("\u{e9}\u{e9}e", 2), "\u{e9}e");
        assert_eq!(tail("", 4), "");
    }
}
