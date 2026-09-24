use crate::Result;
use crate::client::{GrafanaClient, Reply};
use crate::commands::{CommandOutput, Windowed, resolve_ds};
use crate::datasource::Kind;
use crate::error::Error;
use crate::output::OutputMode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::tempo_secs;
use super::{TagValuesArgs, TagsArgs};

/// The TraceQL-ready names in an `api/v2/search/tags` reply: every tag
/// qualified by its scope (`span.http.method`, `resource.service.name`),
/// except intrinsics, which TraceQL addresses bare (`status`, `duration`).
/// Sorted and de-duplicated for stable output. The v2 API is used because
/// v1 `api/search/tags` returns protobuf text mangled into strings.
fn traceql_tag_names(reply: &Reply) -> Result<Vec<String>> {
    let scopes = reply
        .body
        .get("scopes")
        .and_then(Value::as_array)
        .ok_or_else(|| reply.malformed("no scopes array"))?;
    let mut names = Vec::new();
    for scope in scopes {
        let name = scope
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| reply.malformed("scope without a name"))?;
        let tags = scope
            .get("tags")
            .and_then(Value::as_array)
            .ok_or_else(|| reply.malformed("scope without a tags array"))?;
        for tag in tags {
            let tag = tag
                .as_str()
                .ok_or_else(|| reply.malformed("tag is not a string"))?;
            if name == "intrinsic" {
                names.push(tag.to_string());
            } else {
                names.push(format!("{name}.{tag}"));
            }
        }
    }
    names.sort();
    names.dedup();
    Ok(names)
}

pub async fn tags(client: &GrafanaClient, window: Windowed<'_, TagsArgs>) -> Result<CommandOutput> {
    let Windowed { args, bounds } = window;
    let ds = resolve_ds(client, Kind::Tempo, args.window.datasource.as_deref()).await?;
    let (start, end) = tempo_secs(bounds);
    let mut pairs: Vec<(&str, String)> = Vec::new();
    if let Some(scope) = args.scope {
        pairs.push(("scope", scope.as_str().to_string()));
    }
    pairs.push(("start", start));
    pairs.push(("end", end));
    let reply = client
        .proxy_request(&ds.uid, "api/v2/search/tags", &pairs)
        .await?;
    // Discovery names are query-building input, not rows to scan, so they are
    // a JSON array in every output mode, like logs labels.
    Ok(CommandOutput::Json(Value::Array(
        traceql_tag_names(&reply)?
            .into_iter()
            .map(Value::String)
            .collect(),
    )))
}

// A typed tag value: the type decides TraceQL quoting (strings need quotes,
// ints and durations do not), so it travels with the value.
#[derive(Clone, Debug, Deserialize)]
struct TagValue {
    r#type: String,
    value: String,
}

/// The typed values in an `api/v2/search/tag/{tag}/values` reply, sorted by
/// type then value for stable output whatever the upstream order.
fn typed_tag_values(reply: &Reply) -> Result<Vec<TagValue>> {
    let values = reply
        .body
        .get("tagValues")
        .and_then(Value::as_array)
        .ok_or_else(|| reply.malformed("no tagValues array"))?;
    let mut values: Vec<TagValue> = values
        .iter()
        .cloned()
        .map(|v| {
            TagValue::deserialize(v).map_err(|e| reply.malformed(&format!("tagValues entry: {e}")))
        })
        .collect::<Result<_>>()?;
    values.sort_by(|a, b| (&a.r#type, &a.value).cmp(&(&b.r#type, &b.value)));
    Ok(values)
}

/// Rejects a tag unfit for `api/v2/search/tag/{tag}/values` before it becomes
/// a path segment; see `validate_trace_id` for why. TraceQL tag names are
/// dotted scope-qualified names (`resource.service.name`), bare intrinsics
/// (`status`) and colon event names (`event:name`), so the class is
/// `[A-Za-z0-9_.:-]` — and a bare `.` or `..` is what reqwest resolves.
pub(crate) fn validate_tag(tag: &str) -> Result<()> {
    let valid = !matches!(tag, "" | "." | "..")
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'-' | b'_'));
    if valid {
        return Ok(());
    }
    Err(Error::Config {
        message: format!(
            "invalid tag: '{tag}' (expected letters, digits, '.', ':', '-' or '_', and not '.' or '..')"
        ),
    })
}

