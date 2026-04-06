use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Convert a markdown string into styled ratatui lines for terminal display.
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
pub fn markdown_to_lines(md: &str) -> Vec<Line<'static>> {
    let opts = Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(md, opts);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();

    // Style stack: each block/inline start pushes a style, end pops it.
    let mut style_stack: Vec<Style> = Vec::new();

    // Block-level state
    let mut blockquote_depth: usize = 0;
    let mut list_stack: Vec<ListCtx> = Vec::new(); // nested list tracking
    let mut in_code_block = false;
    let mut need_block_gap = false; // insert blank line between blocks
    let mut link_url: Option<String> = None;

    for event in parser {
        match event {
            // ── Block starts ────────────────────────────────────────────
            Event::Start(Tag::Heading { .. }) => {
                if need_block_gap {
                    lines.push(Line::from(""));
                }
                style_stack.push(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                );
            }
            Event::Start(Tag::Paragraph) => {
                if need_block_gap && !in_list_tight_context(&list_stack) {
                    lines.push(Line::from(""));
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                if need_block_gap {
                    lines.push(Line::from(""));
                }
                blockquote_depth += 1;
                style_stack.push(Style::default().fg(Color::DarkGray));
            }
            Event::Start(Tag::CodeBlock(_)) => {
                if need_block_gap {
                    lines.push(Line::from(""));
                }
                in_code_block = true;
                style_stack.push(Style::default().fg(Color::Yellow));
            }
            Event::Start(Tag::List(start)) => {
                if need_block_gap && list_stack.is_empty() {
                    lines.push(Line::from(""));
                }
                // Flush parent item content before starting nested list
                if !current_spans.is_empty() {
                    flush_line(
                        &mut lines,
                        &mut current_spans,
                        blockquote_depth,
                        &list_stack,
                        false,
                    );
                }
                list_stack.push(ListCtx {
                    ordered_start: start,
                    item_index: 0,
                });
            }
            Event::Start(Tag::Item) => {
                // Increment item counter in the current list
                if let Some(ctx) = list_stack.last_mut() {
                    ctx.item_index += 1;
                }
            }

            // ── Inline starts ───────────────────────────────────────────
            Event::Start(Tag::Strong) => {
                style_stack.push(Style::default().add_modifier(Modifier::BOLD));
            }
            Event::Start(Tag::Emphasis) => {
                style_stack.push(Style::default().add_modifier(Modifier::ITALIC));
            }
            Event::Start(Tag::Strikethrough) => {
                style_stack.push(Style::default().add_modifier(Modifier::CROSSED_OUT));
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                link_url = Some(dest_url.to_string());
                style_stack.push(Style::default().add_modifier(Modifier::UNDERLINED));
            }

            // ── Text content ────────────────────────────────────────────
            Event::Text(text) => {
                let style = effective_style(&style_stack);
                if in_code_block {
                    // Code blocks: each source line becomes a separate Line
                    let text_str = text.to_string();
                    for (i, code_line) in text_str.split('\n').enumerate() {
                        if i > 0 {
                            flush_line(
                                &mut lines,
                                &mut current_spans,
                                blockquote_depth,
                                &list_stack,
                                false,
                            );
                        }
                        if !code_line.is_empty() {
                            current_spans
                                .push(Span::styled(format!("  {code_line}"), style));
                        }
                    }
                } else {
                    // Prepend list bullet/number on first text in an item
                    maybe_prepend_list_marker(&mut current_spans, &list_stack);
                    current_spans.push(Span::styled(text.to_string(), style));
                }
            }
            Event::Code(text) => {
                // Inline code — emit with code color, not affected by style stack
                maybe_prepend_list_marker(&mut current_spans, &list_stack);
                current_spans.push(Span::styled(
                    text.to_string(),
                    Style::default().fg(Color::Yellow),
                ));
            }

            // ── Breaks ──────────────────────────────────────────────────
            Event::SoftBreak => {
                // Within a paragraph, soft break = space (ratatui wraps)
                current_spans.push(Span::raw(" "));
            }
            Event::HardBreak => {
                flush_line(
                    &mut lines,
                    &mut current_spans,
                    blockquote_depth,
                    &list_stack,
                    false,
                );
            }

            // ── Block ends ──────────────────────────────────────────────
            Event::End(TagEnd::Heading(_)) => {
                flush_line(
                    &mut lines,
                    &mut current_spans,
                    blockquote_depth,
                    &list_stack,
                    false,
                );
                style_stack.pop();
                need_block_gap = true;
            }
            Event::End(TagEnd::Paragraph) => {
                flush_line(
                    &mut lines,
                    &mut current_spans,
                    blockquote_depth,
                    &list_stack,
                    false,
                );
                need_block_gap = true;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                if !current_spans.is_empty() {
                    flush_line(
                        &mut lines,
                        &mut current_spans,
                        blockquote_depth,
                        &list_stack,
                        false,
                    );
                }
                blockquote_depth = blockquote_depth.saturating_sub(1);
                style_stack.pop();
                need_block_gap = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                // Remove trailing empty spans/line from code block
                if current_spans.is_empty() {
                    // pulldown-cmark adds trailing \n which produces an empty span set
                } else {
                    flush_line(
                        &mut lines,
                        &mut current_spans,
                        blockquote_depth,
                        &list_stack,
                        false,
                    );
                }
                in_code_block = false;
                style_stack.pop();
                need_block_gap = true;
            }
            Event::End(TagEnd::List(_)) => {
                list_stack.pop();
                need_block_gap = true;
            }
            Event::End(TagEnd::Item) => {
                flush_line(
                    &mut lines,
                    &mut current_spans,
                    blockquote_depth,
                    &list_stack,
                    false,
                );
            }

            // ── Inline ends ─────────────────────────────────────────────
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough) => {
                style_stack.pop();
            }
            Event::End(TagEnd::Link) => {
                style_stack.pop();
                // Append dimmed URL after the link text
                if let Some(url) = link_url.take() {
                    current_spans.push(Span::styled(
                        format!(" ({url})"),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    ));
                }
            }

            // ── Other ───────────────────────────────────────────────────
            Event::Rule => {
                if need_block_gap {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(Span::styled(
                    "──────────────────────────────────────────",
                    Style::default().add_modifier(Modifier::DIM),
                )));
                need_block_gap = true;
            }

            _ => {}
        }
    }

    // Flush any remaining spans
    if !current_spans.is_empty() {
        flush_line(
            &mut lines,
            &mut current_spans,
            blockquote_depth,
            &list_stack,
            false,
        );
    }

    lines
}

