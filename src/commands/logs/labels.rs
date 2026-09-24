use crate::Result;
use crate::client::{GrafanaClient, Reply};
use crate::commands::{CommandOutput, QueryWindow, Windowed, success_data};
use crate::error::Error;
use serde::Deserialize;
use serde_json::Value;

use super::loki_context;

/// The names in a `labels` or `label/{label}/values` reply.
fn string_array(reply: &Reply) -> Result<Vec<String>> {
    // Loki leaves `data` out when no label or value matches.
    let Some(data) = success_data(reply)? else {
        return Ok(Vec::new());
    };
    Vec::<String>::deserialize(data).map_err(|e| reply.malformed(&format!("data: {e}")))
}

fn json_string_array(names: Vec<String>) -> Value {
    Value::Array(names.into_iter().map(Value::String).collect())
}

pub async fn labels(
    client: &GrafanaClient,
    _profile: &str,
    window: Windowed<'_, QueryWindow>,
    selector: Option<&str>,
) -> Result<CommandOutput> {
    let (start, end, ds) = loki_context(client, window).await?;
    let mut pairs = vec![("start", start), ("end", end)];
    if let Some(selector) = selector {
        pairs.push(("query", selector.to_string()));
    }
    let reply = client
        .proxy_request(&ds.uid, "loki/api/v1/labels", &pairs)
        .await?;
    Ok(CommandOutput::Json(json_string_array(string_array(
        &reply,
    )?)))
}

/// Rejects a label name Loki would not accept, `[A-Za-z_][A-Za-z0-9_]*`,
/// before it becomes a segment of `label/{label}/values`; see
/// `traces::validate_trace_id` for why.
pub(crate) fn validate_label_name(label: &str) -> Result<()> {
    let mut bytes = label.bytes();
    let valid = bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_');
    if valid {
        return Ok(());
    }
    Err(Error::Config {
        message: format!("invalid label name: '{label}' (expected [A-Za-z_][A-Za-z0-9_]*)"),
    })
}

pub async fn label_values(
    client: &GrafanaClient,
    _profile: &str,
    label: &str,
    window: Windowed<'_, QueryWindow>,
    selector: Option<&str>,
) -> Result<CommandOutput> {
    let (start, end, ds) = loki_context(client, window).await?;
    let mut pairs = vec![("start", start), ("end", end)];
    if let Some(selector) = selector {
        pairs.push(("query", selector.to_string()));
    }
    let reply = client
        .proxy_request(
            &ds.uid,
            &format!("loki/api/v1/label/{label}/values"),
            &pairs,
        )
        .await?;
    Ok(CommandOutput::Json(json_string_array(string_array(
        &reply,
    )?)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn loki_label_names_are_valid() {
        for label in ["job", "_", "__name__", "Job_9"] {
            let result = validate_label_name(label);

            assert!(result.is_ok(), "{label}: {result:?}");
        }
    }

    #[test]
    fn other_label_names_are_config_errors_naming_the_label() {
        for label in [
            "",
            "9job",
            "service.name",
            "a-b",
            "job/../x",
            "job?",
            "j\u{f6}b",
        ] {
            let result = validate_label_name(label);

            assert!(
                matches!(&result, Err(Error::Config { message }) if message.contains(&format!("'{label}'"))),
                "{label}: {result:?}"
            );
        }
    }

    #[test]
    fn label_reply_without_data_is_empty() {
        // Loki leaves `data` out when no label or value matches.
        let reply = Reply::ok(json!({"status": "success"}));

        let names = string_array(&reply).unwrap();

        assert!(names.is_empty(), "{names:?}");
    }

    #[test]
    fn label_reply_with_non_string_names_is_a_response_error() {
        let reply = Reply::ok(json!({"status": "success", "data": [1, 2]}));

        let result = string_array(&reply);

        assert!(
            matches!(result, Err(Error::Response { status: 200, .. })),
            "{result:?}"
        );
    }
}
