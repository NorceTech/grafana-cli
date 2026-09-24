use gcli_lib::commands::CommandOutput;
use gcli_lib::error::{Error, emit_json_error, exit_code};
use gcli_lib::output::OutputMode;
use std::io::Write;

pub(crate) fn print_results(
    results: &[(String, gcli_lib::Result<CommandOutput>)],
    mode: OutputMode,
) -> i32 {
    if results.len() == 1 {
        let (name, res) = &results[0];
        return match res {
            Ok(out) => {
                print_output(out);
                0
            }
            Err(e) => fail(Some(name), e),
        };
    }
    let mut buf: Vec<u8> = Vec::new();
    let mut err = std::io::stderr().lock();
    for (name, res) in results {
        match res {
            Ok(output) => print_multi_ok(&mut buf, name, output, mode),
            Err(e) => {
                let _ = writeln!(err, "{}", emit_json_error(Some(name), e));
            }
        }
    }
    write_stdout(&buf);
    let refs: Vec<(&String, &gcli_lib::Result<CommandOutput>)> =
        results.iter().map(|(n, r)| (n, r)).collect();
    gcli_lib::fanout::aggregate_exit_code(&refs)
}

fn print_multi_ok(w: &mut impl Write, name: &str, out: &CommandOutput, mode: OutputMode) {
    match mode {
        OutputMode::Json => match out {
            CommandOutput::Logs(records) => {
                let _ = gcli_lib::output::write_jsonl(w, records);
            }
            CommandOutput::Json(v) => {
                let _ = writeln!(w, "{}", serde_json::json!({"profile": name, "data": v}));
            }
            CommandOutput::Table { headers, rows } => {
                let _ = writeln!(
                    w,
                    "{}",
                    serde_json::json!({"profile": name, "data": {"headers": headers, "rows": rows}})
                );
            }
        },
        OutputMode::Table => {
            let _ = writeln!(w, "profile: {name}");
            match out {
                CommandOutput::Table { headers, rows } => {
                    let _ = writeln!(w, "{}", gcli_lib::output::render_table(headers, rows));
                }
                CommandOutput::Json(v) => {
                    let _ = writeln!(w, "{v}");
                }
                CommandOutput::Logs(records) => {
                    let _ = gcli_lib::output::write_jsonl(w, records);
                }
            }
        }
    }
}

pub(crate) fn print_output(out: &CommandOutput) {
    let mut buf: Vec<u8> = Vec::new();
    match out {
        CommandOutput::Json(value) => {
            let _ = writeln!(buf, "{value}");
        }
        CommandOutput::Logs(records) => {
            // Vec write is infallible; a real pipe/write error surfaces in write_stdout.
            let _ = gcli_lib::output::write_jsonl(&mut buf, records);
        }
        CommandOutput::Table { headers, rows } => {
            let _ = writeln!(buf, "{}", gcli_lib::output::render_table(headers, rows));
        }
    }
    write_stdout(&buf);
}

/// Write `bytes` to stdout and flush. Rust ignores SIGPIPE, so when the reader
/// closes early (e.g. `gcli datasources | head`) the write returns `BrokenPipe`
/// here instead of killing the process; we treat that as a clean success and
/// exit 0 quietly. Any other stdout write error is emitted as the structured
/// JSON error on stderr and exits with that error's code.
pub(crate) fn write_stdout(bytes: &[u8]) {
    let mut out = std::io::stdout().lock();
    if let Err(e) = out.write_all(bytes).and_then(|()| out.flush()) {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        let err = Error::Io(e);
        eprintln!("{}", emit_json_error(None, &err));
        std::process::exit(exit_code(&err));
    }
}

pub(crate) fn fail(profile: Option<&str>, err: &Error) -> i32 {
    eprintln!("{}", emit_json_error(profile, err));
    exit_code(err)
}
