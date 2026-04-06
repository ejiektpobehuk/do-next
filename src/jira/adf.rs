use serde_json::{json, Value};

/// Best-effort conversion from a Jira rich-text field to markdown.
/// Handles plain strings (v2 API) and ADF JSON documents (v3 API).
pub fn json_to_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(_) => adf_to_markdown(value),
        _ => String::new(),
    }
}

/// Convert an ADF document node to a markdown string.
pub fn adf_to_markdown(node: &Value) -> String {
    let mut out = String::new();
    emit_node(&mut out, node, &Context::default());
    // Remove trailing blank lines, keep single trailing newline
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

#[derive(Default, Clone)]
struct Context {
    /// Current list nesting depth (for indentation).
    list_depth: usize,
    /// Whether the current list context is ordered.
    ordered: bool,
    /// 1-based item counter for ordered lists.
    item_index: usize,
    /// Inside a blockquote.
    in_blockquote: bool,
    /// Inside a table (suppress paragraph double-newlines).
    in_table_cell: bool,
}

fn emit_node(out: &mut String, node: &Value, ctx: &Context) {
    let node_type = node.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match node_type {
        "paragraph" => emit_paragraph(out, node, ctx),
        "heading" => emit_heading(out, node, ctx),
        "text" => emit_text(out, node),
        "hardBreak" => out.push('\n'),
        "bulletList" => emit_bullet_list(out, node, ctx),
        "orderedList" => emit_ordered_list(out, node, ctx),
        "listItem" => emit_list_item(out, node, ctx),
        "codeBlock" => emit_code_block(out, node, ctx),
        "blockquote" => emit_blockquote(out, node, ctx),
        "rule" => emit_rule(out, ctx),
        "table" => emit_table(out, node),
        "tableRow" | "tableHeader" | "tableCell" => { /* handled by emit_table */ }
        _ => emit_children(out, node, ctx),
    }
}

fn emit_children(out: &mut String, node: &Value, ctx: &Context) {
    let Some(content) = node.get("content").and_then(|c| c.as_array()) else {
        return;
    };
    for child in content {
        emit_node(out, child, ctx);
    }
}

fn line_prefix(ctx: &Context) -> String {
    let indent = "  ".repeat(ctx.list_depth.saturating_sub(1));
    if ctx.in_blockquote {
        format!("{indent}> ")
    } else {
        indent
    }
}

fn emit_paragraph(out: &mut String, node: &Value, ctx: &Context) {
    if ctx.in_table_cell {
        emit_inline_children(out, node);
        return;
    }
    // Separate from previous block with a blank line
    if !out.is_empty() && !out.ends_with("\n\n") {
        if out.ends_with('\n') {
            out.push('\n');
        } else {
            out.push_str("\n\n");
        }
    }
    let prefix = line_prefix(ctx);
    if !prefix.is_empty() && ctx.list_depth == 0 {
        out.push_str(&prefix);
    }
    emit_inline_children(out, node);
    out.push('\n');
}

fn emit_inline_children(out: &mut String, node: &Value) {
    let Some(content) = node.get("content").and_then(|c| c.as_array()) else {
        return;
    };
    for child in content {
        let child_type = child.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match child_type {
            "text" => emit_text(out, child),
            "hardBreak" => out.push('\n'),
            _ => {}
        }
    }
}

fn emit_text(out: &mut String, node: &Value) {
    let text = node.get("text").and_then(|t| t.as_str()).unwrap_or("");
    let marks = node.get("marks").and_then(|m| m.as_array());

    let Some(marks) = marks else {
        out.push_str(text);
        return;
    };

    // Collect mark types. Order: link wraps others, then strong, em, code, strike.
    let mut has_strong = false;
    let mut has_em = false;
    let mut has_code = false;
    let mut has_strike = false;
    let mut link_href: Option<&str> = None;

    for mark in marks {
        match mark.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "strong" => has_strong = true,
            "em" => has_em = true,
            "code" => has_code = true,
            "strike" => has_strike = true,
            "link" => {
                link_href = mark
                    .get("attrs")
                    .and_then(|a| a.get("href"))
                    .and_then(|h| h.as_str());
            }
            _ => {}
        }
    }

    if let Some(href) = link_href {
        out.push('[');
        push_marked_text(out, text, has_strong, has_em, has_code, has_strike);
        out.push_str("](");
        out.push_str(href);
        out.push(')');
    } else {
        push_marked_text(out, text, has_strong, has_em, has_code, has_strike);
    }
}

