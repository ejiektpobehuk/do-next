use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use indexmap::IndexMap;

use crate::config::types::{ResolvedTeam, SourceConfig, TeamConfig};
use crate::events::{ActionResult, AppEvent};
use crate::jira::types::{Comment, FieldOption, Issue};

/// Per-team state that is saved/restored when switching tabs.
#[derive(Debug, Clone)]
pub struct PerTeamState {
    pub sources: IndexMap<String, SourceState>,
    pub issues: Vec<Issue>,
    pub subsource_errors: IndexMap<String, Vec<(usize, Arc<anyhow::Error>)>>,
    pub nav_items: Vec<NavItem>,
    pub nav_idx: usize,
    pub field_names: HashMap<String, String>,
    pub field_schemas: HashMap<String, String>,
    pub field_names_state: FieldNamesState,
}

/// A navigable item in the list (issue or error row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavItem {
    Issue(usize),
    /// Whole-source fetch failure (source has no subsources).
    SourceError(String),
    /// Single subsource fetch failure (`source_id`, `subsource_idx`).
    SubsourceError(String, usize),
}

/// Loading state for a single issue source.
#[derive(Debug, Clone)]
pub enum SourceState {
    Pending,
    Loading,
    Loaded(Vec<Issue>),
    Error(Arc<anyhow::Error>),
}

impl SourceState {}

/// Which panel has keyboard focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusedPanel {
    List,
    Detail,
}

/// Which view mode to use for the detail panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewMode {
    /// Auto-generated view using all issue fields.
    Default,
    /// Named custom view defined in `config.views`.
    Custom(String),
    Comments,
    Attachments,
}

/// Which item has focus inside the detail view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetailFocus {
    Comments,
    Attachments,
    /// 0-based field index.
    Field(usize),
}

/// A sub-view shown as a popup overlay on top of the detail view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubView {
    Comments,
    Attachments,
}

/// A template that has been read from disk and is ready to use.
#[derive(Debug, Clone)]
pub struct LoadedTemplate {
    pub name: String,
    pub content: String,
}

/// Current overlay / action being performed.
#[derive(Debug, Clone)]
pub enum ActionState {
    None,
    SelectingTransition {
        issue_key: String,
        transitions: Vec<crate::jira::types::Transition>,
        selected: usize,
    },
    LoadingTransitions {
        issue_key: String,
    },
    /// Waiting for an async operation; description is display-only (never a signal).
    AwaitingAction {
        description: String,
    },
    HidePopup {
        issue_key: String,
        selected_solution: usize,
    },
    PendingTransition {
        issue_key: String,
        transition_id: String,
    },
    PendingHide {
        issue_key: String,
    },
    PendingAssign {
        issue_key: String,
    },
    PendingMove {
        issue_key: String,
    },
    PendingComment {
        issue_key: String,
    },
    PendingFieldEdit {
        issue_key: String,
        field_id: String,
        current_value: String,
        /// Original JSON value of the field (used to determine PUT value shape).
        original_json: serde_json::Value,
    },
    /// Offering to use a template for an empty field; `previewing` toggles between
    /// a small dialog and a full markdown preview.
    OfferingTemplate {
        issue_key: String,
        field_id: String,
        templates: Vec<LoadedTemplate>,
        cursor: usize,
        original_json: serde_json::Value,
        previewing: bool,
        scroll: u16,
    },
    /// User is typing directly in the field widget (single-line string fields).
    InlineEditingField {
        issue_key: String,
        field_id: String,
        field_idx: usize,
        input: String,
        cursor: usize, // char index
    },
    /// Fetching allowedValues from Jira before showing a select popup.
    LoadingFieldOptions {
        issue_key: String,
        field_id: String,
        label: String,
        original_json: serde_json::Value,
        description: Option<String>,
        multi: bool,
    },
    /// Single-select popup for select fields.
    SelectingFieldOption {
        issue_key: String,
        field_id: String,
        label: String,
        options: Vec<FieldOption>,
        description: Option<String>,
        cursor: usize,
    },
    /// Multi-select popup for array fields.
    SelectingFieldOptions {
        issue_key: String,
        field_id: String,
        label: String,
        original_json: serde_json::Value,
        options: Vec<FieldOption>,
        description: Option<String>,
        selected: HashSet<usize>,
        cursor: usize,
    },
    /// Field update ready to be dispatched (value already shaped).
    CommittingFieldEdit {
        issue_key: String,
        field_id: String,
        new_value: serde_json::Value,
    },
    /// Showing a diff preview; waiting for user to confirm or cancel.
    ConfirmingFieldEdit {
        issue_key: String,
        field_id: String,
        old_text: String,
        new_text: String,
        new_value: serde_json::Value,
        /// Active tab: 0 = Preview, 1 = Diff
        tab: usize,
    },
    /// Waiting for $EDITOR to close with edited comment body.
    PendingCommentEdit {
        issue_key: String,
        comment_id: String,
        original_body: String,
    },
    /// Showing a diff/preview for the edited comment; waiting for confirm or cancel.
    ConfirmingCommentEdit {
        issue_key: String,
        comment_id: String,
        old_text: String,
        new_text: String,
        /// Active tab: 0 = Preview, 1 = Diff
        tab: usize,
    },
    /// Sending updated comment to Jira.
    CommittingCommentEdit {
        issue_key: String,
        comment_id: String,
        new_body: String,
    },
    /// Yes/No popup confirming comment deletion. `selected` 0=Yes 1=No.
    ConfirmingCommentDelete {
        issue_key: String,
        comment_id: String,
        /// Default 1 (No) for safety.
        selected: usize,
    },
    /// Sending delete to Jira.
    DeletingComment {
        issue_key: String,
        comment_id: String,
    },
    /// Yes/No popup confirming attachment deletion. `selected` 0=Yes 1=No.
    ConfirmingAttachmentDelete {
        issue_key: String,
        attachment_id: String,
        /// Default 1 (No) for safety.
        selected: usize,
    },
    /// Sending attachment delete to Jira.
    DeletingAttachment {
        issue_key: String,
        attachment_id: String,
    },
    /// User is typing a file path to upload as a new attachment.
    TypingAttachmentPath {
        issue_key: String,
        path: String,
        cursor: usize,
        completions: Vec<String>,
        completion_idx: Option<usize>,
        completion_generation: u64,
    },
    /// File path confirmed; ready to upload.
    PendingAttachmentUpload {
        issue_key: String,
        file_path: String,
    },
    /// Fetching and caching an attachment, then opening with system default app.
    OpeningAttachment {
        attachment_id: String,
        content_url: String,
        filename: String,
        issue_key: String,
    },
    /// Interactive datetime picker overlay.
    EditingDatetimeField {
        issue_key: String,
        field_id: String,
        label: String,
        description: Option<String>,
        picker: crate::tui::overlays::datetime_picker::DatetimePicker,
    },
    Error(Arc<anyhow::Error>),
    /// Keybindings reference overlay.
    KeybindingsHelp,
}

/// Progress of field name fetching from Jira API.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum FieldNamesState {
    #[default]
    Idle,
    Loading,
    AllLoaded,
}

/// Miscellaneous app flags grouped to keep `AppState` bool count low.
#[derive(Debug, Clone, Default)]
pub struct AppFlags {
    pub field_names: FieldNamesState,
    /// Tracks first `g` press for `gg` (jump to first) motion.
    pub pending_g: bool,
    /// Set when a tab switch requires fetching sources for the new team.
    pub pending_team_fetch: bool,
}

pub struct AppState {
    pub resolved_teams: Vec<ResolvedTeam>,
    pub active_team_idx: usize,
    /// Saved per-team state for inactive tabs.
    pub saved_team_states: HashMap<usize, PerTeamState>,
    pub sources: IndexMap<String, SourceState>,
    /// Flat ordered list of all visible issues (after dedup).
    pub issues: Vec<Issue>,
    /// Per-source subsource errors: `source_id` → [(`subsource_idx`, error)].
    pub subsource_errors: IndexMap<String, Vec<(usize, Arc<anyhow::Error>)>>,
    /// Ordered navigable items: issues and error rows.
    pub nav_items: Vec<NavItem>,
    /// Index into `nav_items` for the currently selected item.
    pub nav_idx: usize,
    pub view_mode: ViewMode,
    pub action_state: ActionState,
    pub should_quit: bool,
    pub flags: AppFlags,
    /// Spinner frame counter (incremented on each Tick).
    pub tick_count: u64,
    pub current_user: Option<String>,
    /// Scroll offset for the detail panel (rows).
    pub detail_scroll: usize,
    /// Which panel currently has keyboard focus.
    pub focused_panel: FocusedPanel,
    /// Focused item when in a detail view (`Default` or `Custom`) with `FocusedPanel::Detail`.
    pub detail_focus: DetailFocus,
    /// Virtual (top, bottom) row for each focusable detail item; written each render.
    /// Index: Comments=0, Attachments=1, Field(i)=2+i.
    pub detail_focus_offsets: Vec<(usize, usize)>,
    /// Height of the detail content viewport; written each render.
    pub last_detail_viewport_h: usize,
    /// API-fetched display names for fields: `field_id` → name.
    pub field_names: HashMap<String, String>,
    /// API-fetched Jira schema types for fields: `field_id` → type string.
    pub field_schemas: HashMap<String, String>,
    /// Total content lines of the detail view; written each render.
    pub last_detail_content_h: usize,
    /// Sub-view popup shown on top of the detail view (Comments or Attachments).
    pub overlay: Option<SubView>,
    /// Scroll offset for the sub-view overlay (independent of `detail_scroll`).
    pub overlay_scroll: usize,
    /// Content height (lines) of the sub-view overlay; written each render.
    pub overlay_content_h: usize,
    /// Viewport height of the sub-view overlay; written each render.
    pub overlay_viewport_h: usize,
    /// Index of the focused comment widget in the comments overlay.
    pub overlay_focused_comment: usize,
    /// Virtual (top, bottom) row for each comment widget; written each render.
    pub overlay_comment_offsets: Vec<(usize, usize)>,
    /// Index of the focused attachment in the attachments overlay.
    pub overlay_focused_attachment: usize,
    /// Cached file paths per `attachment_id`.
    pub attachment_cache: HashMap<String, std::path::PathBuf>,
    /// Decoded text content per `attachment_id`.
    pub attachment_text_previews: HashMap<String, String>,
    /// Decoded image protocol state per `attachment_id` (for ratatui-image).
    pub attachment_images:
        HashMap<String, std::cell::RefCell<ratatui_image::protocol::StatefulProtocol>>,
    /// Attachment currently being fetched in the background (id).
    pub attachment_fetching_id: Option<String>,
    /// Update warnings from git-based checks (shown at startup, dismissed on any key).
    pub update_warnings: Vec<String>,
    /// Pending silent background fetch (set by nav handlers, consumed by `dispatch_action`).
    pub pending_attachment_fetch: Option<AttachmentFetchRequest>,
    /// Pending path completion fetch generation (set by key handler, consumed by `dispatch_action`).
    pub pending_completion_fetch: Option<u64>,
    /// Terminal image protocol picker (created once at startup).
    pub image_picker: Option<ratatui_image::picker::Picker>,
}

