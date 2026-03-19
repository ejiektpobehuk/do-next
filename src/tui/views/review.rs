use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::jira::types::Issue;
use crate::tui::app::AppState;

pub fn render_review(f: &mut Frame, area: Rect, issue: &Issue, app: &AppState) -> usize {
    let review_cfg = app.config.view_modes.review.as_ref();

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::styled(&issue.key, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::raw(&issue.fields.summary),
    ]));
    lines.push(Line::from(""));

    // Assignee
    let assignee = issue
        .fields
        .assignee
        .as_ref()
        .map_or_else(|| "Unassigned".into(), |a| a.display().to_string());
    lines.push(Line::from(format!("Assignee: {assignee}")));

    // MR links
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Merge Requests",
        Style::default().add_modifier(Modifier::BOLD),
    )));

    let mr_links = extract_mr_links(issue, review_cfg);
    if mr_links.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no MR links found)",
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        for link in &mr_links {
            lines.push(Line::from(format!("  {link}")));
        }
    }

    let total = lines.len();
    let scroll = u16::try_from(app.detail_scroll).unwrap_or(u16::MAX);
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0)),
        area,
    );
    total
}

fn extract_mr_links(
    issue: &Issue,
    cfg: Option<&crate::config::types::ReviewViewConfig>,
) -> Vec<String> {
    let mut links = Vec::new();

    if let Some(cfg) = cfg {
        match cfg.link_method.as_deref() {
            Some("custom_field") => {
                if let Some(field_id) = &cfg.mr_field
                    && let Some(val) = issue.fields.extra.get(field_id)
                    && let Some(s) = val.as_str()
                {
                    links.push(s.to_string());
                }
            }
            _ => {
                // Derive MR URL from branch name (issue key as branch)
                if let Some(base) = &cfg.base_url {
                    // Conventional branch: feature/PROJ-123-description
                    let key_lower = issue.key.to_lowercase();
                    links.push(format!("{base}/merge_requests?search={key_lower}"));
                }
            }
        }
    }

    // Also check issue remote links (if available in extra fields)
    if let Some(remote_links) = issue.fields.extra.get("remoteLinks")
        && let Some(arr) = remote_links.as_array()
    {
        for rl in arr {
            if let Some(url) = rl.pointer("/object/url").and_then(|u| u.as_str()) {
                links.push(url.to_string());
            }
        }
    }

    links
}