#[allow(clippy::fn_params_excessive_bools)]
fn push_marked_text(out: &mut String, text: &str, strong: bool, em: bool, code: bool, strike: bool) {
    if code {
        out.push('`');
        out.push_str(text);
        out.push('`');
        return;
    }
    if strike {
        out.push_str("~~");
    }
    if strong && em {
        out.push_str("***");
    } else if strong {
        out.push_str("**");
    } else if em {
        out.push('*');
    }
    out.push_str(text);
    if strong && em {
        out.push_str("***");
    } else if strong {
        out.push_str("**");
    } else if em {
        out.push('*');
    }
    if strike {
        out.push_str("~~");
    }
}

fn emit_heading(out: &mut String, node: &Value, ctx: &Context) {
    let level = node
        .get("attrs")
        .and_then(|a| a.get("level"))
        .and_then(serde_json::Value::as_u64)
        .map_or(1, |l| usize::try_from(l).unwrap_or(1))
        .min(6);

    if !out.is_empty() && !out.ends_with("\n\n") {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }

    let prefix = line_prefix(ctx);
    out.push_str(&prefix);
    for _ in 0..level {
        out.push('#');
    }
    out.push(' ');
    emit_inline_children(out, node);
    out.push('\n');
}

fn emit_bullet_list(out: &mut String, node: &Value, ctx: &Context) {
    let Some(items) = node.get("content").and_then(|c| c.as_array()) else {
        return;
    };
    let child_ctx = Context {
        list_depth: ctx.list_depth + 1,
        ordered: false,
        item_index: 0,
        ..*ctx
    };
    for item in items {
        emit_node(out, item, &child_ctx);
    }
}

fn emit_ordered_list(out: &mut String, node: &Value, ctx: &Context) {
    let Some(items) = node.get("content").and_then(|c| c.as_array()) else {
        return;
    };
    for (i, item) in items.iter().enumerate() {
        let child_ctx = Context {
            list_depth: ctx.list_depth + 1,
            ordered: true,
            item_index: i + 1,
            ..*ctx
        };
        emit_node(out, item, &child_ctx);
    }
}

fn emit_list_item(out: &mut String, node: &Value, ctx: &Context) {
    let Some(content) = node.get("content").and_then(|c| c.as_array()) else {
        return;
    };
    let indent = "  ".repeat(ctx.list_depth.saturating_sub(1));
    let marker = if ctx.ordered {
        format!("{}{}. ", indent, ctx.item_index)
    } else {
        format!("{indent}- ")
    };

    for (i, child) in content.iter().enumerate() {
        let child_type = child.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if i == 0 && child_type == "paragraph" {
            // First paragraph: render on same line as marker
            out.push_str(&marker);
            emit_inline_children(out, child);
            out.push('\n');
        } else {
            // Nested content (sub-lists, additional paragraphs)
            emit_node(out, child, ctx);
        }
    }
}