/// Request for a silent background attachment fetch.
pub struct AttachmentFetchRequest {
    pub attachment_id: String,
    pub content_url: String,
    pub filename: String,
    pub issue_key: String,
}

impl AppState {
    pub fn new(resolved_teams: Vec<ResolvedTeam>) -> Self {
        // Build source state from the first (active) team's sources.
        let sources = resolved_teams
            .first()
            .map(|t| {
                t.config
                    .sources
                    .iter()
                    .map(|s| (s.id.clone(), SourceState::Pending))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            resolved_teams,
            active_team_idx: 0,
            saved_team_states: HashMap::new(),
            sources,
            issues: Vec::new(),
            subsource_errors: IndexMap::new(),
            nav_items: Vec::new(),
            nav_idx: 0,
            view_mode: ViewMode::Default,
            action_state: ActionState::None,
            should_quit: false,
            flags: AppFlags::default(),
            tick_count: 0,
            current_user: None,
            detail_scroll: 0,
            focused_panel: FocusedPanel::List,
            detail_focus: DetailFocus::Comments,
            detail_focus_offsets: Vec::new(),
            last_detail_viewport_h: 0,
            field_names: HashMap::new(),
            field_schemas: HashMap::new(),
            last_detail_content_h: 0,
            overlay: None,
            overlay_scroll: 0,
            overlay_content_h: 0,
            overlay_viewport_h: 0,
            overlay_focused_comment: 0,
            overlay_comment_offsets: Vec::new(),
            overlay_focused_attachment: 0,
            attachment_cache: HashMap::new(),
            attachment_text_previews: HashMap::new(),
            attachment_images: HashMap::new(),
            attachment_fetching_id: None,
            update_warnings: Vec::new(),
            pending_attachment_fetch: None,
            pending_completion_fetch: None,
            image_picker: None,
        }
    }

    /// Switch to a different team tab.
    pub fn switch_team(&mut self, new_idx: usize) {
        if new_idx == self.active_team_idx || new_idx >= self.resolved_teams.len() {
            return;
        }
        // Save current team state
        let current_state = PerTeamState {
            sources: std::mem::take(&mut self.sources),
            issues: std::mem::take(&mut self.issues),
            subsource_errors: std::mem::take(&mut self.subsource_errors),
            nav_items: std::mem::take(&mut self.nav_items),
            nav_idx: self.nav_idx,
            field_names: std::mem::take(&mut self.field_names),
            field_schemas: std::mem::take(&mut self.field_schemas),
            field_names_state: self.flags.field_names.clone(),
        };
        self.saved_team_states
            .insert(self.active_team_idx, current_state);

        // Restore new team state
        self.active_team_idx = new_idx;
        if let Some(saved) = self.saved_team_states.remove(&new_idx) {
            self.sources = saved.sources;
            self.issues = saved.issues;
            self.subsource_errors = saved.subsource_errors;
            self.nav_items = saved.nav_items;
            self.nav_idx = saved.nav_idx;
            self.field_names = saved.field_names;
            self.field_schemas = saved.field_schemas;
            self.flags.field_names = saved.field_names_state;
        } else {
            // First time switching to this team — initialize from its config
            self.sources = self.resolved_teams[new_idx]
                .config
                .sources
                .iter()
                .map(|s| (s.id.clone(), SourceState::Pending))
                .collect();
            self.issues = Vec::new();
            self.subsource_errors = IndexMap::new();
            self.nav_items = Vec::new();
            self.nav_idx = 0;
            self.field_names = HashMap::new();
            self.field_schemas = HashMap::new();
            self.flags.field_names = FieldNamesState::Idle;
        }

        // Reset UI state for the new tab
        self.detail_scroll = 0;
        self.view_mode = ViewMode::Default;
        self.focused_panel = FocusedPanel::List;
        self.action_state = ActionState::None;
        self.overlay = None;

        // Trigger source fetches if any sources are still pending
        if self
            .sources
            .values()
            .any(|s| matches!(s, SourceState::Pending))
        {
            self.flags.pending_team_fetch = true;
        }
    }

    /// The active team's config.
    pub fn team_config(&self) -> &TeamConfig {
        &self.resolved_teams[self.active_team_idx].config
    }

    /// The active team's effective Jira config (user default + team override).
    pub fn team_jira(&self) -> &crate::config::types::JiraConfig {
        &self.resolved_teams[self.active_team_idx].jira
    }

    pub fn any_source_loading(&self) -> bool {
        self.sources
            .values()
            .any(|s| matches!(s, SourceState::Pending | SourceState::Loading))
    }

    pub fn selected_issue(&self) -> Option<&Issue> {
        match self.nav_items.get(self.nav_idx)? {
            NavItem::Issue(idx) => self.issues.get(*idx),
            NavItem::SourceError(_) | NavItem::SubsourceError(_, _) => None,
        }
    }

    pub fn selected_nav_item(&self) -> Option<&NavItem> {
        self.nav_items.get(self.nav_idx)
    }

    /// Rebuild the flat issues list from loaded source states (in priority order, deduped),
    /// then rebuild navigable items.
    fn rebuild_issues(&mut self) {
        let mut seen = std::collections::HashSet::new();
        let mut issues = Vec::new();
        for (_, state) in &self.sources {
            if let SourceState::Loaded(source_issues) = state {
                for issue in source_issues {
                    if seen.insert(issue.key.clone()) {
                        issues.push(issue.clone());
                    }
                }
            }
        }
        self.issues = issues;
        self.rebuild_nav();
    }

    /// Rebuild navigable items from sources + issues, preserving the current selection.
    pub fn rebuild_nav(&mut self) {
        let old_item = self.nav_items.get(self.nav_idx).cloned();

        let mut nav_items = Vec::new();
        let mut issue_pos = 0usize;
        for (source_id, state) in &self.sources {
            match state {
                SourceState::Loaded(_) => {
                    let start = issue_pos;
                    while issue_pos < self.issues.len()
                        && self.issues[issue_pos].source_id.as_deref() == Some(source_id.as_str())
                    {
                        issue_pos += 1;
                    }
                    for idx in start..issue_pos {
                        nav_items.push(NavItem::Issue(idx));
                    }
                    // Subsource errors shown after that source's issues.
                    if let Some(errors) = self.subsource_errors.get(source_id) {
                        for (sub_idx, _) in errors {
                            nav_items.push(NavItem::SubsourceError(source_id.clone(), *sub_idx));
                        }
                    }
                }
                SourceState::Error(_) => {
                    nav_items.push(NavItem::SourceError(source_id.clone()));
                }
                SourceState::Pending | SourceState::Loading => {}
            }
        }
        self.nav_items = nav_items;

        // Try to restore the previous selection.
        if let Some(old) = old_item {
            match old {
                NavItem::Issue(old_idx) => {
                    if let Some(key) = self.issues.get(old_idx).map(|i| i.key.clone())
                        && let Some(pos) = self.nav_items.iter().position(|n| {
                            matches!(n, NavItem::Issue(i) if self.issues.get(*i).map(|iss| &iss.key) == Some(&key))
                        })
                    {
                        self.nav_idx = pos;
                        return;
                    }
                }
                NavItem::SourceError(ref id) => {
                    if let Some(pos) = self
                        .nav_items
                        .iter()
                        .position(|n| n == &NavItem::SourceError(id.clone()))
                    {
                        self.nav_idx = pos;
                        return;
                    }
                }
                NavItem::SubsourceError(ref id, sub_idx) => {
                    if let Some(pos) = self
                        .nav_items
                        .iter()
                        .position(|n| n == &NavItem::SubsourceError(id.clone(), sub_idx))
                    {
                        self.nav_idx = pos;
                        return;
                    }
                }
            }
        }
        // Clamp.
        if self.nav_idx >= self.nav_items.len() {
            self.nav_idx = self.nav_items.len().saturating_sub(1);
        }
    }
}

/// Look up a `SourceConfig` by ID.
pub fn source_config_for<'a>(team_config: &'a TeamConfig, id: &str) -> Option<&'a SourceConfig> {
    team_config.sources.iter().find(|s| s.id == id)
}

/// Determine the auto view mode for an issue based on its source config.
fn auto_view_mode(issue: &Issue, team_config: &TeamConfig) -> ViewMode {
    let Some(source_id) = issue.source_id.as_deref() else {
        return ViewMode::Default;
    };
    let view_id = source_config_for(team_config, source_id).and_then(|s| s.view_mode.as_deref());
    match view_id {
        Some(id) if team_config.views.contains_key(id) => ViewMode::Custom(id.to_string()),
        _ => ViewMode::Default,
    }
}

pub fn update_state(app: &mut AppState, event: AppEvent) {
    match event {
        AppEvent::Tick => {
            app.tick_count = app.tick_count.wrapping_add(1);
        }

        AppEvent::SourceLoaded(source_id, issues) => {
            app.sources.insert(source_id, SourceState::Loaded(issues));
            app.rebuild_issues();
            // Auto-update view mode for newly selected issue
            if let Some(issue) = app.selected_issue() {
                let issue = issue.clone();
                let mode = auto_view_mode(&issue, app.team_config());
                if app.view_mode == ViewMode::Default {
                    app.view_mode = mode;
                }
            }
        }

        AppEvent::SourceError(source_id, e) => {
            app.sources
                .insert(source_id, SourceState::Error(Arc::new(e)));
        }

        AppEvent::SubsourceError(source_id, subsource_idx, e) => {
            app.subsource_errors
                .entry(source_id)
                .or_default()
                .push((subsource_idx, Arc::new(e)));
            // nav rebuild deferred until SourceLoaded arrives for this source
        }

        AppEvent::CurrentUserResolved(user) => {
            app.current_user = Some(user);
        }

        AppEvent::ActionDone(result) => {
            handle_action_done(app, result);
        }

        AppEvent::Input(event) => {
            handle_input(app, event);
        }

        AppEvent::PathCompletions {
            generation,
            completions,
        } => {
            if let ActionState::TypingAttachmentPath {
                ref completion_generation,
                completions: ref mut c,
                ref mut completion_idx,
                ..
            } = app.action_state
                && generation == *completion_generation
            {
                *c = completions;
                *completion_idx = None;
            }
        }

        AppEvent::UpdateWarnings(warnings) => {
            app.update_warnings = warnings;
        }
    }
}

