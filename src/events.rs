use std::collections::HashMap;

use crate::jira::types::{FieldOption, Issue, Transition};

#[derive(Debug)]
pub enum AppEvent {
    /// Keyboard or mouse event from the terminal.
    Input(crossterm::event::Event),
    /// A background fetch completed successfully.
    SourceLoaded(String, Vec<Issue>),
    /// A whole-source fetch failed (no subsources).
    SourceError(String, anyhow::Error),
    /// One subsource fetch failed; other subsources continue.
    SubsourceError(String, usize, anyhow::Error),
    /// A Jira action (transition, comment, assign, move) completed.
    ActionDone(ActionResult),
    /// Current user resolved (sent once on startup).
    CurrentUserResolved(String),
    /// Spinner animation frame — only sent while sources are loading.
    Tick,
}

#[derive(Debug)]
pub enum ActionResult {
    TransitionApplied {
        issue_key: String,
        new_status: String,
    },
    TransitionsLoaded {
        issue_key: String,
        transitions: Vec<Transition>,
    },
    CommentPosted {
        #[allow(dead_code)]
        issue_key: String,
    },
    AssignedToMe {
        issue_key: String,
    },
    MovedToProject {
        issue_key: String,
        project: String,
    },
    Hidden {
        issue_key: String,
    },
    FieldUpdated {
        issue_key: String,
        field_id: String,
        new_value: serde_json::Value,
    },
    FieldOptionsLoaded {
        issue_key: String,
        field_id: String,
        label: String,
        original_json: serde_json::Value,
        options: Vec<FieldOption>,
        description: Option<String>,
        multi: bool,
    },
    PostmortemFieldNamesLoaded {
        names: HashMap<String, String>,
        /// Jira editmeta `schema.type` per `field_id` (e.g. `"date"`, `"datetime"`).
        schemas: HashMap<String, String>,
    },
    Error(anyhow::Error),
}
