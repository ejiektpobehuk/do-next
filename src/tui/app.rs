use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use indexmap::IndexMap;

use crate::config::types::{Config, SourceConfig};
use crate::events::{ActionResult, AppEvent};
use crate::jira::types::{FieldOption, Issue};

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

impl SourceState {
    pub const fn is_terminal(&self) -> bool {
        matches!(self, Self::Loaded(_) | Self::Error(_))
    }
}

/// Which panel has keyboard focus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusedPanel {
    List,
    Detail,
}

/// Which view mode to use for the detail panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewMode {
    Default,
    Incident,
    Postmortem,
    Review,
    Comments,
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

pub struct AppState {
    pub config: Config,
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
    /// Spinner frame counter (incremented on each Tick).
    pub tick_count: u64,
    pub current_user: Option<String>,
    /// Scroll offset for the detail panel (rows).
    pub detail_scroll: usize,
    /// Which panel currently has keyboard focus.
    pub focused_panel: FocusedPanel,
    /// Index of the focused editable field when `ViewMode::Postmortem` && `FocusedPanel::Detail`.
    pub postmortem_field_idx: usize,
    /// Virtual (top, bottom) row for each editable postmortem field; written each render.
    pub postmortem_field_offsets: Vec<(usize, usize)>,
    /// Height of the detail content viewport; written each render.
    pub last_detail_viewport_h: usize,
    /// API-fetched display names for postmortem fields: `field_id` → name.
    pub postmortem_field_names: HashMap<String, String>,
    /// API-fetched Jira schema types for postmortem fields: `field_id` → type string.
    pub postmortem_field_schemas: HashMap<String, String>,
    /// Set while a field-names fetch is in flight to prevent duplicate requests.
    pub postmortem_field_names_loading: bool,
    /// Tracks first `g` press for `gg` (jump to first) motion.
    pub pending_g: bool,
    /// Total content lines of the detail view; written each render.
    pub last_detail_content_h: usize,
}

impl AppState {
    pub fn new(config: Config) -> Self {
        let sources = config
            .sources
            .iter()
            .map(|s| (s.id.clone(), SourceState::Pending))
            .collect();
        Self {
            config,
            sources,
            issues: Vec::new(),
            subsource_errors: IndexMap::new(),
            nav_items: Vec::new(),
            nav_idx: 0,
            view_mode: ViewMode::Default,
            action_state: ActionState::None,
            should_quit: false,
            tick_count: 0,
            current_user: None,
            detail_scroll: 0,
            focused_panel: FocusedPanel::List,
            postmortem_field_idx: 0,
            postmortem_field_offsets: Vec::new(),
            last_detail_viewport_h: 0,
            postmortem_field_names: HashMap::new(),
            postmortem_field_schemas: HashMap::new(),
            postmortem_field_names_loading: false,
            pending_g: false,
            last_detail_content_h: 0,
        }
    }

    pub fn all_sources_terminal(&self) -> bool {
        self.sources.values().all(SourceState::is_terminal)
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
pub fn source_config_for<'a>(config: &'a Config, id: &str) -> Option<&'a SourceConfig> {
    config.sources.iter().find(|s| s.id == id)
}

/// Determine the auto view mode for an issue based on its source config.
fn auto_view_mode(issue: &Issue, config: &Config) -> ViewMode {
    let Some(source_id) = issue.source_id.as_deref() else {
        return ViewMode::Default;
    };
    match source_config_for(config, source_id).and_then(|s| s.view_mode.as_deref()) {
        Some("incident") => ViewMode::Incident,
        Some("postmortem") => ViewMode::Postmortem,
        Some("review") => ViewMode::Review,
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
                let mode = auto_view_mode(&issue, &app.config);
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
    }
}