fn handle_action_done(app: &mut AppState, result: ActionResult) {
    match result {
        ActionResult::Error(e) => app.action_state = ActionState::Error(Arc::new(e)),
        ActionResult::Hidden { ref issue_key } => apply_hidden(app, issue_key),
        ActionResult::TransitionApplied {
            ref issue_key,
            ref new_status,
        } => apply_transition_applied(app, issue_key, new_status),
        ActionResult::TransitionsLoaded {
            issue_key,
            transitions,
        } => apply_transitions_loaded(app, issue_key, transitions),
        ActionResult::AssignedToMe { ref issue_key } => {
            apply_assigned_to_me(app, issue_key);
            app.action_state = ActionState::None;
        }
        ActionResult::MovedToProject {
            ref issue_key,
            ref project,
        } => apply_moved_to_project(app, issue_key, project),
        ActionResult::CommentPosted {
            issue_key,
            new_comment,
        } => apply_comment_posted(app, &issue_key, new_comment),
        ActionResult::FieldUpdated {
            issue_key,
            field_id,
            new_value,
        } => apply_field_updated(app, &issue_key, &field_id, &new_value),
        ActionResult::FieldOptionsLoaded {
            issue_key,
            field_id,
            label,
            original_json,
            options,
            description,
            multi,
        } => {
            app.action_state = field_options_to_state(
                issue_key,
                field_id,
                label,
                original_json,
                options,
                description,
                multi,
            );
        }
        ActionResult::FieldNamesLoaded {
            names,
            schemas,
            all_fields,
        } => apply_field_names_loaded(app, names, schemas, all_fields),
        ActionResult::CommentEdited {
            issue_key,
            updated_comment,
        } => {
            apply_comment_edit(app, &issue_key, &updated_comment);
            app.action_state = ActionState::None;
        }
        ActionResult::CommentDeleted {
            issue_key,
            comment_id,
        } => {
            apply_comment_deleted(app, &issue_key, &comment_id);
            app.action_state = ActionState::None;
        }
        ActionResult::AttachmentDeleted {
            issue_key,
            attachment_id,
        } => apply_attachment_deleted(app, &issue_key, &attachment_id),
        ActionResult::AttachmentCached {
            attachment_id,
            cache_path,
            open_after,
        } => handle_attachment_cached(app, attachment_id, cache_path.as_path(), open_after),
        ActionResult::AttachmentUploaded {
            issue_key,
            new_attachment,
        } => apply_attachment_uploaded(app, &issue_key, new_attachment),
    }
}

fn apply_hidden(app: &mut AppState, issue_key: &str) {
    app.issues.retain(|i| i.key != issue_key);
    app.rebuild_nav();
    app.action_state = ActionState::None;
}

fn apply_transitions_loaded(
    app: &mut AppState,
    issue_key: String,
    transitions: Vec<crate::jira::types::Transition>,
) {
    app.action_state = ActionState::SelectingTransition {
        issue_key,
        transitions,
        selected: 0,
    };
}

fn apply_field_names_loaded(
    app: &mut AppState,
    names: HashMap<String, String>,
    schemas: HashMap<String, String>,
    all_fields: bool,
) {
    app.field_names.extend(names);
    app.field_schemas.extend(schemas);
    app.flags.field_names = FieldNamesState::Idle;
    if all_fields {
        app.flags.field_names = FieldNamesState::AllLoaded;
    }
}

fn apply_transition_applied(app: &mut AppState, issue_key: &str, new_status: &str) {
    if let Some(issue) = app.issues.iter_mut().find(|i| i.key == issue_key) {
        issue.fields.status.name = new_status.to_string();
    }
    app.action_state = ActionState::None;
}

fn apply_moved_to_project(app: &mut AppState, issue_key: &str, project: &str) {
    if let Some(issue) = app.issues.iter_mut().find(|i| i.key == issue_key) {
        issue.fields.project.key = project.to_string();
    }
    app.action_state = ActionState::None;
}

fn apply_attachment_uploaded(
    app: &mut AppState,
    issue_key: &str,
    new_attachment: crate::jira::types::Attachment,
) {
    if let Some(issue) = app.issues.iter_mut().find(|i| i.key == issue_key) {
        issue
            .fields
            .attachment
            .get_or_insert_with(Vec::new)
            .push(new_attachment);
    }
    app.action_state = ActionState::None;
}

fn handle_attachment_cached(
    app: &mut AppState,
    attachment_id: String,
    cache_path: &std::path::Path,
    open_after: bool,
) {
    app.attachment_fetching_id = None;
    app.attachment_cache
        .insert(attachment_id.clone(), cache_path.to_path_buf());
    if open_after {
        let _ = open::that_detached(cache_path);
    } else if let Ok(bytes) = std::fs::read(cache_path) {
        let ext = cache_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if is_text_extension(&ext) {
            let text = String::from_utf8_lossy(&bytes).to_string();
            app.attachment_text_previews.insert(attachment_id, text);
        } else if is_image_extension(&ext)
            && let Some(picker) = &app.image_picker
            && let Ok(dyn_img) = image::load_from_memory(&bytes)
        {
            let protocol = picker.new_resize_protocol(dyn_img);
            app.attachment_images
                .insert(attachment_id, std::cell::RefCell::new(protocol));
        }
    }
    app.action_state = ActionState::None;
}

fn apply_comment_posted(app: &mut AppState, issue_key: &str, new_comment: Comment) {
    if let Some(issue) = app.issues.iter_mut().find(|i| i.key == issue_key) {
        let list = issue
            .fields
            .comment
            .get_or_insert_with(|| crate::jira::types::CommentList {
                comments: vec![],
                total: 0,
            });
        list.comments.push(new_comment);
        list.total = u32::try_from(list.comments.len()).unwrap_or(0);
    }
    app.action_state = ActionState::None;
}

fn apply_field_updated(
    app: &mut AppState,
    issue_key: &str,
    field_id: &str,
    new_value: &serde_json::Value,
) {
    // Update in-memory field value immediately (no re-fetch needed)
    if let Some(issue) = app.issues.iter_mut().find(|i| i.key == issue_key) {
        issue
            .fields
            .extra
            .insert(field_id.to_owned(), new_value.clone());
    }
    app.action_state = ActionState::None;
}

fn apply_assigned_to_me(app: &mut AppState, issue_key: &str) {
    // Mark assignee as current user in the list (best-effort display update)
    if let Some(ref me) = app.current_user.clone()
        && let Some(issue) = app.issues.iter_mut().find(|i| i.key == issue_key)
    {
        issue.fields.assignee = Some(crate::jira::types::UserField {
            name: None,
            display_name: Some(me.clone()),
            account_id: Some(me.clone()),
        });
    }
}

fn apply_comment_edit(app: &mut AppState, issue_key: &str, updated_comment: &Comment) {
    if let Some(issue) = app.issues.iter_mut().find(|i| i.key == issue_key)
        && let Some(list) = &mut issue.fields.comment
        && let Some(c) = list
            .comments
            .iter_mut()
            .find(|c| c.id == updated_comment.id)
    {
        c.body.clone_from(&updated_comment.body);
        c.updated.clone_from(&updated_comment.updated);
    }
}

fn apply_comment_deleted(app: &mut AppState, issue_key: &str, comment_id: &str) {
    if let Some(issue) = app.issues.iter_mut().find(|i| i.key == issue_key)
        && let Some(list) = &mut issue.fields.comment
    {
        list.comments.retain(|c| c.id != comment_id);
        list.total = u32::try_from(list.comments.len()).unwrap_or(0);
    }
    // Clamp focused comment index
    let comment_count = app
        .selected_issue()
        .and_then(|i| i.fields.comment.as_ref())
        .map_or(0, |l| l.comments.len());
    if app.overlay_focused_comment >= comment_count && comment_count > 0 {
        app.overlay_focused_comment = comment_count - 1;
    } else if comment_count == 0 {
        app.overlay_focused_comment = 0;
    }
}

fn apply_attachment_deleted(app: &mut AppState, issue_key: &str, attachment_id: &str) {
    if let Some(issue) = app.issues.iter_mut().find(|i| i.key == issue_key)
        && let Some(ref mut atts) = issue.fields.attachment
    {
        atts.retain(|a| a.id != attachment_id);
    }
    // Clamp focused attachment index
    let att_count = app
        .selected_issue()
        .and_then(|i| i.fields.attachment.as_deref())
        .map_or(0, <[_]>::len);
    if app.overlay_focused_attachment >= att_count && att_count > 0 {
        app.overlay_focused_attachment = att_count - 1;
    } else if att_count == 0 {
        app.overlay_focused_attachment = 0;
    }
    app.action_state = ActionState::None;
}

fn field_options_to_state(
    issue_key: String,
    field_id: String,
    label: String,
    original_json: serde_json::Value,
    options: Vec<FieldOption>,
    description: Option<String>,
    multi: bool,
) -> ActionState {
    if options.is_empty() {
        // No allowed values — fall back to $EDITOR
        let current_value = crate::tui::views::custom::val_to_str(&original_json);
        ActionState::PendingFieldEdit {
            issue_key,
            field_id,
            current_value,
            original_json,
        }
    } else if multi {
        let current_values: HashSet<String> = original_json
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        item.as_str()
                            .or_else(|| item.get("value").and_then(|v| v.as_str()))
                            .or_else(|| item.get("name").and_then(|v| v.as_str()))
                            .map(str::to_string)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let selected = options
            .iter()
            .enumerate()
            .filter(|(_, o)| current_values.contains(&o.value))
            .map(|(i, _)| i)
            .collect();
        ActionState::SelectingFieldOptions {
            issue_key,
            field_id,
            label,
            original_json,
            options,
            description,
            selected,
            cursor: 0,
        }
    } else {
        ActionState::SelectingFieldOption {
            issue_key,
            field_id,
            label,
            options,
            description,
            cursor: 0,
        }
    }
}