fn emit_code_block(out: &mut String, node: &Value, _ctx: &Context) {
    let lang = node
        .get("attrs")
        .and_then(|a| a.get("language"))
        .and_then(|l| l.as_str())
        .unwrap_or("");
    out.push_str("```");
    out.push_str(lang);
    out.push('\n');
    // Code block content is typically a single text node
    if let Some(content) = node.get("content").and_then(|c| c.as_array()) {
        for child in content {
            if let Some(text) = child.get("text").and_then(|t| t.as_str()) {
                out.push_str(text);
            }
        }
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("```\n");
}

fn emit_blockquote(out: &mut String, node: &Value, ctx: &Context) {
    let child_ctx = Context {
        in_blockquote: true,
        ..*ctx
    };
    let Some(content) = node.get("content").and_then(|c| c.as_array()) else {
        return;
    };
    for child in content {
        let child_type = child.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if child_type == "paragraph" {
            out.push_str("> ");
            emit_inline_children(out, child);
            out.push('\n');
        } else {
            emit_node(out, child, &child_ctx);
        }
    }
}

fn emit_rule(out: &mut String, _ctx: &Context) {
    if !out.is_empty() && !out.ends_with("\n\n") {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str("---\n");
}

fn emit_table(out: &mut String, node: &Value) {
    let Some(rows) = node.get("content").and_then(|c| c.as_array()) else {
        return;
    };
    let cell_ctx = Context {
        in_table_cell: true,
        ..Context::default()
    };

    for (row_idx, row) in rows.iter().enumerate() {
        let Some(cells) = row.get("content").and_then(|c| c.as_array()) else {
            continue;
        };
        out.push('|');
        let mut col_count = 0;
        for cell in cells {
            out.push(' ');
            emit_children(out, cell, &cell_ctx);
            out.push_str(" |");
            col_count += 1;
        }
        out.push('\n');

        // After header row (first row with tableHeader cells), emit separator
        if row_idx == 0 {
            let is_header = cells
                .first()
                .and_then(|c| c.get("type"))
                .and_then(|t| t.as_str())
                == Some("tableHeader");
            if is_header {
                out.push('|');
                for _ in 0..col_count {
                    out.push_str(" --- |");
                }
                out.push('\n');
            }
        }
    }
}

/// Parse a markdown string and produce an ADF document suitable for Jira Cloud v3 API.
#[allow(clippy::too_many_lines)]
pub fn markdown_to_adf(markdown: &str) -> Value {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

    let opts = Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(markdown, opts);

    // Stack of (node_type, attrs, content_vec) for building the tree.
    let mut stack: Vec<(String, Option<Value>, Vec<Value>)> = vec![];
    // Current inline marks (strong, em, etc.)
    let mut marks: Vec<Value> = vec![];

    // Root document
    stack.push(("doc".to_string(), None, vec![]));

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => stack.push(("paragraph".into(), None, vec![])),
                Tag::Heading { level, .. } => {
                    stack.push(("heading".into(), Some(json!({ "level": level as u8 })), vec![]));
                }
                Tag::BlockQuote(_) => stack.push(("blockquote".into(), None, vec![])),
                Tag::CodeBlock(kind) => {
                    let lang = match kind {
                        pulldown_cmark::CodeBlockKind::Fenced(lang) if !lang.is_empty() => {
                            Some(json!({ "language": lang.as_ref() }))
                        }
                        _ => None,
                    };
                    stack.push(("codeBlock".into(), lang, vec![]));
                }
                Tag::List(start) => {
                    let name = if start.is_some() { "orderedList" } else { "bulletList" };
                    stack.push((name.into(), None, vec![]));
                }
                Tag::Item => stack.push(("listItem".into(), None, vec![])),
                Tag::Emphasis => marks.push(json!({ "type": "em" })),
                Tag::Strong => marks.push(json!({ "type": "strong" })),
                Tag::Strikethrough => marks.push(json!({ "type": "strike" })),
                Tag::Link { dest_url, .. } => {
                    marks.push(
                        json!({ "type": "link", "attrs": { "href": dest_url.as_ref() } }),
                    );
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::BlockQuote(_)
                | TagEnd::List(_) => md_pop_container(&mut stack),
                TagEnd::CodeBlock => {
                    // Strip trailing newline that pulldown-cmark adds to code text
                    if let Some(top) = stack.last_mut()
                        && let Some(last_child) = top.2.last_mut()
                            && let Some(text) = last_child.get("text").and_then(|t| t.as_str()) {
                                let trimmed = text.trim_end_matches('\n');
                                if trimmed != text {
                                    last_child["text"] = Value::String(trimmed.to_string());
                                }
                            }
                    md_pop_container(&mut stack);
                }
                TagEnd::Item => {
                    // Tight lists: pulldown-cmark doesn't emit Paragraph inside Item.
                    // ADF requires listItem > paragraph > text, so wrap inline content.
                    if let Some(top) = stack.last_mut() {
                        let has_block_child = top.2.iter().any(|c| {
                            let t = c.get("type").and_then(|t| t.as_str()).unwrap_or("");
                            matches!(
                                t,
                                "paragraph"
                                    | "bulletList"
                                    | "orderedList"
                                    | "codeBlock"
                                    | "blockquote"
                            )
                        });
                        if !has_block_child && !top.2.is_empty() {
                            let inline_nodes = std::mem::take(&mut top.2);
                            let para = json!({
                                "type": "paragraph",
                                "content": inline_nodes,
                            });
                            top.2.push(para);
                        }
                    }
                    md_pop_container(&mut stack);
                }
                TagEnd::Emphasis => {
                    marks.retain(|m| m.get("type").and_then(|t| t.as_str()) != Some("em"));
                }
                TagEnd::Strong => {
                    marks.retain(|m| m.get("type").and_then(|t| t.as_str()) != Some("strong"));
                }
                TagEnd::Strikethrough => {
                    marks.retain(|m| m.get("type").and_then(|t| t.as_str()) != Some("strike"));
                }
                TagEnd::Link => {
                    marks.retain(|m| m.get("type").and_then(|t| t.as_str()) != Some("link"));
                }
                _ => {}
            },
            Event::Text(text) => md_add_text(&mut stack, &marks, text.as_ref()),
            Event::Code(code) => {
                let mut code_marks = marks.clone();
                code_marks.push(json!({ "type": "code" }));
                md_add_text(&mut stack, &code_marks, code.as_ref());
            }
            Event::SoftBreak => md_add_text(&mut stack, &marks, " "),
            Event::HardBreak => {
                if let Some(parent) = stack.last_mut() {
                    parent.2.push(json!({ "type": "hardBreak" }));
                }
            }
            Event::Rule => {
                if let Some(parent) = stack.last_mut() {
                    parent.2.push(json!({ "type": "rule" }));
                }
            }
            _ => {}
        }
    }

    // The root "doc" node should be the only thing left
    let (_, _, content) = stack.pop().unwrap_or_default();
    json!({ "type": "doc", "version": 1, "content": content })
}