// ── Helpers ─────────────────────────────────────────────────────────────────

struct ListCtx {
    /// `Some(start)` for ordered lists, `None` for unordered.
    ordered_start: Option<u64>,
    /// 1-based item counter within this list.
    item_index: usize,
}

fn effective_style(stack: &[Style]) -> Style {
    let mut s = Style::default();
    for layer in stack {
        s = s.patch(*layer);
    }
    s
}

const fn in_list_tight_context(list_stack: &[ListCtx]) -> bool {
    !list_stack.is_empty()
}

/// Prepend bullet/number marker if we're at the start of a list item line.
fn maybe_prepend_list_marker(spans: &mut Vec<Span<'static>>, list_stack: &[ListCtx]) {
    // Only prepend if this is the first span for this line
    if !spans.is_empty() {
        return;
    }
    let Some(ctx) = list_stack.last() else {
        return;
    };
    let depth = list_stack.len().saturating_sub(1);
    let indent = "  ".repeat(depth);
    let marker = ctx.ordered_start.map_or_else(
        || format!("{indent}• "),
        |start| {
            #[allow(clippy::cast_possible_truncation)]
            let num = start as usize + ctx.item_index - 1;
            format!("{indent}{num}. ")
        },
    );
    spans.push(Span::styled(
        marker,
        Style::default().add_modifier(Modifier::DIM),
    ));
}