fn handle_attachment_path_input(app: &mut AppState, code: crossterm::event::KeyCode) -> bool {
    use crossterm::event::KeyCode;
    let mut pending_gen: Option<u64> = None;

    if let ActionState::TypingAttachmentPath {
        ref mut path,
        ref mut cursor,
        ref issue_key,
        ref mut completions,
        ref mut completion_idx,
        ref mut completion_generation,
    } = app.action_state
    {
        match code {
            KeyCode::Esc => {
                if completions.is_empty() {
                    app.action_state = ActionState::None;
                } else {
                    *completions = vec![];
                    *completion_idx = None;
                }
            }
            KeyCode::Enter => {
                if let Some(idx) = *completion_idx {
                    if let Some(comp) = completions.get(idx).cloned() {
                        let is_dir = comp.ends_with('/');
                        path.clone_from(&comp);
                        *cursor = comp.chars().count();
                        *completions = vec![];
                        *completion_idx = None;
                        if is_dir {
                            *completion_generation += 1;
                            pending_gen = Some(*completion_generation);
                        } else {
                            let ik = issue_key.clone();
                            app.action_state = ActionState::PendingAttachmentUpload {
                                issue_key: ik,
                                file_path: comp,
                            };
                        }
                    }
                } else if !path.is_empty() {
                    let ik = issue_key.clone();
                    let fp = path.clone();
                    app.action_state = ActionState::PendingAttachmentUpload {
                        issue_key: ik,
                        file_path: fp,
                    };
                }
            }
            KeyCode::Tab | KeyCode::Down => {
                if !completions.is_empty() {
                    let n = completions.len();
                    *completion_idx = Some(completion_idx.map_or(0, |i| (i + 1) % n));
                }
            }
            KeyCode::Up => {
                if !completions.is_empty() {
                    let n = completions.len();
                    *completion_idx = Some(match *completion_idx {
                        None | Some(0) => n - 1,
                        Some(i) => i - 1,
                    });
                }
            }
            KeyCode::Backspace => {
                if *cursor > 0 {
                    let mut chars: Vec<char> = path.chars().collect();
                    chars.remove(*cursor - 1);
                    *path = chars.into_iter().collect();
                    *cursor -= 1;
                }
                *completions = vec![];
                *completion_idx = None;
                *completion_generation += 1;
                pending_gen = Some(*completion_generation);
            }
            KeyCode::Char(c) => {
                let mut chars: Vec<char> = path.chars().collect();
                chars.insert(*cursor, c);
                *path = chars.into_iter().collect();
                *cursor += 1;
                *completions = vec![];
                *completion_idx = None;
                *completion_generation += 1;
                pending_gen = Some(*completion_generation);
            }
            _ => {}
        }
        // borrow of app.action_state ends here
    } else {
        return false;
    }

    if let Some(g) = pending_gen {
        app.pending_completion_fetch = Some(g);
    }
    true
}

fn handle_overlay_input(app: &mut AppState, event: &crossterm::event::Event) {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let Event::Key(KeyEvent {
        code, modifiers, ..
    }) = *event
    else {
        return;
    };

    // Intercept all input while typing an attachment path
    if handle_attachment_path_input(app, code) {
        return;
    }

    // Sub-modal: comment delete confirmation
    if handle_comment_delete_confirm_input(app, code, modifiers) {
        return;
    }

    // Sub-modal: attachment delete confirmation
    if handle_attachment_delete_confirm_input(app, code, modifiers) {
        return;
    }

    // Sub-modal: comment edit confirmation/diff
    if handle_comment_edit_confirm_input(app, code, modifiers) {
        return;
    }

    // Normal overlay navigation and actions
    let is_comments = matches!(app.overlay, Some(SubView::Comments));
    let is_attachments = matches!(app.overlay, Some(SubView::Attachments));
    match (code, modifiers) {
        (KeyCode::Char('q') | KeyCode::Esc, m) if !matches!(m, KeyModifiers::CONTROL) => {
            app.overlay = None;
            app.overlay_scroll = 0;
            app.overlay_focused_comment = 0;
            app.overlay_focused_attachment = 0;
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        (KeyCode::Down | KeyCode::Char('j'), _) => {
            if is_comments {
                overlay_comment_nav_down(app);
            } else {
                overlay_attachment_nav_down(app);
            }
        }
        (KeyCode::Up | KeyCode::Char('k'), _) => {
            if is_comments {
                overlay_comment_nav_up(app);
            } else {
                overlay_attachment_nav_up(app);
            }
        }
        (KeyCode::PageDown, _) => {
            app.overlay_scroll = app.overlay_scroll.saturating_add(10);
        }
        (KeyCode::PageUp, _) => {
            app.overlay_scroll = app.overlay_scroll.saturating_sub(10);
        }
        // Comment actions (only in comments overlay)
        (KeyCode::Char('n'), _) if is_comments => {
            if let Some(issue) = app.selected_issue() {
                app.action_state = ActionState::PendingComment {
                    issue_key: issue.key.clone(),
                };
            }
        }
        (KeyCode::Char('e'), _) if is_comments => {
            start_comment_edit(app);
        }
        (KeyCode::Char('d'), _) if is_comments => {
            start_comment_delete(app);
        }
        (KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter, _) if is_attachments => {
            trigger_attachment_open(app);
        }
        (KeyCode::Char('d'), _) if is_attachments => {
            start_attachment_delete(app);
        }
        (KeyCode::Char('n'), _) if is_attachments => {
            if let Some(issue) = app.selected_issue() {
                app.action_state = ActionState::TypingAttachmentPath {
                    issue_key: issue.key.clone(),
                    path: String::new(),
                    cursor: 0,
                    completions: vec![],
                    completion_idx: None,
                    completion_generation: 0,
                };
            }
        }
        _ => {}
    }
    // Clamp: no scrolling past the end, and no scrolling when content fits
    let max_scroll = app.overlay_content_h.saturating_sub(app.overlay_viewport_h);
    app.overlay_scroll = app.overlay_scroll.min(max_scroll);
}

fn handle_comment_delete_confirm_input(
    app: &mut AppState,
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    let ActionState::ConfirmingCommentDelete {
        issue_key,
        comment_id,
        selected,
    } = &app.action_state.clone()
    else {
        return false;
    };
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        (KeyCode::Esc | KeyCode::Char('q'), _) => {
            app.action_state = ActionState::None;
        }
        (KeyCode::Left | KeyCode::Char('h' | 'l') | KeyCode::Right | KeyCode::Tab, _) => {
            app.action_state = ActionState::ConfirmingCommentDelete {
                issue_key: issue_key.clone(),
                comment_id: comment_id.clone(),
                selected: 1 - selected,
            };
        }
        (KeyCode::Enter, _) => {
            if *selected == 0 {
                app.action_state = ActionState::DeletingComment {
                    issue_key: issue_key.clone(),
                    comment_id: comment_id.clone(),
                };
            } else {
                app.action_state = ActionState::None;
            }
        }
        _ => {}
    }
    true
}

fn handle_attachment_delete_confirm_input(
    app: &mut AppState,
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    let ActionState::ConfirmingAttachmentDelete {
        issue_key,
        attachment_id,
        selected,
    } = &app.action_state.clone()
    else {
        return false;
    };
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        (KeyCode::Esc | KeyCode::Char('q'), _) => {
            app.action_state = ActionState::None;
        }
        (KeyCode::Left | KeyCode::Char('h' | 'l') | KeyCode::Right | KeyCode::Tab, _) => {
            app.action_state = ActionState::ConfirmingAttachmentDelete {
                issue_key: issue_key.clone(),
                attachment_id: attachment_id.clone(),
                selected: 1 - selected,
            };
        }
        (KeyCode::Enter, _) => {
            if *selected == 0 {
                app.action_state = ActionState::DeletingAttachment {
                    issue_key: issue_key.clone(),
                    attachment_id: attachment_id.clone(),
                };
            } else {
                app.action_state = ActionState::None;
            }
        }
        _ => {}
    }
    true
}

fn handle_comment_edit_confirm_input(
    app: &mut AppState,
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    let ActionState::ConfirmingCommentEdit {
        issue_key,
        comment_id,
        old_text,
        new_text,
        tab,
    } = &app.action_state.clone()
    else {
        return false;
    };
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        (KeyCode::Esc | KeyCode::Char('q'), _) => {
            app.action_state = ActionState::None;
        }
        (KeyCode::Tab, _) => {
            app.action_state = ActionState::ConfirmingCommentEdit {
                issue_key: issue_key.clone(),
                comment_id: comment_id.clone(),
                old_text: old_text.clone(),
                new_text: new_text.clone(),
                tab: 1 - tab,
            };
        }
        (KeyCode::Enter, _) => {
            app.action_state = ActionState::CommittingCommentEdit {
                issue_key: issue_key.clone(),
                comment_id: comment_id.clone(),
                new_body: new_text.clone(),
            };
        }
        _ => {}
    }
    true
}

fn overlay_comment_nav_down(app: &mut AppState) {
    let count = app
        .selected_issue()
        .and_then(|i| i.fields.comment.as_ref())
        .map_or(0, |l| l.comments.len());
    if count == 0 {
        return;
    }
    if app.overlay_focused_comment + 1 < count {
        app.overlay_focused_comment += 1;
        auto_scroll_to_comment(app);
    }
}

fn overlay_comment_nav_up(app: &mut AppState) {
    if app.overlay_focused_comment > 0 {
        app.overlay_focused_comment -= 1;
        auto_scroll_to_comment(app);
    }
}

fn auto_scroll_to_comment(app: &mut AppState) {
    let idx = app.overlay_focused_comment;
    let Some(&(top, bottom)) = app.overlay_comment_offsets.get(idx) else {
        return;
    };
    let viewport_h = app.overlay_viewport_h;
    if top < app.overlay_scroll {
        app.overlay_scroll = top;
    } else if bottom > app.overlay_scroll + viewport_h {
        app.overlay_scroll = bottom.saturating_sub(viewport_h);
    }
}