fn md_pop_container(stack: &mut Vec<(String, Option<Value>, Vec<Value>)>) {
    let Some((node_type, attrs, content)) = stack.pop() else {
        return;
    };
    let mut node = json!({ "type": node_type });
    if let Some(attrs) = attrs {
        node["attrs"] = attrs;
    }
    if !content.is_empty() {
        node["content"] = Value::Array(content);
    }
    if let Some(parent) = stack.last_mut() {
        parent.2.push(node);
    }
}

fn md_add_text(
    stack: &mut [(String, Option<Value>, Vec<Value>)],
    marks: &[Value],
    text: &str,
) {
    if text.is_empty() {
        return;
    }
    let mut node = json!({ "type": "text", "text": text });
    if !marks.is_empty() {
        node["marks"] = Value::Array(marks.to_vec());
    }
    if let Some(parent) = stack.last_mut() {
        parent.2.push(node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn adf_doc(content: Vec<Value>) -> Value {
        json!({ "type": "doc", "version": 1, "content": content })
    }

    fn adf_paragraph(content: Vec<Value>) -> Value {
        json!({ "type": "paragraph", "content": content })
    }

    fn adf_text(text: &str) -> Value {
        json!({ "type": "text", "text": text })
    }

    fn adf_text_with_marks(text: &str, marks: Vec<Value>) -> Value {
        json!({ "type": "text", "text": text, "marks": marks })
    }

    fn adf_heading(level: u8, content: Vec<Value>) -> Value {
        json!({ "type": "heading", "attrs": { "level": level }, "content": content })
    }

    fn adf_bullet_list(items: Vec<Value>) -> Value {
        json!({ "type": "bulletList", "content": items })
    }

    fn adf_ordered_list(items: Vec<Value>) -> Value {
        json!({ "type": "orderedList", "content": items })
    }

    fn adf_list_item(content: Vec<Value>) -> Value {
        json!({ "type": "listItem", "content": content })
    }

    fn adf_code_block(language: Option<&str>, text: &str) -> Value {
        let mut node = json!({
            "type": "codeBlock",
            "content": [{ "type": "text", "text": text }]
        });
        if let Some(lang) = language {
            node["attrs"] = json!({ "language": lang });
        }
        node
    }

    fn adf_blockquote(content: Vec<Value>) -> Value {
        json!({ "type": "blockquote", "content": content })
    }

    fn adf_rule() -> Value {
        json!({ "type": "rule" })
    }

    fn adf_hard_break() -> Value {
        json!({ "type": "hardBreak" })
    }

    fn adf_table(rows: Vec<Value>) -> Value {
        json!({ "type": "table", "content": rows })
    }

    fn adf_table_row(cells: Vec<Value>) -> Value {
        json!({ "type": "tableRow", "content": cells })
    }

    fn adf_table_header(content: Vec<Value>) -> Value {
        json!({ "type": "tableHeader", "content": content })
    }

    fn adf_table_cell(content: Vec<Value>) -> Value {
        json!({ "type": "tableCell", "content": content })
    }

    // ── ADF → Markdown tests ────────────────────────────────────────────────

    #[test]
    fn adf_to_md_plain_paragraph() {
        let adf = adf_doc(vec![adf_paragraph(vec![adf_text("hello world")])]);
        assert_eq!(adf_to_markdown(&adf), "hello world\n");
    }

    #[test]
    fn adf_to_md_multiple_paragraphs() {
        let adf = adf_doc(vec![
            adf_paragraph(vec![adf_text("first")]),
            adf_paragraph(vec![adf_text("second")]),
        ]);
        assert_eq!(adf_to_markdown(&adf), "first\n\nsecond\n");
    }

    #[test]
    fn adf_to_md_heading() {
        let adf = adf_doc(vec![
            adf_heading(1, vec![adf_text("Title")]),
            adf_heading(3, vec![adf_text("Sub")]),
        ]);
        assert_eq!(adf_to_markdown(&adf), "# Title\n\n### Sub\n");
    }

    #[test]
    fn adf_to_md_bold() {
        let adf = adf_doc(vec![adf_paragraph(vec![adf_text_with_marks(
            "bold",
            vec![json!({ "type": "strong" })],
        )])]);
        assert_eq!(adf_to_markdown(&adf), "**bold**\n");
    }

    #[test]
    fn adf_to_md_italic() {
        let adf = adf_doc(vec![adf_paragraph(vec![adf_text_with_marks(
            "italic",
            vec![json!({ "type": "em" })],
        )])]);
        assert_eq!(adf_to_markdown(&adf), "*italic*\n");
    }

    #[test]
    fn adf_to_md_inline_code() {
        let adf = adf_doc(vec![adf_paragraph(vec![adf_text_with_marks(
            "code",
            vec![json!({ "type": "code" })],
        )])]);
        assert_eq!(adf_to_markdown(&adf), "`code`\n");
    }

    #[test]
    fn adf_to_md_strikethrough() {
        let adf = adf_doc(vec![adf_paragraph(vec![adf_text_with_marks(
            "deleted",
            vec![json!({ "type": "strike" })],
        )])]);
        assert_eq!(adf_to_markdown(&adf), "~~deleted~~\n");
    }

    #[test]
    fn adf_to_md_link() {
        let adf = adf_doc(vec![adf_paragraph(vec![adf_text_with_marks(
            "click here",
            vec![json!({ "type": "link", "attrs": { "href": "https://example.com" } })],
        )])]);
        assert_eq!(adf_to_markdown(&adf), "[click here](https://example.com)\n");
    }

    #[test]
    fn adf_to_md_nested_marks() {
        let adf = adf_doc(vec![adf_paragraph(vec![adf_text_with_marks(
            "important",
            vec![json!({ "type": "strong" }), json!({ "type": "em" })],
        )])]);
        assert_eq!(adf_to_markdown(&adf), "***important***\n");
    }

    #[test]
    fn adf_to_md_mixed_text() {
        let adf = adf_doc(vec![adf_paragraph(vec![
            adf_text("normal "),
            adf_text_with_marks("bold", vec![json!({ "type": "strong" })]),
            adf_text(" normal"),
        ])]);
        assert_eq!(adf_to_markdown(&adf), "normal **bold** normal\n");
    }

    #[test]
    fn adf_to_md_bullet_list() {
        let adf = adf_doc(vec![adf_bullet_list(vec![
            adf_list_item(vec![adf_paragraph(vec![adf_text("one")])]),
            adf_list_item(vec![adf_paragraph(vec![adf_text("two")])]),
        ])]);
        assert_eq!(adf_to_markdown(&adf), "- one\n- two\n");
    }

    #[test]
    fn adf_to_md_ordered_list() {
        let adf = adf_doc(vec![adf_ordered_list(vec![
            adf_list_item(vec![adf_paragraph(vec![adf_text("first")])]),
            adf_list_item(vec![adf_paragraph(vec![adf_text("second")])]),
        ])]);
        assert_eq!(adf_to_markdown(&adf), "1. first\n2. second\n");
    }

    #[test]
    fn adf_to_md_nested_bullet_list() {
        let adf = adf_doc(vec![adf_bullet_list(vec![adf_list_item(vec![
            adf_paragraph(vec![adf_text("parent")]),
            adf_bullet_list(vec![
                adf_list_item(vec![adf_paragraph(vec![adf_text("child")])]),
            ]),
        ])])]);
        assert_eq!(adf_to_markdown(&adf), "- parent\n  - child\n");
    }

    #[test]
    fn adf_to_md_code_block_with_language() {
        let adf = adf_doc(vec![adf_code_block(Some("rust"), "fn main() {}")]);
        assert_eq!(
            adf_to_markdown(&adf),
            "```rust\nfn main() {}\n```\n"
        );
    }

    #[test]
    fn adf_to_md_code_block_no_language() {
        let adf = adf_doc(vec![adf_code_block(None, "some code")]);
        assert_eq!(adf_to_markdown(&adf), "```\nsome code\n```\n");
    }

    #[test]
    fn adf_to_md_blockquote() {
        let adf = adf_doc(vec![adf_blockquote(vec![adf_paragraph(vec![adf_text(
            "quoted text",
        )])])]);
        assert_eq!(adf_to_markdown(&adf), "> quoted text\n");
    }

    #[test]
    fn adf_to_md_hard_break() {
        let adf = adf_doc(vec![adf_paragraph(vec![
            adf_text("before"),
            adf_hard_break(),
            adf_text("after"),
        ])]);
        assert_eq!(adf_to_markdown(&adf), "before\nafter\n");
    }

    #[test]
    fn adf_to_md_rule() {
        let adf = adf_doc(vec![
            adf_paragraph(vec![adf_text("above")]),
            adf_rule(),
            adf_paragraph(vec![adf_text("below")]),
        ]);
        assert_eq!(adf_to_markdown(&adf), "above\n\n---\n\nbelow\n");
    }

    #[test]
    fn adf_to_md_table() {
        let adf = adf_doc(vec![adf_table(vec![
            adf_table_row(vec![
                adf_table_header(vec![adf_paragraph(vec![adf_text("Name")])]),
                adf_table_header(vec![adf_paragraph(vec![adf_text("Value")])]),
            ]),
            adf_table_row(vec![
                adf_table_cell(vec![adf_paragraph(vec![adf_text("foo")])]),
                adf_table_cell(vec![adf_paragraph(vec![adf_text("bar")])]),
            ]),
        ])]);
        assert_eq!(
            adf_to_markdown(&adf),
            "| Name | Value |\n| --- | --- |\n| foo | bar |\n"
        );
    }

    #[test]
    fn adf_to_md_empty_paragraph() {
        let adf = adf_doc(vec![adf_paragraph(vec![])]);
        assert_eq!(adf_to_markdown(&adf), "\n");
    }

    #[test]
    fn adf_to_md_json_to_text_plain_string() {
        let val = Value::String("hello".into());
        assert_eq!(json_to_text(&val), "hello");
    }

    #[test]
    fn adf_to_md_json_to_text_adf_object() {
        let adf = adf_doc(vec![adf_paragraph(vec![adf_text("hello")])]);
        assert_eq!(json_to_text(&adf), "hello\n");
    }

    // ── Markdown → ADF tests ────────────────────────────────────────────────

    #[test]
    fn md_to_adf_plain_paragraph() {
        let result = markdown_to_adf("hello world");
        assert_eq!(
            result,
            adf_doc(vec![adf_paragraph(vec![adf_text("hello world")])])
        );
    }

    #[test]
    fn md_to_adf_multiple_paragraphs() {
        let result = markdown_to_adf("first\n\nsecond");
        assert_eq!(
            result,
            adf_doc(vec![
                adf_paragraph(vec![adf_text("first")]),
                adf_paragraph(vec![adf_text("second")]),
            ])
        );
    }

    #[test]
    fn md_to_adf_heading() {
        let result = markdown_to_adf("# Title");
        assert_eq!(
            result,
            adf_doc(vec![adf_heading(1, vec![adf_text("Title")])])
        );
    }

    #[test]
    fn md_to_adf_bold() {
        let result = markdown_to_adf("**bold**");
        assert_eq!(
            result,
            adf_doc(vec![adf_paragraph(vec![adf_text_with_marks(
                "bold",
                vec![json!({ "type": "strong" })],
            )])])
        );
    }

    #[test]
    fn md_to_adf_italic() {
        let result = markdown_to_adf("*italic*");
        assert_eq!(
            result,
            adf_doc(vec![adf_paragraph(vec![adf_text_with_marks(
                "italic",
                vec![json!({ "type": "em" })],
            )])])
        );
    }

    #[test]
    fn md_to_adf_inline_code() {
        let result = markdown_to_adf("`code`");
        assert_eq!(
            result,
            adf_doc(vec![adf_paragraph(vec![adf_text_with_marks(
                "code",
                vec![json!({ "type": "code" })],
            )])])
        );
    }

    #[test]
    fn md_to_adf_strikethrough() {
        let result = markdown_to_adf("~~deleted~~");
        assert_eq!(
            result,
            adf_doc(vec![adf_paragraph(vec![adf_text_with_marks(
                "deleted",
                vec![json!({ "type": "strike" })],
            )])])
        );
    }

    #[test]
    fn md_to_adf_link() {
        let result = markdown_to_adf("[click here](https://example.com)");
        assert_eq!(
            result,
            adf_doc(vec![adf_paragraph(vec![adf_text_with_marks(
                "click here",
                vec![json!({ "type": "link", "attrs": { "href": "https://example.com" } })],
            )])])
        );
    }

    #[test]
    fn md_to_adf_bullet_list() {
        let result = markdown_to_adf("- one\n- two");
        assert_eq!(
            result,
            adf_doc(vec![adf_bullet_list(vec![
                adf_list_item(vec![adf_paragraph(vec![adf_text("one")])]),
                adf_list_item(vec![adf_paragraph(vec![adf_text("two")])]),
            ])])
        );
    }

    #[test]
    fn md_to_adf_ordered_list() {
        let result = markdown_to_adf("1. first\n2. second");
        assert_eq!(
            result,
            adf_doc(vec![adf_ordered_list(vec![
                adf_list_item(vec![adf_paragraph(vec![adf_text("first")])]),
                adf_list_item(vec![adf_paragraph(vec![adf_text("second")])]),
            ])])
        );
    }

    #[test]
    fn md_to_adf_code_block_with_language() {
        let result = markdown_to_adf("```rust\nfn main() {}\n```");
        assert_eq!(result, adf_doc(vec![adf_code_block(Some("rust"), "fn main() {}")]));
    }

    #[test]
    fn md_to_adf_code_block_no_language() {
        let result = markdown_to_adf("```\nsome code\n```");
        assert_eq!(result, adf_doc(vec![adf_code_block(None, "some code")]));
    }

    #[test]
    fn md_to_adf_blockquote() {
        let result = markdown_to_adf("> quoted text");
        assert_eq!(
            result,
            adf_doc(vec![adf_blockquote(vec![adf_paragraph(vec![adf_text(
                "quoted text",
            )])])])
        );
    }

    #[test]
    fn md_to_adf_rule() {
        let result = markdown_to_adf("---");
        assert_eq!(result, adf_doc(vec![adf_rule()]));
    }

    #[test]
    fn md_to_adf_mixed_inline() {
        let result = markdown_to_adf("normal **bold** normal");
        assert_eq!(
            result,
            adf_doc(vec![adf_paragraph(vec![
                adf_text("normal "),
                adf_text_with_marks("bold", vec![json!({ "type": "strong" })]),
                adf_text(" normal"),
            ])])
        );
    }
}