fn flush_line(
    lines: &mut Vec<Line<'static>>,
    spans: &mut Vec<Span<'static>>,
    blockquote_depth: usize,
    _list_stack: &[ListCtx],
    _is_code: bool,
) {
    let mut final_spans: Vec<Span<'static>> = Vec::new();

    // Blockquote prefix
    if blockquote_depth > 0 {
        let prefix = "│ ".repeat(blockquote_depth);
        final_spans.push(Span::styled(
            prefix,
            Style::default().fg(Color::DarkGray),
        ));
    }

    final_spans.append(spans);
    lines.push(Line::from(final_spans));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{
        style::{Color, Modifier, Style},
        text::{Line, Span},
    };

    // ── Inline formatting ───────────────────────────────────────────────────

    #[test]
    fn test_bold() {
        let lines = markdown_to_lines("**bold**");
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].spans,
            vec![Span::styled(
                "bold",
                Style::default().add_modifier(Modifier::BOLD)
            )]
        );
    }

    #[test]
    fn test_italic() {
        let lines = markdown_to_lines("*italic*");
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].spans,
            vec![Span::styled(
                "italic",
                Style::default().add_modifier(Modifier::ITALIC)
            )]
        );
    }

    #[test]
    fn test_inline_code() {
        let lines = markdown_to_lines("`code`");
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].spans,
            vec![Span::styled("code", Style::default().fg(Color::Yellow))]
        );
    }

    #[test]
    fn test_strikethrough() {
        let lines = markdown_to_lines("~~strike~~");
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].spans,
            vec![Span::styled(
                "strike",
                Style::default().add_modifier(Modifier::CROSSED_OUT)
            )]
        );
    }

    #[test]
    fn test_nested_bold_italic() {
        let lines = markdown_to_lines("***both***");
        assert_eq!(lines.len(), 1);
        // pulldown-cmark emits Emphasis wrapping Strong (or vice versa)
        let span = &lines[0].spans[0];
        let style = span.style;
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(span.content, "both");
    }

    #[test]
    fn test_link() {
        let lines = markdown_to_lines("[click](https://example.com)");
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines[0].spans[0],
            Span::styled(
                "click",
                Style::default().add_modifier(Modifier::UNDERLINED)
            )
        );
        assert_eq!(
            lines[0].spans[1],
            Span::styled(
                " (https://example.com)",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM)
            )
        );
    }

    // ── Block-level elements ────────────────────────────────────────────────

    #[test]
    fn test_heading() {
        let lines = markdown_to_lines("# Title");
        assert_eq!(lines.len(), 1);
        let span = &lines[0].spans[0];
        assert_eq!(span.content, "Title");
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(span.style.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_blockquote() {
        let lines = markdown_to_lines("> quoted text");
        assert_eq!(lines.len(), 1);
        // First span is the blockquote prefix
        assert_eq!(
            lines[0].spans[0],
            Span::styled("│ ", Style::default().fg(Color::DarkGray))
        );
        // Text has the DarkGray style from blockquote context
        let text_span = &lines[0].spans[1];
        assert_eq!(text_span.content, "quoted text");
        assert_eq!(text_span.style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_code_block() {
        let lines = markdown_to_lines("```\nhello\nworld\n```");
        assert!(lines.len() >= 2);
        // Code lines should be indented and yellow
        let first_code = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("hello")))
            .expect("should have hello line");
        let span = &first_code.spans[0];
        assert!(span.content.contains("hello"));
        assert_eq!(span.style.fg, Some(Color::Yellow));
    }

    #[test]
    fn test_horizontal_rule() {
        let lines = markdown_to_lines("---");
        assert_eq!(lines.len(), 1);
        let span = &lines[0].spans[0];
        assert!(span.content.contains('─'));
        assert!(span.style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn test_unordered_list() {
        let lines = markdown_to_lines("- first\n- second");
        assert!(lines.len() >= 2);
        // First item should start with bullet marker
        let first = &lines[0];
        assert!(first.spans[0].content.contains('•'));
        assert_eq!(first.spans[1].content, "first");
    }

    #[test]
    fn test_ordered_list() {
        let lines = markdown_to_lines("1. first\n2. second");
        assert!(lines.len() >= 2);
        let first = &lines[0];
        assert!(first.spans[0].content.contains("1."));
        assert_eq!(first.spans[1].content, "first");
    }

    // ── Composition and edge cases ──────────────────────────────────────────

    #[test]
    fn test_empty_input() {
        let lines = markdown_to_lines("");
        assert!(lines.is_empty());
    }

    #[test]
    fn test_plain_text() {
        let lines = markdown_to_lines("hello world");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans, vec![Span::raw("hello world")]);
    }

    #[test]
    fn test_nested_list() {
        let lines = markdown_to_lines("- outer\n  - inner");
        assert!(lines.len() >= 2);
        // Inner item should have increased indent
        let inner = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("inner")))
            .expect("should have inner line");
        // The marker span should contain indent
        let marker = &inner.spans[0];
        assert!(marker.content.starts_with("  ")); // 2-space indent for depth 1
    }

    #[test]
    fn test_mixed_blocks() {
        let lines = markdown_to_lines("# Heading\n\nSome text\n\n- item");
        // Should have heading, gap, paragraph, gap, list item
        assert!(lines.len() >= 3);
        // First line is the heading
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Cyan));
    }
}