fn handle_action_done(app: &mut AppState, result: ActionResult) {
    match result {
        ActionResult::Error(e) => {
            app.action_state = ActionState::Error(Arc::new(e));
        }
        ActionResult::Hidden { ref issue_key } => {
            app.issues.retain(|i| &i.key != issue_key);
            app.rebuild_nav();
            app.action_state = ActionState::None;
        }
        ActionResult::TransitionApplied {
            ref issue_key,
            ref new_status,
        } => {
            if let Some(issue) = app.issues.iter_mut().find(|i| &i.key == issue_key) {
                issue.fields.status.name.clone_from(new_status);
            }
            app.action_state = ActionState::None;
        }
        ActionResult::TransitionsLoaded {
            issue_key,
            transitions,
        } => {
            app.action_state = ActionState::SelectingTransition {
                issue_key,
                transitions,
                selected: 0,
            };
        }
        ActionResult::AssignedToMe { ref issue_key } => {
            // Mark assignee as current user in the list (best-effort display update)
            if let Some(ref me) = app.current_user.clone()
                && let Some(issue) = app.issues.iter_mut().find(|i| &i.key == issue_key)
            {
                issue.fields.assignee = Some(crate::jira::types::UserField {
                    name: me.clone(),
                    display_name: Some(me.clone()),
                    account_id: None,
                });
            }
            app.action_state = ActionState::None;
        }
        ActionResult::MovedToProject {
            ref issue_key,
            ref project,
        } => {
            if let Some(issue) = app.issues.iter_mut().find(|i| &i.key == issue_key) {
                issue.fields.project.key.clone_from(project);
            }
            app.action_state = ActionState::None;
        }
        ActionResult::CommentPosted { .. } => {
            app.action_state = ActionState::None;
        }
        ActionResult::FieldUpdated {
            ref issue_key,
            ref field_id,
            ref new_value,
        } => {
            // Update in-memory field value immediately (no re-fetch needed)
            if let Some(issue) = app.issues.iter_mut().find(|i| &i.key == issue_key) {
                issue
                    .fields
                    .extra
                    .insert(field_id.clone(), new_value.clone());
            }
            app.action_state = ActionState::None;
        }
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
        ActionResult::PostmortemFieldNamesLoaded { names, schemas } => {
            app.postmortem_field_names.extend(names);
            app.postmortem_field_schemas.extend(schemas);
            app.postmortem_field_names_loading = false;
        }
    }
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
        let current_value = crate::tui::views::postmortem::val_to_str(&original_json);
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

fn handle_input(app: &mut AppState, event: crossterm::event::Event) {
    use crossterm::event::{Event, KeyEvent};

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
        | ActionState::CommittingFieldEdit { .. } => {
            // Ignore input while waiting
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
        if app.pending_g {
            app.pending_g = false;
            key_jump_first(app);
        } else {
            app.pending_g = true;
        }
        return;
    }
    app.pending_g = false;

    match (code, modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        (KeyCode::Left | KeyCode::Char('h'), _) => {
            app.focused_panel = FocusedPanel::List;
        }
        (KeyCode::Right | KeyCode::Char('l'), _) => {
            app.focused_panel = FocusedPanel::Detail;
        }
        (KeyCode::Down | KeyCode::Char('j'), _) => {
            if app.focused_panel == FocusedPanel::Detail && app.view_mode == ViewMode::Postmortem {
                let max_idx = crate::tui::views::postmortem::num_postmortem_fields(
                    app.config.view_modes.postmortem.as_ref(),
                )
                .saturating_sub(1);
                if app.postmortem_field_idx < max_idx {
                    app.postmortem_field_idx += 1;
                }
                auto_scroll_to_field(app);
            } else if app.focused_panel == FocusedPanel::Detail {
                app.detail_scroll = app.detail_scroll.saturating_add(1);
            } else if !app.nav_items.is_empty() {
                app.nav_idx = (app.nav_idx + 1).min(app.nav_items.len() - 1);
                update_view_mode_on_navigate(app);
            }
        }
        (KeyCode::Up | KeyCode::Char('k'), _) => {
            if app.focused_panel == FocusedPanel::Detail && app.view_mode == ViewMode::Postmortem {
                if app.postmortem_field_idx > 0 {
                    app.postmortem_field_idx -= 1;
                }
                auto_scroll_to_field(app);
            } else if app.focused_panel == FocusedPanel::Detail {
                app.detail_scroll = app.detail_scroll.saturating_sub(1);
            } else if app.nav_idx > 0 {
                app.nav_idx -= 1;
                update_view_mode_on_navigate(app);
            }
        }
        (KeyCode::Enter, _) => {
            if app.focused_panel == FocusedPanel::Detail && app.view_mode == ViewMode::Postmortem {
                key_edit_postmortem_field(app);
            }
        }
        (KeyCode::Char('v'), _) => {
            // Cycle view modes manually
            app.view_mode = match app.view_mode {
                ViewMode::Default
                | ViewMode::Incident
                | ViewMode::Postmortem
                | ViewMode::Review => ViewMode::Comments,
                ViewMode::Comments => ViewMode::Default,
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
                let url = format!("{}/browse/{}", app.config.jira.base_url, issue.key);
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

fn key_jump_first(app: &mut AppState) {
    if app.focused_panel == FocusedPanel::Detail {
        if app.view_mode == ViewMode::Postmortem {
            app.postmortem_field_idx = 0;
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
        if app.view_mode == ViewMode::Postmortem {
            let max_idx = crate::tui::views::postmortem::num_postmortem_fields(
                app.config.view_modes.postmortem.as_ref(),
            )
            .saturating_sub(1);
            app.postmortem_field_idx = max_idx;
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
            .and_then(|id| source_config_for(&app.config, id))
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
            .and_then(|id| source_config_for(&app.config, id))
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
            .and_then(|id| source_config_for(&app.config, id))
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
        app.view_mode = auto_view_mode(&issue, &app.config);
    }
    app.detail_scroll = 0;
    app.postmortem_field_idx = 0;
    app.postmortem_field_offsets.clear();
    app.postmortem_field_names.clear();
    app.postmortem_field_schemas.clear();
    app.postmortem_field_names_loading = false;
}

fn key_edit_postmortem_field(app: &mut AppState) {
    let Some(issue) = app.selected_issue() else {
        return;
    };
    let issue = issue.clone();
    let field_idx = app.postmortem_field_idx;
    let cfg = app.config.view_modes.postmortem.as_ref();
    let (field_id, original_json) =
        crate::tui::views::postmortem::postmortem_editable_field_spec(cfg, &issue, field_idx);

    if field_id.is_empty() {
        return;
    }

    // Readonly fields: open URL in browser if the value is a link, otherwise do nothing
    if crate::tui::views::postmortem::postmortem_field_is_readonly(cfg, field_idx) {
        if let serde_json::Value::String(s) = &original_json
            && (s.starts_with("http://") || s.starts_with("https://"))
        {
            let _ = open::that(s.clone());
        }
        return;
    }

    let label = crate::tui::views::postmortem::postmortem_field_cfg(cfg, field_idx)
        .map(|f| crate::tui::views::postmortem::resolve_field_label(f, &app.postmortem_field_names))
        .unwrap_or_default();
    let description = crate::tui::views::postmortem::postmortem_field_hint(cfg, field_idx);

    // `use_editor: true` always opens $EDITOR regardless of field type
    let use_editor = crate::tui::views::postmortem::postmortem_field_cfg(cfg, field_idx)
        .and_then(|f| f.use_editor)
        .unwrap_or(false);

    // Datetime picker: triggered by `datetime: true` config flag or editmeta schema type
    if !use_editor {
        let by_config = crate::tui::views::postmortem::postmortem_field_cfg(cfg, field_idx)
            .and_then(|f| f.datetime)
            .unwrap_or(false);
        let by_schema = app
            .postmortem_field_schemas
            .get(&field_id)
            .is_some_and(|t| t == "date" || t == "datetime");
        if by_config || by_schema {
            let tz = crate::tui::views::postmortem::resolve_tz(cfg);
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

    if use_editor {
        let current_value = crate::tui::views::postmortem::val_to_str(&original_json);
        app.action_state = ActionState::PendingFieldEdit {
            issue_key: issue.key,
            field_id,
            current_value,
            original_json,
        };
        return;
    }

    set_postmortem_edit_state(
        app,
        issue.key,
        field_id,
        field_idx,
        label,
        description,
        original_json,
    );
}

fn set_postmortem_edit_state(
    app: &mut AppState,
    issue_key: String,
    field_id: String,
    field_idx: usize,
    label: String,
    description: Option<String>,
    original_json: serde_json::Value,
) {
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
            let current_value = crate::tui::views::postmortem::val_to_str(&original_json);
            app.action_state = ActionState::PendingFieldEdit {
                issue_key,
                field_id,
                current_value,
                original_json,
            };
        }
        _ => {
            let input = crate::tui::views::postmortem::val_to_str(&original_json);
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

fn auto_scroll_to_field(app: &mut AppState) {
    let idx = app.postmortem_field_idx;
    let Some(&(top, bottom)) = app.postmortem_field_offsets.get(idx) else {
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
    let solutions_len = app.config.hide_for_a_day.suggested_solutions.len();

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
