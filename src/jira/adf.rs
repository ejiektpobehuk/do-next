use serde_json::{json, Value};

/// Best-effort plain text extraction from a Jira rich-text field
/// (plain string in v2, ADF JSON document in v3).
pub fn json_to_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(_) => extract_adf_text(value),
        _ => String::new(),
    }
}

fn extract_adf_text(node: &Value) -> String {
    let mut out = String::new();
    if let Some(text) = node.get("text").and_then(|t| t.as_str()) {
        out.push_str(text);
    }
    if let Some(content) = node.get("content").and_then(|c| c.as_array()) {
        for child in content {
            out.push_str(&extract_adf_text(child));
        }
        out.push('\n');
    }
    out
}

/// Wrap plain text into an ADF document suitable for Jira Cloud v3 API.
/// Each line becomes a separate paragraph node.
pub fn plain_text_to_adf(text: &str) -> Value {
    let paragraphs: Vec<Value> = text
        .lines()
        .map(|line| {
            if line.is_empty() {
                json!({ "type": "paragraph", "content": [] })
            } else {
                json!({
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": line }]
                })
            }
        })
        .collect();

    json!({
        "type": "doc",
        "version": 1,
        "content": paragraphs
    })
}
