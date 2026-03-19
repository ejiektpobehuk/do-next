use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::jira::JiraClient;

/// `do-next fields ISSUE_KEY [--field FIELD_ID] [--raw]`
///
/// Prints all fields on the given issue alongside their human-readable names.
/// Use `--field` to dump the raw value of one specific field.
/// Add `--raw` to dump the raw editmeta JSON object for that field instead.
pub async fn run(
    client: &JiraClient,
    issue_key: &str,
    field: Option<&str>,
    raw: bool,
) -> Result<()> {
    // --field --raw: dump the raw editmeta JSON object for this field.
    if let (Some(fid), true) = (field, raw) {
        let value = client
            .get_editmeta_field_raw(issue_key, fid)
            .await
            .context("Failed to fetch editmeta")?;
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    // Fetch field definitions and the full issue in parallel.
    let (defs_res, issue_res) = tokio::join!(
        client.get_all_fields(),
        client.get_issue_all_fields(issue_key),
    );

    let defs = defs_res.context("Failed to fetch field definitions")?;
    let issue = issue_res.context("Failed to fetch issue")?;

    let name_map: HashMap<String, String> = defs.into_iter().map(|f| (f.id, f.name)).collect();

    let fields = issue
        .get("fields")
        .and_then(|f| f.as_object())
        .ok_or_else(|| anyhow::anyhow!("No 'fields' object in Jira response"))?;

    // --field: dump raw JSON for one field and exit.
    if let Some(fid) = field {
        match fields.get(fid) {
            Some(v) => {
                println!("{}", serde_json::to_string_pretty(v)?);
                return Ok(());
            }
            None => anyhow::bail!("Field '{fid}' not present on issue {issue_key}"),
        }
    }

    // Full table output.
    let col_id = 30usize;
    let col_name = 30usize;
    println!(
        "{:<col_id$}  {:<col_name$}  Value",
        "Field ID", "Field Name"
    );
    println!("{}", "─".repeat(col_id + 2 + col_name + 2 + 50));

    let mut rows: Vec<(&String, &serde_json::Value)> = fields.iter().collect();
    // System fields first, then custom fields (customfield_NNNNN) alphabetically.
    rows.sort_by_key(|(k, _)| {
        let is_custom = k.starts_with("customfield_");
        (is_custom, k.as_str())
    });

    for (id, value) in rows {
        if value.is_null() {
            continue; // skip empty fields to reduce noise
        }
        let name = name_map
            .get(id.as_str())
            .map_or("—", std::string::String::as_str);
        let display = format_value(value);
        println!("{id:<col_id$}  {name:<col_name$}  {display}");
    }

    println!();
    println!("Tip: use --field FIELD_ID to see the full raw value of a specific field.");

    Ok(())
}

fn format_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "(null)".into(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => truncate(&s.replace('\n', "↵"), 80),
        serde_json::Value::Array(a) => {
            if a.is_empty() {
                return "(empty list)".into();
            }
            // Try to render as a compact list of names/values.
            let items: Vec<String> = a
                .iter()
                .take(5)
                .map(|item| {
                    item.get("name")
                        .or_else(|| item.get("value"))
                        .or_else(|| item.get("displayName"))
                        .and_then(|n| n.as_str())
                        .map_or_else(
                            || truncate(&item.to_string(), 20),
                            std::string::ToString::to_string,
                        )
                })
                .collect();
            let suffix = if a.len() > 5 {
                format!(", … ({} total)", a.len())
            } else {
                String::new()
            };
            format!("[{}{}]", items.join(", "), suffix)
        }
        serde_json::Value::Object(_) => {
            // Try common single-value patterns before falling back to raw JSON.
            for key in &["name", "value", "displayName", "key"] {
                if let Some(s) = v.get(key).and_then(|n| n.as_str()) {
                    return format!("{{{key}: {s}}}");
                }
            }
            truncate(&v.to_string(), 80)
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    // Truncate at a char boundary to avoid panics on multibyte chars.
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let end = s.char_indices().nth(max).map_or(s.len(), |(i, _)| i);
        format!("{}…", &s[..end])
    }
}
