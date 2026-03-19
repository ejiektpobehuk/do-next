use anyhow::{Context, Result, bail};

use crate::jira::JiraClient;

/// `do-next comment [ISSUE_KEY]`
/// If `issue_key` is None, tries to determine the "current" task (not yet implemented).
pub async fn run(client: &JiraClient, issue_key: Option<&str>, no_history: bool) -> Result<()> {
    let key = match issue_key {
        Some(k) => k.to_string(),
        None => bail!("No issue key provided and auto-detection is not yet implemented."),
    };

    // Optionally fetch and display comment history
    if !no_history {
        let issue = client
            .get_issue(&key)
            .await
            .context("Failed to fetch issue")?;
        println!("Comments for {}: {}", key, issue.fields.summary);
        println!("{}", "─".repeat(60));
        if let Some(ref cl) = issue.fields.comment {
            if cl.comments.is_empty() {
                println!("(no comments)");
            }
            for c in &cl.comments {
                let date = &c.created[..10];
                println!("{} · {}", c.author.display(), date);
                for line in c.body.lines() {
                    println!("  {line}");
                }
                println!();
            }
        }
        println!("{}", "─".repeat(60));
    }

    // Open editor
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let tmp = std::env::temp_dir().join(format!("do-next-comment-{}.txt", std::process::id()));

    let status = std::process::Command::new(&editor)
        .arg(&tmp)
        .status()
        .context("Failed to launch editor")?;

    if !status.success() {
        bail!("Editor exited with non-zero status");
    }

    let content = std::fs::read_to_string(&tmp).unwrap_or_default();
    let _ = std::fs::remove_file(&tmp);
    let body = content.trim().to_string();

    if body.is_empty() {
        println!("Empty comment; nothing posted.");
        return Ok(());
    }

    client
        .post_comment(&key, &body)
        .await
        .context("Failed to post comment")?;
    println!("Comment posted to {key}.");
    Ok(())
}