fn overlay_attachment_nav_down(app: &mut AppState) {
    let count = app
        .selected_issue()
        .and_then(|i| i.fields.attachment.as_ref())
        .map_or(0, std::vec::Vec::len);
    if count == 0 {
        return;
    }
    if app.overlay_focused_attachment + 1 < count {
        app.overlay_focused_attachment += 1;
        auto_scroll_to_attachment(app);
        maybe_fetch_attachment_preview(app);
    }
}

fn overlay_attachment_nav_up(app: &mut AppState) {
    if app.overlay_focused_attachment > 0 {
        app.overlay_focused_attachment -= 1;
        auto_scroll_to_attachment(app);
        maybe_fetch_attachment_preview(app);
    }
}

const fn auto_scroll_to_attachment(app: &mut AppState) {
    let idx = app.overlay_focused_attachment;
    let viewport_h = app.overlay_viewport_h;
    if viewport_h == 0 {
        return;
    }
    if idx < app.overlay_scroll {
        app.overlay_scroll = idx;
    } else if idx >= app.overlay_scroll + viewport_h {
        app.overlay_scroll = idx + 1 - viewport_h;
    }
}

/// Compute the local cache path for an attachment.
pub fn cache_path_for(issue_key: &str, attachment_id: &str, filename: &str) -> std::path::PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join("do-next")
        .join(issue_key)
        .join(format!("{attachment_id}-{filename}"))
}

/// Schedule a silent background preview fetch for the currently focused attachment,
/// unless it is already cached or already in flight.
pub fn maybe_fetch_attachment_preview(app: &mut AppState) {
    let Some(issue) = app.selected_issue() else {
        return;
    };
    let attachments = issue.fields.attachment.as_deref().unwrap_or(&[]);
    let Some(att) = attachments.get(app.overlay_focused_attachment) else {
        return;
    };
    let Some(content_url) = att.content.clone() else {
        return;
    };
    let att_id = att.id.clone();
    let filename = att.filename.clone();
    let issue_key = issue.key.clone();

    if app.attachment_cache.contains_key(&att_id) {
        return;
    }
    if app.attachment_fetching_id.as_deref() == Some(att_id.as_str()) {
        return;
    }

    // If the file is already on disk from a previous run, pre-populate the cache so
    // "fetching…" is not shown, and schedule a decode-only task (no HTTP request).
    let cache_path = cache_path_for(&issue_key, &att_id, &filename);
    if cache_path.exists() {
        app.attachment_cache.insert(att_id.clone(), cache_path);
        app.pending_attachment_fetch = Some(AttachmentFetchRequest {
            attachment_id: att_id,
            content_url,
            filename,
            issue_key,
        });
        return;
    }

    app.attachment_fetching_id = Some(att_id.clone());
    app.pending_attachment_fetch = Some(AttachmentFetchRequest {
        attachment_id: att_id,
        content_url,
        filename,
        issue_key,
    });
}

/// Trigger opening the focused attachment with the system default app.
/// If already cached, opens immediately; otherwise sets `OpeningAttachment` action state.
fn trigger_attachment_open(app: &mut AppState) {
    let Some(issue) = app.selected_issue() else {
        return;
    };
    let attachments = issue.fields.attachment.as_deref().unwrap_or(&[]);
    let Some(att) = attachments.get(app.overlay_focused_attachment) else {
        return;
    };
    let Some(content_url) = att.content.clone() else {
        return;
    };
    let att_id = att.id.clone();

    if let Some(path) = app.attachment_cache.get(&att_id) {
        let _ = open::that_detached(path);
        return;
    }

    app.action_state = ActionState::OpeningAttachment {
        attachment_id: att_id,
        content_url,
        filename: att.filename.clone(),
        issue_key: issue.key.clone(),
    };
}

fn is_text_extension(ext: &str) -> bool {
    matches!(
        ext,
        "txt"
            | "md"
            | "log"
            | "json"
            | "yaml"
            | "yml"
            | "xml"
            | "html"
            | "csv"
            | "toml"
            | "rs"
            | "py"
            | "js"
            | "ts"
            | "sh"
            | "conf"
            | "cfg"
            | "ini"
            | "sql"
            | "diff"
            | "patch"
            | "env"
            | "tf"
            | "go"
            | "rb"
            | "java"
            | "c"
            | "cpp"
            | "h"
    )
}

fn is_image_extension(ext: &str) -> bool {
    matches!(
        ext,
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "tif" | "ico"
    )
}

fn start_comment_edit(app: &mut AppState) {
    let Some(issue) = app.selected_issue() else {
        return;
    };
    let Some(list) = &issue.fields.comment else {
        return;
    };
    let Some(comment) = list.comments.get(app.overlay_focused_comment) else {
        return;
    };
    app.action_state = ActionState::PendingCommentEdit {
        issue_key: issue.key.clone(),
        comment_id: comment.id.clone(),
        original_body: crate::jira::adf::json_to_text(&comment.body),
    };
}

fn start_comment_delete(app: &mut AppState) {
    let Some(issue) = app.selected_issue() else {
        return;
    };
    let Some(list) = &issue.fields.comment else {
        return;
    };
    let Some(comment) = list.comments.get(app.overlay_focused_comment) else {
        return;
    };
    app.action_state = ActionState::ConfirmingCommentDelete {
        issue_key: issue.key.clone(),
        comment_id: comment.id.clone(),
        selected: 1, // default to No
    };
}

fn start_attachment_delete(app: &mut AppState) {
    let Some(issue) = app.selected_issue() else {
        return;
    };
    let attachments = issue.fields.attachment.as_deref().unwrap_or(&[]);
    let Some(att) = attachments.get(app.overlay_focused_attachment) else {
        return;
    };
    app.action_state = ActionState::ConfirmingAttachmentDelete {
        issue_key: issue.key.clone(),
        attachment_id: att.id.clone(),
        selected: 1, // default to No
    };
}

fn handle_input(app: &mut AppState, event: crossterm::event::Event) {
    use crossterm::event::{Event, KeyEvent};

    // Sub-view overlay captures all input
    if app.overlay.is_some() {
        handle_overlay_input(app, &event);
        return;
    }

    // Handle overlay-specific input first
    match &app.action_state {
        ActionState::SelectingTransition { .. } => {
            handle_transition_input(app, event);
            return;
        }
        ActionState::HidePopup { .. } => {
            handle_hide_input(app, event);
            return;
        }
        ActionState::Error(_) => {
            // Any key dismisses error
            app.action_state = ActionState::None;
            return;
        }
        ActionState::InlineEditingField { .. } => {
            handle_inline_edit_input(app, event);
            return;
        }
        ActionState::SelectingFieldOption { .. } => {
            handle_select_option_input(app, event);
            return;
        }
        ActionState::SelectingFieldOptions { .. } => {
            handle_select_options_input(app, event);
            return;
        }
        ActionState::EditingDatetimeField { .. } => {
            handle_datetime_picker_input(app, event);
            return;
        }
        ActionState::OfferingTemplate { .. } => {
            handle_offering_template_input(app, &event);
            return;
        }
        ActionState::ConfirmingFieldEdit { .. } => {
            handle_confirm_field_edit_input(app, &event);
            return;
        }
        ActionState::KeybindingsHelp => {
            if let crossterm::event::Event::Key(crossterm::event::KeyEvent { code, .. }) = event
                && matches!(code, KeyCode::Char('q') | KeyCode::Esc)
            {
                app.action_state = ActionState::None;
            }
            return;
        }
        ActionState::AwaitingAction { .. }
        | ActionState::LoadingTransitions { .. }
        | ActionState::PendingTransition { .. }
        | ActionState::PendingHide { .. }
        | ActionState::PendingAssign { .. }
        | ActionState::PendingMove { .. }
        | ActionState::PendingComment { .. }
        | ActionState::PendingFieldEdit { .. }
        | ActionState::LoadingFieldOptions { .. }
        | ActionState::CommittingFieldEdit { .. }
        | ActionState::PendingCommentEdit { .. }
        | ActionState::CommittingCommentEdit { .. }
        | ActionState::DeletingComment { .. }
        | ActionState::ConfirmingCommentEdit { .. }
        | ActionState::ConfirmingCommentDelete { .. }
        | ActionState::ConfirmingAttachmentDelete { .. }
        | ActionState::DeletingAttachment { .. }
        | ActionState::OpeningAttachment { .. }
        | ActionState::PendingAttachmentUpload { .. }
        | ActionState::TypingAttachmentPath { .. } => {
            // Ignore input while waiting / handled by overlay
            return;
        }
        ActionState::None => {}
    }

    if let Event::Key(KeyEvent {
        code, modifiers, ..
    }) = event
    {
        handle_key(app, code, modifiers);
    }
}