pub async fn tag_values(
    client: &GrafanaClient,
    mode: OutputMode,
    window: Windowed<'_, TagValuesArgs>,
) -> Result<CommandOutput> {
    let Windowed { args, bounds } = window;
    let ds = resolve_ds(client, Kind::Tempo, args.window.datasource.as_deref()).await?;
    let (start, end) = tempo_secs(bounds);
    let reply = client
        .proxy_request(
            &ds.uid,
            &format!("api/v2/search/tag/{}/values", args.tag),
            &[("start", start), ("end", end)],
        )
        .await?;
    let values = typed_tag_values(&reply)?;
    if mode == OutputMode::Table {
        let rows = values
            .iter()
            .map(|v| vec![v.r#type.clone(), v.value.clone()])
            .collect();
        return Ok(CommandOutput::Table {
            headers: ["type", "value"].map(String::from).to_vec(),
            rows,
        });
    }
    Ok(CommandOutput::Json(Value::Array(
        values
            .into_iter()
            .map(|v| json!({"type": v.r#type, "value": v.value}))
            .collect(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traceql_shaped_tags_are_valid() {
        for tag in [
            "resource.service.name",
            "span.http.status_code",
            "status",
            "duration",
            "event:exception.type",
            "instrumentation:name",
            "k8s-namespace",
            "tag_with_underscore",
        ] {
            let result = validate_tag(tag);

            assert!(result.is_ok(), "{tag}: {result:?}");
        }
    }

    #[test]
    fn other_tags_are_config_errors_naming_the_tag() {
        for tag in ["", ".", "..", "bad tag", "tag/../other", "tag?", "t\u{e4}g"] {
            let result = validate_tag(tag);

            assert!(
                matches!(&result, Err(Error::Config { message }) if message.contains(&format!("'{tag}'"))),
                "{tag}: {result:?}"
            );
        }
    }

    #[test]
    fn tag_names_flatten_sort_and_dedup() {
        // Scopes out of alphabetical order, one intrinsic tag duplicated.
        let reply = Reply::ok(json!({"scopes": [
            {"name": "span", "tags": ["http.method", "db.name"]},
            {"name": "intrinsic", "tags": ["status", "duration", "name", "status"]},
            {"name": "resource", "tags": ["service.name"]}
        ]}));

        let names = traceql_tag_names(&reply).unwrap();

        assert_eq!(
            names,
            [
                "duration",
                "name",
                "resource.service.name",
                "span.db.name",
                "span.http.method",
                "status"
            ]
        );
    }

    #[test]
    fn tags_reply_of_the_wrong_shape_is_a_response_error() {
        for body in [
            json!({"tagNames": []}),
            json!({"scopes": [{"tags": []}]}),
            json!({"scopes": [{"name": "span"}]}),
            json!({"scopes": [{"name": "span", "tags": [1]}]}),
        ] {
            let result = traceql_tag_names(&Reply::ok(body));

            assert!(
                matches!(result, Err(Error::Response { status: 200, .. })),
                "{result:?}"
            );
        }
    }

    #[test]
    fn tag_values_sort_by_type_then_value() {
        let reply = Reply::ok(json!({"tagValues": [
            {"type": "string", "value": "checkout"},
            {"type": "int", "value": "200"},
            {"type": "string", "value": "cart"}
        ]}));

        let values = typed_tag_values(&reply).unwrap();

        let pairs: Vec<(&str, &str)> = values
            .iter()
            .map(|v| (v.r#type.as_str(), v.value.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [("int", "200"), ("string", "cart"), ("string", "checkout")]
        );
    }

    #[test]
    fn tag_values_reply_of_the_wrong_shape_is_a_response_error() {
        for body in [
            json!({"values": []}),
            json!({"tagValues": [{"type": "string"}]}),
            json!({"tagValues": [{"value": "x"}]}),
            json!({"tagValues": ["x"]}),
        ] {
            let result = typed_tag_values(&Reply::ok(body));

            assert!(
                matches!(result, Err(Error::Response { status: 200, .. })),
                "{result:?}"
            );
        }
    }
}
