use gcli_lib::commands::CommandOutput;
use gcli_lib::error::{Error, emit_json_error, exit_code};
use gcli_lib::output::OutputMode;
use std::io::{BufWriter, Write};

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
    // Results stream through a BufWriter over the stdout lock instead of
    // being assembled in a Vec first: a big JSON output never exists twice
    // (once buffered, once written).
    let mut out = BufWriter::new(std::io::stdout().lock());
    let mut err = std::io::stderr().lock();
    for (name, res) in results {
        match res {
            Ok(output) => {
                if let Err(e) = write_multi_ok(&mut out, name, output, mode) {
                    handle_stdout_error(e);
                }
            }
            Err(e) => {
                let _ = writeln!(err, "{}", emit_json_error(Some(name), e));
            }
        }
    }
    if let Err(e) = out.flush() {
        handle_stdout_error(e);
    }
    gcli_lib::fanout::aggregate_exit_code(results)
}

fn write_multi_ok(
    w: &mut impl Write,
    name: &str,
    out: &CommandOutput,
    mode: OutputMode,
) -> std::io::Result<()> {
    match mode {
        OutputMode::Json => match out {
            CommandOutput::Logs(records) => gcli_lib::output::write_jsonl(w, records),
            CommandOutput::Json(v) => {
                writeln!(w, "{}", serde_json::json!({"profile": name, "data": v}))
            }
            CommandOutput::Table { headers, rows } => writeln!(
                w,
                "{}",
                serde_json::json!({"profile": name, "data": {"headers": headers, "rows": rows}})
            ),
        },
        OutputMode::Table => {
            writeln!(w, "profile: {name}")?;
            match out {
                CommandOutput::Table { headers, rows } => {
                    writeln!(w, "{}", gcli_lib::output::render_table(headers, rows))
                }
                CommandOutput::Json(v) => writeln!(w, "{v}"),
                CommandOutput::Logs(records) => gcli_lib::output::write_jsonl(w, records),
            }
        }
    }
}

pub(crate) fn print_output(out: &CommandOutput) {
    let mut out_writer = BufWriter::new(std::io::stdout().lock());
    if let Err(e) = write_output(&mut out_writer, out).and_then(|()| out_writer.flush()) {
        handle_stdout_error(e);
    }
}

fn write_output(w: &mut impl Write, out: &CommandOutput) -> std::io::Result<()> {
    match out {
        CommandOutput::Json(value) => writeln!(w, "{value}"),
        CommandOutput::Logs(records) => gcli_lib::output::write_jsonl(w, records),
        CommandOutput::Table { headers, rows } => {
            writeln!(w, "{}", gcli_lib::output::render_table(headers, rows))
        }
    }
}

/// Write whole, already-built `bytes` (e.g. shell completions) to stdout.
pub(crate) fn write_stdout(bytes: &[u8]) {
    let mut out = std::io::stdout().lock();
    if let Err(e) = out.write_all(bytes).and_then(|()| out.flush()) {
        handle_stdout_error(e);
    }
}

/// One `logs search --follow` batch: JSONL, written and flushed as it
/// arrives so a consumer tailing gcli sees each batch immediately (`--output`
/// is ignored in follow mode). A closed stdout ends the run cleanly.
pub(crate) fn print_follow_batch(records: &[gcli_lib::output::LogRecord]) {
    let mut out = std::io::stdout().lock();
    if let Err(e) = gcli_lib::output::write_jsonl(&mut out, records).and_then(|()| out.flush()) {
        handle_stdout_error(e);
    }
}

/// A stdout write or flush failed. Rust ignores SIGPIPE, so when the reader
/// closes early (e.g. `gcli datasources | head`) the write returns `BrokenPipe`
/// here instead of killing the process; we treat that as a clean success and
/// exit 0 quietly. Any other stdout write error is emitted as the structured
/// JSON error on stderr and exits with that error's code.
fn handle_stdout_error(e: std::io::Error) {
    if e.kind() == std::io::ErrorKind::BrokenPipe {
        std::process::exit(0);
    }
    let err = Error::Io(e);
    eprintln!("{}", emit_json_error(None, &err));
    std::process::exit(exit_code(&err));
}

pub(crate) fn fail(profile: Option<&str>, err: &Error) -> i32 {
    eprintln!("{}", emit_json_error(profile, err));
    exit_code(err)
}