fn handle_key(app: &mut AppState, code: KeyCode, modifiers: KeyModifiers) {
    // `gg` motion: first `g` arms the latch; a second `g` fires jump-to-first.
    // Any other key clears the latch (handled at the end of each arm via the default clear below).
    if code == KeyCode::Char('g') {
        if app.flags.pending_g {
            app.flags.pending_g = false;
            key_jump_first(app);
        } else {
            app.flags.pending_g = true;
        }
        return;
    }
    app.flags.pending_g = false;

    match (code, modifiers) {
        // Tab switching (only when multiple teams)
        (KeyCode::Tab, _) if app.resolved_teams.len() > 1 => {
            app.switch_team((app.active_team_idx + 1) % app.resolved_teams.len());
        }
        (KeyCode::BackTab, _) if app.resolved_teams.len() > 1 => {
            let len = app.resolved_teams.len();
            app.switch_team((app.active_team_idx + len - 1) % len);
        }
        (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        (KeyCode::Left | KeyCode::Char('h'), _) => {
            app.focused_panel = FocusedPanel::List;
        }
        (KeyCode::Right | KeyCode::Char('l'), _) => {
            app.focused_panel = FocusedPanel::Detail;
        }
        (KeyCode::Down | KeyCode::Char('j'), _) => key_nav_down(app),
        (KeyCode::Up | KeyCode::Char('k'), _) => key_nav_up(app),
        (KeyCode::Enter, _) => {
            if app.focused_panel == FocusedPanel::Detail
                && matches!(app.view_mode, ViewMode::Default | ViewMode::Custom(_))
            {
                key_edit_detail_field(app);
            }
        }
        (KeyCode::Char('v'), _) => {
            // Cycle view modes manually
            app.view_mode = match &app.view_mode {
                ViewMode::Default | ViewMode::Custom(_) => ViewMode::Comments,
                ViewMode::Comments => ViewMode::Attachments,
                ViewMode::Attachments => {
                    // Return to the natural view for the current issue
                    app.selected_issue().map_or(ViewMode::Default, |issue| {
                        auto_view_mode(&issue.clone(), app.team_config())
                    })
                }
            };
            app.detail_scroll = 0;
        }
        (KeyCode::PageDown, _) => {
            app.detail_scroll = app.detail_scroll.saturating_add(10);
        }
        (KeyCode::PageUp, _) => {
            app.detail_scroll = app.detail_scroll.saturating_sub(10);
        }
        (KeyCode::Char('o'), _) => {
            if let Some(issue) = app.selected_issue() {
                let url = format!("{}/browse/{}", app.team_jira().base_url, issue.key);
                let _ = open::that(url);
            }
        }
        (KeyCode::Char('t'), _) => {
            if let Some(issue) = app.selected_issue() {
                let key = issue.key.clone();
                app.action_state = ActionState::LoadingTransitions { issue_key: key };
            }
        }
        (KeyCode::Char('c'), _) => {
            if let Some(issue) = app.selected_issue() {
                let key = issue.key.clone();
                app.action_state = ActionState::PendingComment { issue_key: key };
            }
        }
        (KeyCode::Char('G'), _) => key_jump_last(app),
        (KeyCode::Char('i'), _) => key_hide(app),
        (KeyCode::Char('a'), _) => key_assign(app),
        (KeyCode::Char('m'), _) => key_move(app),
        (KeyCode::Char('?'), _) => {
            app.action_state = ActionState::KeybindingsHelp;
        }
        _ => {}
    }
}

fn key_nav_down(app: &mut AppState) {
    if app.focused_panel == FocusedPanel::Detail
        && matches!(app.view_mode, ViewMode::Default | ViewMode::Custom(_))
    {
        let view_cfg = crate::tui::views::custom::current_view_config(app);
        let num_fields = crate::tui::views::custom::num_view_fields(view_cfg, app.selected_issue());
        app.detail_focus = match &app.detail_focus {
            DetailFocus::Comments => DetailFocus::Attachments,
            DetailFocus::Attachments => {
                if num_fields > 0 {
                    DetailFocus::Field(0)
                } else {
                    DetailFocus::Attachments
                }
            }
            DetailFocus::Field(i) => {
                if *i + 1 < num_fields {
                    DetailFocus::Field(i + 1)
                } else {
                    DetailFocus::Field(*i)
                }
            }
        };
        auto_scroll_to_field(app);
    } else if app.focused_panel == FocusedPanel::Detail {
        app.detail_scroll = app.detail_scroll.saturating_add(1);
    } else if !app.nav_items.is_empty() {
        app.nav_idx = (app.nav_idx + 1).min(app.nav_items.len() - 1);
        update_view_mode_on_navigate(app);
    }
}

fn key_nav_up(app: &mut AppState) {
    if app.focused_panel == FocusedPanel::Detail
        && matches!(app.view_mode, ViewMode::Default | ViewMode::Custom(_))
    {
        app.detail_focus = match &app.detail_focus {
            DetailFocus::Comments | DetailFocus::Attachments => DetailFocus::Comments,
            DetailFocus::Field(0) => DetailFocus::Attachments,
            DetailFocus::Field(i) => DetailFocus::Field(i - 1),
        };
        auto_scroll_to_field(app);
    } else if app.focused_panel == FocusedPanel::Detail {
        app.detail_scroll = app.detail_scroll.saturating_sub(1);
    } else if app.nav_idx > 0 {
        app.nav_idx -= 1;
        update_view_mode_on_navigate(app);
    }
}

fn key_jump_first(app: &mut AppState) {
    if app.focused_panel == FocusedPanel::Detail {
        if matches!(app.view_mode, ViewMode::Default | ViewMode::Custom(_)) {
            app.detail_focus = DetailFocus::Comments;
            auto_scroll_to_field(app);
        } else {
            app.detail_scroll = 0;
        }
    } else {
        app.nav_idx = 0;
        update_view_mode_on_navigate(app);
    }
}

fn key_jump_last(app: &mut AppState) {
    if app.focused_panel == FocusedPanel::Detail {
        if matches!(app.view_mode, ViewMode::Default | ViewMode::Custom(_)) {
            let view_cfg = crate::tui::views::custom::current_view_config(app);
            let num_fields =
                crate::tui::views::custom::num_view_fields(view_cfg, app.selected_issue());
            app.detail_focus = if num_fields > 0 {
                DetailFocus::Field(num_fields - 1)
            } else {
                DetailFocus::Attachments
            };
            auto_scroll_to_field(app);
        } else {
            app.detail_scroll = app
                .last_detail_content_h
                .saturating_sub(app.last_detail_viewport_h);
        }
    } else if !app.nav_items.is_empty() {
        app.nav_idx = app.nav_items.len() - 1;
        update_view_mode_on_navigate(app);
    }
}

fn key_hide(app: &mut AppState) {
    if let Some(issue) = app.selected_issue() {
        let key = issue.key.clone();
        let can_hide = issue
            .source_id
            .as_deref()
            .and_then(|id| source_config_for(app.team_config(), id))
            .is_some_and(|s| s.allow_hide_for_a_day);
        if can_hide {
            app.action_state = ActionState::HidePopup {
                issue_key: key,
                selected_solution: 0,
            };
        }
    }
}

fn key_assign(app: &mut AppState) {
    if let Some(issue) = app.selected_issue() {
        let key = issue.key.clone();
        // Assign-to-me is available for sources that have an "unassigned" subsource
        let can_assign = issue
            .source_id
            .as_deref()
            .and_then(|id| source_config_for(app.team_config(), id))
            .is_some_and(|s| {
                s.subsources
                    .iter()
                    .any(|sub| sub.badge.as_deref() == Some("unassigned"))
            });
        if can_assign {
            app.action_state = ActionState::PendingAssign { issue_key: key };
        }
    }
}

fn key_move(app: &mut AppState) {
    if let Some(issue) = app.selected_issue() {
        let wrong_project = issue
            .source_id
            .as_deref()
            .and_then(|id| source_config_for(app.team_config(), id))
            .and_then(|s| s.expected_project.as_ref())
            .is_some_and(|ep| issue.fields.project.key != *ep);
        if wrong_project {
            let key = issue.key.clone();
            app.action_state = ActionState::PendingMove { issue_key: key };
        }
    }
}

fn update_view_mode_on_navigate(app: &mut AppState) {
    if let Some(issue) = app.selected_issue() {
        let issue = issue.clone();
        app.view_mode = auto_view_mode(&issue, app.team_config());
    }
    app.detail_scroll = 0;
    app.overlay = None;
    app.detail_focus = DetailFocus::Comments;
    app.detail_focus_offsets.clear();
    app.flags.field_names = FieldNamesState::Idle;
}

/// Open a readonly URL, routing Slack links to the desktop app when enabled.
fn open_readonly_url(
    url: &str,
    open_slack_in_app: bool,
    slack_team_id: Option<&str>,
    open_with: Option<&str>,
) {
    let is_slack = url.contains(".slack.com/");
    let use_slack = match open_with {
        Some("browser") => false,
        Some("slack") => true,
        _ => open_slack_in_app && is_slack,
    };
    if use_slack && let Some(deep_link) = slack_deep_link(url, slack_team_id) {
        let _ = open::that_detached(deep_link);
        return;
    }
    let _ = open::that_detached(url);
}

/// Convert a Slack web URL to a `slack://` deep link.
///
/// Input:  `https://workspace.slack.com/archives/C0123ABC/p1234567890123456`
/// Output: `slack://channel?team=T0123&id=C0123ABC&thread_ts=1234567890.123456`
fn slack_deep_link(url: &str, team_id: Option<&str>) -> Option<String> {
    let team_id = team_id?;
    let path = url.split(".slack.com/").nth(1)?;
    let mut segments = path.split('/');
    if segments.next()? != "archives" {
        return None;
    }
    let channel_id = segments.next()?;
    let mut deep = format!("slack://channel?team={team_id}&id={channel_id}");
    if let Some(msg_segment) = segments.next()
        && let Some(raw_ts) = msg_segment.strip_prefix('p')
        && raw_ts.len() > 6
    {
        // Slack timestamps: "p1234567890123456" → "1234567890.123456"
        let (secs, micros) = raw_ts.split_at(raw_ts.len() - 6);
        let _ = write!(deep, "&thread_ts={secs}.{micros}");
    }
    Some(deep)
}

#[allow(clippy::too_many_lines)]
fn key_edit_detail_field(app: &mut AppState) {
    let Some(issue) = app.selected_issue() else {
        return;
    };
    let issue = issue.clone();

    // Nav widget: open as popup overlay (one layer deeper)
    match &app.detail_focus {
        DetailFocus::Comments => {
            app.overlay = Some(SubView::Comments);
            app.overlay_scroll = 0;
            return;
        }
        DetailFocus::Attachments => {
            app.overlay = Some(SubView::Attachments);
            app.overlay_scroll = 0;
            app.overlay_focused_attachment = 0;
            maybe_fetch_attachment_preview(app);
            return;
        }
        DetailFocus::Field(_) => {}
    }

    let field_idx = match &app.detail_focus {
        DetailFocus::Field(i) => *i,
        _ => return,
    };
    let view_cfg = crate::tui::views::custom::current_view_config(app);
    let (field_id, original_json) =
        crate::tui::views::custom::view_editable_field_spec(view_cfg, &issue, field_idx);

    if field_id.is_empty() {
        return;
    }

    let field_cfg = crate::tui::views::custom::view_field_cfg(view_cfg, Some(&issue), field_idx);

    // Readonly fields: open URL if the value is a link, otherwise do nothing.
    // Slack URLs are opened in the Slack desktop app by default.
    if field_cfg.as_ref().and_then(|f| f.readonly).unwrap_or(false) {
        if let serde_json::Value::String(s) = &original_json
            && (s.starts_with("http://") || s.starts_with("https://"))
        {
            let team = &app.resolved_teams[app.active_team_idx];
            let open_with = field_cfg.as_ref().and_then(|f| f.open_with.as_deref());
            open_readonly_url(
                s,
                team.open_slack_in_app,
                team.slack_team_id.as_deref(),
                open_with,
            );
        }
        return;
    }

    let label = field_cfg
        .as_ref()
        .map(|f| crate::tui::views::custom::resolve_field_label(f, &app.field_names))
        .unwrap_or_default();
    let description = field_cfg.as_ref().and_then(|f| f.hint.clone());

    // `use_editor: true` always opens $EDITOR regardless of field type
    let use_editor = field_cfg
        .as_ref()
        .and_then(|f| f.use_editor)
        .unwrap_or(false);

    // Datetime picker: triggered by `datetime: true` config flag or editmeta schema type
    if !use_editor {
        let by_config = field_cfg.as_ref().and_then(|f| f.datetime).unwrap_or(false);
        let by_schema = app
            .field_schemas
            .get(&field_id)
            .is_some_and(|t| t == "date" || t == "datetime");
        if by_config || by_schema {
            let tz = crate::tui::views::custom::resolve_tz(view_cfg);
            let picker = crate::tui::overlays::datetime_picker::DatetimePicker::from_value(
                &original_json,
                tz,
            );
            app.action_state = ActionState::EditingDatetimeField {
                issue_key: issue.key,
                field_id,
                label,
                description,
                picker,
            };
            return;
        }
    }

    let templates = field_cfg
        .as_ref()
        .map(|f| {
            let team_path = &app.resolved_teams[app.active_team_idx].path;
            resolve_templates(team_path, &f.effective_templates())
        })
        .unwrap_or_default();

    if use_editor {
        let current_value = crate::tui::views::custom::val_to_str(&original_json);
        let field_is_empty = original_json.is_null() || current_value.is_empty();
        if field_is_empty && !templates.is_empty() {
            app.action_state = ActionState::OfferingTemplate {
                issue_key: issue.key,
                field_id,
                templates,
                cursor: 0,
                original_json,
                previewing: false,
                scroll: 0,
            };
            return;
        }
        app.action_state = ActionState::PendingFieldEdit {
            issue_key: issue.key,
            field_id,
            current_value,
            original_json,
        };
        return;
    }

    set_detail_edit_state(
        app,
        issue.key,
        field_id,
        field_idx,
        label,
        description,
        original_json,
        templates,
    );
}

#[allow(clippy::too_many_arguments)]
fn set_detail_edit_state(
    app: &mut AppState,
    issue_key: String,
    field_id: String,
    field_idx: usize,
    label: String,
    description: Option<String>,
    original_json: serde_json::Value,
    templates: Vec<LoadedTemplate>,
) {
    let is_empty = match &original_json {
        serde_json::Value::Null => true,
        serde_json::Value::String(s) => s.is_empty(),
        _ => false,
    };
    if is_empty && !templates.is_empty() {
        app.action_state = ActionState::OfferingTemplate {
            issue_key,
            field_id,
            templates,
            cursor: 0,
            original_json,
            previewing: false,
            scroll: 0,
        };
        return;
    }
    match &original_json {
        serde_json::Value::Object(map) if map.contains_key("value") => {
            app.action_state = ActionState::LoadingFieldOptions {
                issue_key,
                field_id,
                label,
                original_json,
                description,
                multi: false,
            };
        }
        serde_json::Value::Array(_) => {
            app.action_state = ActionState::LoadingFieldOptions {
                issue_key,
                field_id,
                label,
                original_json,
                description,
                multi: true,
            };
        }
        serde_json::Value::String(s) if s.contains('\n') => {
            let current_value = crate::tui::views::custom::val_to_str(&original_json);
            app.action_state = ActionState::PendingFieldEdit {
                issue_key,
                field_id,
                current_value,
                original_json,
            };
        }
        _ => {
            let input = crate::tui::views::custom::val_to_str(&original_json);
            let cursor = input.chars().count();
            app.action_state = ActionState::InlineEditingField {
                issue_key,
                field_id,
                field_idx,
                input,
                cursor,
            };
        }
    }
}

fn focus_offset_idx(focus: &DetailFocus) -> usize {
    match focus {
        DetailFocus::Comments => 0,
        DetailFocus::Attachments => 1,
        DetailFocus::Field(i) => 2 + i,
    }
}

fn auto_scroll_to_field(app: &mut AppState) {
    let idx = focus_offset_idx(&app.detail_focus);
    let Some(&(top, bottom)) = app.detail_focus_offsets.get(idx) else {
        return;
    };
    let viewport_h = app.last_detail_viewport_h;
    if bottom > app.detail_scroll + viewport_h {
        app.detail_scroll = bottom.saturating_sub(viewport_h);
    } else if top < app.detail_scroll {
        app.detail_scroll = top;
    }
}

#[allow(clippy::needless_pass_by_value)]
fn handle_transition_input(app: &mut AppState, event: crossterm::event::Event) {
    use crossterm::event::{Event, KeyCode, KeyEvent};
    let ActionState::SelectingTransition {
        ref transitions,
        ref mut selected,
        ref issue_key,
    } = app.action_state
    else {
        return;
    };

    if let Event::Key(KeyEvent { code, .. }) = event {
        match code {
            KeyCode::Esc => {
                app.action_state = ActionState::None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if *selected + 1 < transitions.len() {
                    *selected += 1;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if *selected > 0 {
                    *selected -= 1;
                }
            }
            KeyCode::Enter => {
                let transition_id = transitions[*selected].id.clone();
                let key = issue_key.clone();
                app.action_state = ActionState::PendingTransition {
                    issue_key: key,
                    transition_id,
                };
            }
            _ => {}
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn handle_hide_input(app: &mut AppState, event: crossterm::event::Event) {
    use crossterm::event::{Event, KeyCode, KeyEvent};
    let solutions_len = app.team_config().hide_for_a_day.suggested_solutions.len();

    if let Event::Key(KeyEvent { code, .. }) = event {
        match code {
            KeyCode::Esc => {
                app.action_state = ActionState::None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let ActionState::HidePopup {
                    ref mut selected_solution,
                    ..
                } = app.action_state
                    && *selected_solution + 1 < solutions_len
                {
                    *selected_solution += 1;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let ActionState::HidePopup {
                    ref mut selected_solution,
                    ..
                } = app.action_state
                    && *selected_solution > 0
                {
                    *selected_solution -= 1;
                }
            }
            KeyCode::Enter => {
                if let ActionState::HidePopup { ref issue_key, .. } = app.action_state {
                    let key = issue_key.clone();
                    app.action_state = ActionState::PendingHide { issue_key: key };
                }
            }
            _ => {}
        }
    }
}

fn handle_confirm_field_edit_input(app: &mut AppState, event: &crossterm::event::Event) {
    use crossterm::event::{Event, KeyCode, KeyEvent};
    let ActionState::ConfirmingFieldEdit {
        ref issue_key,
        ref field_id,
        ref new_value,
        ref mut tab,
        ..
    } = app.action_state
    else {
        return;
    };
    let Event::Key(KeyEvent { code, .. }) = event else {
        return;
    };
    match code {
        KeyCode::Tab => {
            *tab = 1 - *tab;
        }
        KeyCode::Char('y') | KeyCode::Enter => {
            let issue_key = issue_key.clone();
            let field_id = field_id.clone();
            let new_value = new_value.clone();
            app.action_state = ActionState::CommittingFieldEdit {
                issue_key,
                field_id,
                new_value,
            };
        }
        KeyCode::Char('n' | 'q') | KeyCode::Esc => {
            app.action_state = ActionState::None;
        }
        _ => {}
    }
}

fn resolve_templates(
    team_path: &str,
    entries: &[crate::config::types::TemplateEntry],
) -> Vec<LoadedTemplate> {
    entries
        .iter()
        .filter_map(|entry| {
            let path = std::path::Path::new(team_path).join(&entry.path);
            let content = std::fs::read_to_string(path).ok()?;
            let trimmed = content.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(LoadedTemplate {
                    name: entry.name.clone(),
                    content: trimmed,
                })
            }
        })
        .collect()
}

fn handle_offering_template_input(app: &mut AppState, event: &crossterm::event::Event) {
    use crossterm::event::{Event, KeyCode, KeyEvent};

    let ActionState::OfferingTemplate {
        ref issue_key,
        ref field_id,
        ref templates,
        ref mut cursor,
        ref original_json,
        ref mut previewing,
        ref mut scroll,
    } = app.action_state
    else {
        return;
    };
    let Event::Key(KeyEvent { code, .. }) = event else {
        return;
    };

    if *previewing {
        // Full preview mode
        match code {
            KeyCode::Char('y') | KeyCode::Enter => {
                let issue_key = issue_key.clone();
                let field_id = field_id.clone();
                let original_json = original_json.clone();
                let current_value = templates[*cursor].content.clone();
                app.action_state = ActionState::PendingFieldEdit {
                    issue_key,
                    field_id,
                    current_value,
                    original_json,
                };
            }
            KeyCode::Char('n') => {
                let issue_key = issue_key.clone();
                let field_id = field_id.clone();
                let original_json = original_json.clone();
                app.action_state = ActionState::PendingFieldEdit {
                    issue_key,
                    field_id,
                    current_value: String::new(),
                    original_json,
                };
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                *previewing = false;
                *scroll = 0;
            }
            KeyCode::Up => {
                *scroll = scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                *scroll = scroll.saturating_add(1);
            }
            _ => {}
        }
    } else {
        // Dialog mode with template selection
        match code {
            KeyCode::Char('y') | KeyCode::Enter => {
                let issue_key = issue_key.clone();
                let field_id = field_id.clone();
                let original_json = original_json.clone();
                let current_value = templates[*cursor].content.clone();
                app.action_state = ActionState::PendingFieldEdit {
                    issue_key,
                    field_id,
                    current_value,
                    original_json,
                };
            }
            KeyCode::Char('n') => {
                let issue_key = issue_key.clone();
                let field_id = field_id.clone();
                let original_json = original_json.clone();
                app.action_state = ActionState::PendingFieldEdit {
                    issue_key,
                    field_id,
                    current_value: String::new(),
                    original_json,
                };
            }
            KeyCode::Char('p') => {
                *previewing = true;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                *cursor = cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let max = templates.len().saturating_sub(1);
                *cursor = (*cursor + 1).min(max);
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                app.action_state = ActionState::None;
            }
            _ => {}
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn handle_inline_edit_input(app: &mut AppState, event: crossterm::event::Event) {
    use crossterm::event::{Event, KeyCode, KeyEvent};

    let ActionState::InlineEditingField { .. } = app.action_state else {
        return;
    };
    let Event::Key(KeyEvent { code, .. }) = event else {
        return;
    };

    match code {
        KeyCode::Esc => {
            app.action_state = ActionState::None;
        }
        KeyCode::Enter => {
            let (key, fid, new_val) = if let ActionState::InlineEditingField {
                ref issue_key,
                ref field_id,
                ref input,
                ..
            } = app.action_state
            {
                (
                    issue_key.clone(),
                    field_id.clone(),
                    serde_json::Value::String(input.clone()),
                )
            } else {
                return;
            };
            app.action_state = ActionState::CommittingFieldEdit {
                issue_key: key,
                field_id: fid,
                new_value: new_val,
            };
        }
        code => {
            if let ActionState::InlineEditingField {
                ref mut cursor,
                ref mut input,
                ..
            } = app.action_state
            {
                edit_text(input, cursor, code);
            }
        }
    }
}

fn edit_text(input: &mut String, cursor: &mut usize, code: KeyCode) {
    match code {
        KeyCode::Left => {
            if *cursor > 0 {
                *cursor -= 1;
            }
        }
        KeyCode::Right => {
            if *cursor < input.chars().count() {
                *cursor += 1;
            }
        }
        KeyCode::Home => {
            *cursor = 0;
        }
        KeyCode::End => {
            *cursor = input.chars().count();
        }
        KeyCode::Backspace => {
            if *cursor > 0 {
                let byte_idx = char_to_byte(*cursor - 1, input);
                let char_len = input[byte_idx..].chars().next().map_or(0, char::len_utf8);
                input.drain(byte_idx..byte_idx + char_len);
                *cursor -= 1;
            }
        }
        KeyCode::Delete => {
            if *cursor < input.chars().count() {
                let byte_idx = char_to_byte(*cursor, input);
                let char_len = input[byte_idx..].chars().next().map_or(0, char::len_utf8);
                input.drain(byte_idx..byte_idx + char_len);
            }
        }
        KeyCode::Char(c) => {
            let byte_idx = char_to_byte(*cursor, input);
            input.insert(byte_idx, c);
            *cursor += 1;
        }
        _ => {}
    }
}

fn char_to_byte(char_idx: usize, s: &str) -> usize {
    s.char_indices().nth(char_idx).map_or(s.len(), |(b, _)| b)
}

#[allow(clippy::needless_pass_by_value)]
fn handle_select_option_input(app: &mut AppState, event: crossterm::event::Event) {
    use crossterm::event::{Event, KeyCode, KeyEvent};

    let ActionState::SelectingFieldOption { .. } = app.action_state else {
        return;
    };
    let Event::Key(KeyEvent { code, .. }) = event else {
        return;
    };

    match code {
        KeyCode::Esc => {
            app.action_state = ActionState::None;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let ActionState::SelectingFieldOption {
                ref mut cursor,
                ref options,
                ..
            } = app.action_state
                && *cursor + 1 < options.len()
            {
                *cursor += 1;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let ActionState::SelectingFieldOption { ref mut cursor, .. } = app.action_state
                && *cursor > 0
            {
                *cursor -= 1;
            }
        }
        KeyCode::Enter => {
            let (key, fid, new_val) = if let ActionState::SelectingFieldOption {
                ref issue_key,
                ref field_id,
                ref options,
                cursor,
                ..
            } = app.action_state
            {
                let value = options.get(cursor).map_or("", |o| &o.value).to_string();
                (
                    issue_key.clone(),
                    field_id.clone(),
                    serde_json::json!({ "value": value }),
                )
            } else {
                return;
            };
            app.action_state = ActionState::CommittingFieldEdit {
                issue_key: key,
                field_id: fid,
                new_value: new_val,
            };
        }
        _ => {}
    }
}

#[allow(clippy::needless_pass_by_value)]
fn handle_select_options_input(app: &mut AppState, event: crossterm::event::Event) {
    use crossterm::event::{Event, KeyCode, KeyEvent};

    let ActionState::SelectingFieldOptions { .. } = app.action_state else {
        return;
    };
    let Event::Key(KeyEvent { code, .. }) = event else {
        return;
    };

    match code {
        KeyCode::Esc => {
            app.action_state = ActionState::None;
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if let ActionState::SelectingFieldOptions {
                ref mut cursor,
                ref options,
                ..
            } = app.action_state
                && *cursor + 1 < options.len()
            {
                *cursor += 1;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            if let ActionState::SelectingFieldOptions { ref mut cursor, .. } = app.action_state
                && *cursor > 0
            {
                *cursor -= 1;
            }
        }
        KeyCode::Char(' ') => {
            if let ActionState::SelectingFieldOptions {
                ref cursor,
                ref mut selected,
                ..
            } = app.action_state
            {
                let idx = *cursor;
                if selected.contains(&idx) {
                    selected.remove(&idx);
                } else {
                    selected.insert(idx);
                }
            }
        }
        KeyCode::Enter => {
            let (key, fid, new_val) = if let ActionState::SelectingFieldOptions {
                ref issue_key,
                ref field_id,
                ref original_json,
                ref options,
                ref selected,
                ..
            } = app.action_state
            {
                let nv = shape_array_value(options, selected, original_json);
                (issue_key.clone(), field_id.clone(), nv)
            } else {
                return;
            };
            app.action_state = ActionState::CommittingFieldEdit {
                issue_key: key,
                field_id: fid,
                new_value: new_val,
            };
        }
        _ => {}
    }
}

#[allow(clippy::needless_pass_by_value)]
fn handle_datetime_picker_input(app: &mut AppState, event: crossterm::event::Event) {
    use crate::tui::overlays::datetime_picker::{
        DatetimePickerMode, handle_date_key, handle_time_key,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent};

    let Event::Key(KeyEvent { code, .. }) = event else {
        return;
    };

    match code {
        KeyCode::Esc | KeyCode::Char('q') => {
            let ActionState::EditingDatetimeField { ref mut picker, .. } = app.action_state else {
                return;
            };
            if picker.mode == DatetimePickerMode::Time {
                if picker.time_focus == crate::tui::overlays::datetime_picker::TimeFocus::Minute {
                    picker.time_focus = crate::tui::overlays::datetime_picker::TimeFocus::Hour;
                } else {
                    picker.mode = DatetimePickerMode::Date;
                }
            } else {
                app.action_state = ActionState::None;
            }
            return;
        }
        KeyCode::Enter => {
            let ActionState::EditingDatetimeField {
                ref issue_key,
                ref field_id,
                ref mut picker,
                ..
            } = app.action_state
            else {
                return;
            };
            // Date mode → switch to Time; Time/Hour → advance to Minute; Time/Minute → commit.
            if picker.mode == DatetimePickerMode::Date {
                picker.mode = DatetimePickerMode::Time;
                return;
            }
            if picker.time_focus == crate::tui::overlays::datetime_picker::TimeFocus::Hour {
                picker.time_focus = crate::tui::overlays::datetime_picker::TimeFocus::Minute;
                return;
            }
            let (key, fid, iso) = (issue_key.clone(), field_id.clone(), picker.to_iso_string());
            app.action_state = ActionState::CommittingFieldEdit {
                issue_key: key,
                field_id: fid,
                new_value: serde_json::Value::String(iso),
            };
            return;
        }
        _ => {}
    }

    // Mutate picker in-place for navigation keys
    let ActionState::EditingDatetimeField { ref mut picker, .. } = app.action_state else {
        return;
    };
    let mode = picker.mode.clone();
    match mode {
        DatetimePickerMode::Date => handle_date_key(picker, code),
        DatetimePickerMode::Time => handle_time_key(picker, code),
    }
}

fn shape_array_value(
    options: &[FieldOption],
    selected: &HashSet<usize>,
    original: &serde_json::Value,
) -> serde_json::Value {
    let use_object_shape = original
        .as_array()
        .and_then(|a| a.first())
        .is_some_and(serde_json::Value::is_object);

    let items: Vec<serde_json::Value> = options
        .iter()
        .enumerate()
        .filter(|(i, _)| selected.contains(i))
        .map(|(_, opt)| {
            if use_object_shape {
                serde_json::json!({ "value": opt.value })
            } else {
                serde_json::Value::String(opt.value.clone())
            }
        })
        .collect();

    serde_json::Value::Array(items)
}

/// Compute filesystem path completions for the given partial path.
/// Expands a leading `~/` (or bare `~`) to the home directory.
/// Returns full absolute paths; directories are suffixed with `/`.
/// Results are sorted: directories first, then files, each group alphabetically.
pub fn compute_completions_for(path: &str) -> Vec<String> {
    // Tilde expansion
    let expanded: String = path.strip_prefix("~/").map_or_else(
        || {
            if path == "~" {
                dirs::home_dir()
                    .map_or_else(|| path.to_string(), |h| h.to_string_lossy().to_string())
            } else {
                path.to_string()
            }
        },
        |rest| {
            dirs::home_dir().map_or_else(|| path.to_string(), |h| format!("{}/{rest}", h.display()))
        },
    );

    // Split at last '/' to get (dir_part, prefix)
    let (dir_str, prefix): (String, String) = if expanded.ends_with('/') {
        let d = expanded.trim_end_matches('/');
        let d = if d.is_empty() { "/" } else { d };
        (d.to_string(), String::new())
    } else if let Some(pos) = expanded.rfind('/') {
        let d = &expanded[..pos];
        let d = if d.is_empty() { "/" } else { d };
        (d.to_string(), expanded[pos + 1..].to_string())
    } else {
        (".".to_string(), expanded)
    };

    let dir_path = std::path::Path::new(&dir_str);
    let Ok(entries) = std::fs::read_dir(dir_path) else {
        return vec![];
    };

    let mut dirs_vec: Vec<String> = vec![];
    let mut files_vec: Vec<String> = vec![];

    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !file_name.starts_with(prefix.as_str()) {
            continue;
        }
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        let full_path = dir_path.join(&file_name);
        let full = if is_dir {
            format!("{}/", full_path.display())
        } else {
            full_path.display().to_string()
        };
        if is_dir {
            dirs_vec.push(full);
        } else {
            files_vec.push(full);
        }
    }

    dirs_vec.sort();
    files_vec.sort();
    dirs_vec.extend(files_vec);
    dirs_vec
}
