use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use indexmap::IndexMap;

use crate::config::types::{ResolvedTeam, SourceConfig, TeamConfig};
use crate::events::{ActionResult, AppEvent};
use crate::items::WorkItem;
use crate::jira::types::{
    BoardConfiguration, Comment, FieldOption, FieldSchema, Issue, ProjectInfo, StatusInfo,
};
use crate::tui::search::{RankedHit, SearchFilters};

/// Per-team state that is saved/restored when switching tabs.
#[derive(Debug, Clone)]
pub struct PerTeamState {
    pub sources: IndexMap<String, SourceState>,
    pub issues: Vec<WorkItem>,
    pub subsource_errors: IndexMap<String, Vec<(usize, Arc<anyhow::Error>)>>,
    pub nav_items: Vec<NavItem>,
    pub nav_idx: usize,
    pub field_names: HashMap<String, String>,
    pub field_schemas: HashMap<String, FieldSchema>,
    pub field_names_state: FieldNamesState,
    pub board_configs: HashMap<String, BoardConfiguration>,
    pub board_lanes: HashMap<String, LanesState>,
}

/// Loading state for a board source's resolved query-swimlanes.
#[derive(Debug, Clone)]
pub enum LanesState {
    Loading,
    Loaded(crate::jira::types::BoardSwimlanes),
    Error(String),
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

/// Loading state for a single work-item source.
#[derive(Debug, Clone)]
pub enum SourceState {
    Pending,
    Loading,
    Loaded(Vec<WorkItem>),
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
    /// Board-mode move-card picker: the board's columns annotated with the
    /// transition (if any) that reaches each one. `transitions` is kept so
    /// `t` inside the picker can swap to the raw transition list refetch-free.
    SelectingBoardColumn {
        issue_key: String,
        transitions: Vec<crate::jira::types::Transition>,
        columns: Vec<crate::tui::board::BoardColumnChoice>,
        selected: usize,
    },
    LoadingTransitions {
        issue_key: String,
    },
    /// Backlog send-to-sprint: waiting for the board's sprint list.
    LoadingSprints {
        issue_key: String,
    },
    /// Backlog send-to-sprint picker: the board's active and future sprints.
    SelectingSprint {
        issue_key: String,
        sprints: Vec<crate::jira::types::Sprint>,
        selected: usize,
    },
    /// Confirmed sprint choice, waiting for dispatch.
    PendingMoveToSprint {
        issue_key: String,
        sprint_id: u64,
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
        /// Vertical scroll offset within the active tab.
        scroll: u16,
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
        /// Vertical scroll offset within the active tab.
        scroll: u16,
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
    /// Yes/No popup confirming marking a Confluence task complete.
    /// `selected` 0=Yes 1=No.
    ConfirmingCompleteTask {
        item_key: String,
        task_id: String,
        /// Default 1 (No) for safety.
        selected: usize,
    },
    /// Confluence complete-task call ready to be dispatched.
    PendingCompleteTask {
        item_key: String,
        task_id: String,
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
    Error {
        error: Arc<anyhow::Error>,
        scroll: u16,
    },
    /// Keybindings reference overlay.
    KeybindingsHelp,
    /// Create-issue form overlay.
    CreatingIssue(crate::tui::overlays::create_issue::CreateForm),
    /// New-issue create request ready to dispatch (`payload` is the full
    /// `{ "fields": { … } }` object). Transient, like `CommittingFieldEdit`.
    /// Carries the form so a server-side failure can restore it.
    CommittingCreate {
        payload: serde_json::Value,
        form: crate::tui::overlays::create_issue::CreateForm,
    },
    /// Create request in flight. Unlike the generic `AwaitingAction`, keeps
    /// the form so an error returns to it instead of discarding the draft.
    AwaitingCreate {
        form: crate::tui::overlays::create_issue::CreateForm,
    },
    /// Confirmation popup shown after a successful create.
    IssueCreatedConfirm {
        key: String,
    },
    /// Telescope-style search popup: text input + chip filters + result list +
    /// live preview pane.
    Searching {
        query: String,
        cursor: usize,
        filters: SearchFilters,
        focus: SearchFocus,
        local_results: Vec<RankedHit>,
        jira_state: JiraSearchState,
        /// Index in the merged result list of the currently highlighted hit.
        selected: usize,
        /// Selected nav index before opening search; restored on Esc.
        prev_nav_idx: usize,
        /// Incremented on every query/filter change; lets stale Jira responses be discarded.
        debounce_token: u64,
        /// Instant when query/filters last changed. The background dispatcher
        /// spawns the Jira search 250ms after the latest change.
        last_change_at: std::time::Instant,
        /// True once the Jira spawn for the current `debounce_token` has fired.
        jira_spawned_for_token: bool,
        /// Active filter picker overlay, if any.
        picker: Option<FilterPicker>,
    },
}

/// Which sub-widget inside the search popup currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchFocus {
    Input,
    StatusSlot,
    ProjectSlot,
    Result(usize),
}

/// Which filter the active picker is editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    Status,
    Project,
}

/// Section of the picker an item belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerSection {
    Team,
    Other,
}

/// Tri-state choice for a filter picker row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterChoice {
    Include,
    Exclude,
}

impl FilterChoice {
    /// Cycle: None → Include → Exclude → None.
    pub const fn next(current: Option<Self>) -> Option<Self> {
        match current {
            None => Some(Self::Include),
            Some(Self::Include) => Some(Self::Exclude),
            Some(Self::Exclude) => None,
        }
    }
}

/// A single selectable row in the filter picker.
#[derive(Debug, Clone)]
pub struct PickerItem {
    pub section: PickerSection,
    /// Canonical value used for matching/JQL (status name or project key).
    pub value: String,
    /// Display label (status name, or "KEY  Name" for projects).
    pub label: String,
}

/// State for the popup that edits a single filter.
#[derive(Debug, Clone)]
pub struct FilterPicker {
    pub kind: FilterKind,
    pub query: String,
    pub query_cursor: usize,
    pub items: Vec<PickerItem>,
    /// Cursor index into the filtered/visible items.
    pub cursor: usize,
    /// Map of values to their tri-state choice. Absent = unselected.
    /// `FilterKind::Project` only ever stores `FilterChoice::Include`.
    pub selected: HashMap<String, FilterChoice>,
    pub loading: bool,
}

/// Lifecycle of a background-fetched cache.
#[derive(Debug, Clone, Default)]
pub enum CacheState<T> {
    #[default]
    Idle,
    Loading,
    Loaded(T),
    #[allow(dead_code)]
    Failed(String),
}

impl<T> CacheState<T> {
    pub const fn loaded(&self) -> Option<&T> {
        match self {
            Self::Loaded(v) => Some(v),
            _ => None,
        }
    }
    pub const fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }
    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::Idle | Self::Loading)
    }
}

/// Async state of the parallel Jira-side search.
#[derive(Debug, Clone)]
pub enum JiraSearchState {
    Idle,
    Pending {
        /// Debounce token of the request in flight; lets stale responses be discarded.
        #[allow(dead_code)]
        token: u64,
    },
    Loaded {
        hits: Vec<RankedHit>,
        issues: Vec<Issue>,
    },
    Error(String),
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

#[allow(clippy::struct_excessive_bools)]
pub struct AppState {
    pub resolved_teams: Vec<ResolvedTeam>,
    pub active_team_idx: usize,
    /// Saved per-team state for inactive tabs.
    pub saved_team_states: HashMap<usize, PerTeamState>,
    pub sources: IndexMap<String, SourceState>,
    /// Flat ordered list of all visible items (after dedup).
    pub issues: Vec<WorkItem>,
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
    /// API-fetched Jira schema info for fields: `field_id` → schema.
    pub field_schemas: HashMap<String, FieldSchema>,
    /// Total content lines of the detail view; written each render.
    pub last_detail_content_h: usize,
    /// Content height of the active confirm overlay (field/comment edit); written each render.
    pub last_confirm_content_h: usize,
    /// Viewport height of the active confirm overlay; written each render.
    pub last_confirm_viewport_h: usize,
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
    /// Set by `r`/`Shift+R` when the user requests a full team refresh.
    pub pending_refresh_all: bool,
    /// Set by `P` to preload full detail for every partial (board-trimmed)
    /// issue; consumed by the main loop dispatcher.
    pub pending_preload: bool,
    /// Set by `r` on a focused issue; consumed by the main loop dispatcher.
    pub pending_refresh_issue: Option<RefreshIssueRequest>,
    /// Issue keys with a refresh currently in flight (drives spinner + de-dupes presses).
    pub refreshing_issues: HashSet<String>,
    /// Terminal image protocol picker (created once at startup).
    pub image_picker: Option<ratatui_image::picker::Picker>,
    /// When true, hide the list panel and render the detail view full-width.
    /// Toggled off by `h` or `Esc` in normal mode. Set when opening an issue
    /// from the search popup.
    pub fullscreen_detail: bool,
    /// The `Searching` state that opened the current fullscreen detail, parked
    /// so `q`/`Esc` can restore the search overlay with its prior results,
    /// filters, and selection. `Some` only while a search-originated detail is
    /// open; cleared whenever that detail is left by any other path.
    pub saved_search: Option<Box<ActionState>>,
    /// Distinct status names from the active team projects' workflows.
    pub status_team_cache: CacheState<Vec<String>>,
    /// All statuses configured on this Jira instance (used for the picker's
    /// "Other" section). Each entry carries its `statusCategory` so terminal
    /// statuses can bubble to the top.
    pub status_all_cache: CacheState<Vec<StatusInfo>>,
    /// Cached visible Jira projects (first page of `/project/search`).
    pub project_cache: CacheState<Vec<ProjectInfo>>,
    /// Set when search opens (or the status picker opens) and the team-status
    /// cache is still cold; consumed by the background dispatcher.
    pub pending_team_status_fetch: bool,
    /// Set when search opens (or the status picker opens) and the all-status
    /// cache is still cold; consumed by the background dispatcher.
    pub pending_all_statuses_fetch: bool,
    /// Set when search opens (or the project picker opens) and the project
    /// cache is still cold; consumed by the background dispatcher.
    pub pending_projects_fetch: bool,
    /// Column configuration per board source (`source_id` → config).
    pub board_configs: HashMap<String, BoardConfiguration>,
    /// Resolved query-swimlanes per board source (`source_id` → state).
    pub board_lanes: HashMap<String, LanesState>,
    /// Some(`source_id`) while a dedicated-tab source (board or backlog)
    /// covers the main area. Selection stays in `nav_idx`; on board tabs the
    /// board cursor is derived from it.
    pub board_view: Option<String>,
    /// The latest optimistic backlog reorder, waiting out its debounce window
    /// before being sent to Jira. Consecutive moves of the same issue
    /// collapse into it. Not team-scoped: it is self-contained and survives
    /// tab/team switches via `rank_flush_queue`.
    pub pending_rank: Option<crate::tui::backlog::PendingRank>,
    /// Rank mutations that must dispatch on the next loop iteration without
    /// waiting for the debounce (a different issue started moving, or a
    /// team/tab switch displaced `pending_rank`).
    pub rank_flush_queue: Vec<crate::tui::backlog::PendingRank>,
    /// Set when a rank mutation failed: the backlog source to refetch so the
    /// list falls back to the server's order.
    pub pending_rank_refetch: Option<String>,
    /// Global default board detail-load mode (per-board config overrides it).
    pub detail_load: crate::config::types::DetailLoad,
    /// On-disk source cache settings (stale-while-revalidate).
    pub cache: crate::config::types::CacheConfig,
}

/// Request for a silent background attachment fetch.
pub struct AttachmentFetchRequest {
    pub attachment_id: String,
    pub content_url: String,
    pub filename: String,
    pub issue_key: String,
}

/// Request for a single-issue background refresh.
pub struct RefreshIssueRequest {
    pub key: String,
    pub source_id: Option<String>,
    pub subsource_idx: usize,
}

impl AppState {
    pub fn new(resolved_teams: Vec<ResolvedTeam>, config: &crate::config::types::Config) -> Self {
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
        // A board-only first team starts on its board, not the (absent) list tab.
        let board_view = resolved_teams.first().and_then(Self::default_view_for);
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
            last_confirm_content_h: 0,
            last_confirm_viewport_h: 0,
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
            pending_refresh_all: false,
            pending_preload: false,
            pending_refresh_issue: None,
            refreshing_issues: HashSet::new(),
            image_picker: None,
            fullscreen_detail: false,
            saved_search: None,
            status_team_cache: CacheState::default(),
            status_all_cache: CacheState::default(),
            project_cache: CacheState::default(),
            pending_team_status_fetch: false,
            pending_all_statuses_fetch: false,
            pending_projects_fetch: false,
            board_configs: HashMap::new(),
            board_lanes: HashMap::new(),
            board_view,
            pending_rank: None,
            rank_flush_queue: Vec::new(),
            pending_rank_refetch: None,
            detail_load: config.detail_load,
            cache: config.cache.clone(),
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
            board_configs: std::mem::take(&mut self.board_configs),
            board_lanes: std::mem::take(&mut self.board_lanes),
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
            self.board_configs = saved.board_configs;
            self.board_lanes = saved.board_lanes;
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
            self.board_configs = HashMap::new();
            self.board_lanes = HashMap::new();
        }

        // Reset UI state for the new tab
        self.detail_scroll = 0;
        self.view_mode = ViewMode::Default;
        // Filter caches are team-scoped; drop them so the next picker open
        // re-fetches for the new team.
        self.status_team_cache = CacheState::default();
        self.status_all_cache = CacheState::default();
        self.project_cache = CacheState::default();
        self.pending_team_status_fetch = false;
        self.pending_all_statuses_fetch = false;
        self.pending_projects_fetch = false;
        self.focused_panel = FocusedPanel::List;
        self.action_state = ActionState::None;
        self.overlay = None;
        self.fullscreen_detail = false;
        self.saved_search = None;
        // Board-only teams have no list tab; land on their first board.
        self.board_view = Self::default_view_for(&self.resolved_teams[new_idx]);
        // Drop any in-flight refresh signals; the running tasks will still
        // send events, but the handler tolerates unknown keys.
        self.refreshing_issues.clear();
        self.pending_refresh_all = false;
        self.pending_preload = false;
        self.pending_refresh_issue = None;
        // A pending rank move must still reach Jira — its anchor was captured
        // at keypress, so it dispatches as-is without the old team's state.
        if let Some(pending) = self.pending_rank.take() {
            self.rank_flush_queue.push(pending);
        }

        // Recompute the flat list from the (correct, full) restored sources for
        // the team-list tab. The saved `issues`/`nav_items` may have been
        // captured while a board tab was active (board-filtered), so we can't
        // trust them; `sources` is the source of truth.
        self.rebuild_issues();

        // Trigger source fetches if any sources are still pending
        if self
            .sources
            .values()
            .any(|s| matches!(s, SourceState::Pending))
        {
            self.flags.pending_team_fetch = true;
        }
    }

    /// Whether a source kind renders as its own dedicated tab (rather than
    /// as rows in the team's flat list).
    pub(crate) const fn is_dedicated_tab_kind(kind: crate::config::types::SourceKind) -> bool {
        matches!(
            kind,
            crate::config::types::SourceKind::Board | crate::config::types::SourceKind::Backlog
        )
    }

    /// The kind of the active dedicated-tab source, `None` on a list tab.
    pub(crate) fn active_tab_source_kind(&self) -> Option<crate::config::types::SourceKind> {
        let id = self.board_view.as_deref()?;
        source_config_for(self.team_config(), id).map(|s| s.kind)
    }

    /// The default view for a team's tab: `None` (the flat list) normally,
    /// or the first source for a team with only dedicated-tab sources
    /// (boards/backlogs) — such teams get no list tab (it would always be
    /// empty).
    fn default_view_for(team: &ResolvedTeam) -> Option<String> {
        if team
            .config
            .sources
            .iter()
            .any(|s| !Self::is_dedicated_tab_kind(s.kind))
        {
            return None;
        }
        team.config.sources.first().map(|s| s.id.clone())
    }

    /// The ordered tab list. Each team contributes a list tab (unless all its
    /// sources are boards/backlogs), followed by one tab per board or backlog
    /// source it defines. A tab is identified by
    /// `(team_idx, Option<source_id>)`; `None` is the team's list view.
    pub fn tab_list(&self) -> Vec<(usize, Option<String>)> {
        let mut tabs = Vec::new();
        for (i, team) in self.resolved_teams.iter().enumerate() {
            if Self::default_view_for(team).is_none() {
                tabs.push((i, None));
            }
            for src in &team.config.sources {
                if Self::is_dedicated_tab_kind(src.kind) {
                    tabs.push((i, Some(src.id.clone())));
                }
            }
        }
        tabs
    }

    /// Index of the active tab within `tab_list()`.
    pub fn active_tab_index(&self) -> usize {
        self.tab_list()
            .iter()
            .position(|(t, b)| *t == self.active_team_idx && *b == self.board_view)
            .unwrap_or(0)
    }

    /// Activate the tab at `idx` in `tab_list()` (team-list or board view).
    pub fn activate_tab(&mut self, idx: usize) {
        let Some((team_idx, board)) = self.tab_list().get(idx).cloned() else {
            return;
        };
        if team_idx != self.active_team_idx {
            // switch_team resets board_view to the team's default and rebuilds.
            self.switch_team(team_idx);
        }
        if self.board_view != board {
            self.board_view = board;
            self.fullscreen_detail = false;
            self.saved_search = None;
            self.detail_scroll = 0;
            self.nav_idx = 0;
            self.rebuild_issues();
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

    /// True iff any popup (sub-view or action overlay) is rendered on top of the main panels.
    /// Inline editing states are NOT popups.
    pub const fn popup_active(&self) -> bool {
        self.overlay.is_some() || self.action_popup_active()
    }

    /// True iff an action overlay is rendered on top (excludes inline edit states).
    pub const fn action_popup_active(&self) -> bool {
        !matches!(
            self.action_state,
            ActionState::None
                | ActionState::InlineEditingField { .. }
                | ActionState::TypingAttachmentPath { .. },
        )
    }

    pub fn selected_item(&self) -> Option<&WorkItem> {
        match self.nav_items.get(self.nav_idx)? {
            NavItem::Issue(idx) => self.issues.get(*idx),
            NavItem::SourceError(_) | NavItem::SubsourceError(_, _) => None,
        }
    }

    /// The selected item's Jira payload; `None` on error rows and non-Jira
    /// items. Jira-only flows (transitions, comments, attachments, editmeta)
    /// gate through this.
    pub fn selected_issue(&self) -> Option<&Issue> {
        self.selected_item().and_then(WorkItem::as_jira)
    }

    /// Mutable access to a Jira issue in the flat list by key. Used by the
    /// `apply_*` mutators, which are only ever reached for Jira items.
    fn jira_issue_mut(&mut self, key: &str) -> Option<&mut Issue> {
        self.issues
            .iter_mut()
            .find(|i| i.key() == key)
            .and_then(WorkItem::as_jira_mut)
    }

    pub fn selected_nav_item(&self) -> Option<&NavItem> {
        self.nav_items.get(self.nav_idx)
    }

    /// Whether a source's items belong in the currently active tab.
    ///
    /// Board and backlog sources are their own tabs, never part of a team's
    /// flat list: on a team-list tab (`board_view == None`) all dedicated-tab
    /// sources are excluded; on a board/backlog tab only that one source is
    /// included.
    pub(crate) fn source_in_active_tab(&self, source_id: &str) -> bool {
        let is_dedicated = source_config_for(self.team_config(), source_id)
            .is_some_and(|s| Self::is_dedicated_tab_kind(s.kind));
        self.board_view
            .as_deref()
            .map_or(!is_dedicated, |active| source_id == active)
    }

    /// Rebuild the flat items list from loaded source states (in priority order, deduped),
    /// then rebuild navigable items.
    pub(crate) fn rebuild_issues(&mut self) {
        let mut seen = std::collections::HashSet::new();
        let mut issues = Vec::new();
        for (source_id, state) in &self.sources {
            if !self.source_in_active_tab(source_id) {
                continue;
            }
            if let SourceState::Loaded(source_items) = state {
                for item in source_items {
                    if seen.insert(item.key().to_owned()) {
                        issues.push(item.clone());
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
            if !self.source_in_active_tab(source_id) {
                continue;
            }
            match state {
                SourceState::Loaded(_) => {
                    let start = issue_pos;
                    while issue_pos < self.issues.len()
                        && self.issues[issue_pos].source_id() == Some(source_id.as_str())
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
                    if let Some(key) = self.issues.get(old_idx).map(|i| i.key().to_owned())
                        && let Some(pos) = self.nav_items.iter().position(|n| {
                            matches!(n, NavItem::Issue(i) if self.issues.get(*i).map(WorkItem::key) == Some(key.as_str()))
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

/// Determine the auto view mode for an item based on its source config.
fn auto_view_mode(item: &WorkItem, team_config: &TeamConfig) -> ViewMode {
    let Some(source_id) = item.source_id() else {
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

        AppEvent::SourceLoaded(source_id, items) => {
            app.sources.insert(source_id, SourceState::Loaded(items));
            app.rebuild_issues();
            // Auto-update view mode for newly selected item
            if let Some(item) = app.selected_item() {
                let item = item.clone();
                let mode = auto_view_mode(&item, app.team_config());
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

        AppEvent::BoardConfigLoaded(source_id, config) => {
            apply_board_config_loaded(app, source_id, config);
        }

        AppEvent::BoardLanesLoaded(source_id, result) => {
            let state = match result {
                Ok(lanes) => LanesState::Loaded(lanes),
                Err(e) => LanesState::Error(format!("{e:#}")),
            };
            app.board_lanes.insert(source_id, state);
            // No rebuild: lanes only affect board-view grouping, derived per frame.
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

        AppEvent::IssueRanked { source_id, result } => apply_issue_ranked(app, source_id, result),

        AppEvent::IssueRefreshed(item) => {
            let item = *item;
            app.refreshing_issues.remove(item.key());
            apply_issue_refresh(&mut app.issues, &mut app.sources, item);
        }

        AppEvent::IssueRefreshError { issue_key, error } => {
            app.refreshing_issues.remove(&issue_key);
            app.action_state = ActionState::Error {
                error: Arc::new(error),
                scroll: 0,
            };
        }

        AppEvent::SearchJiraResult { token, result } => {
            apply_search_jira_result(app, token, result);
        }

        AppEvent::TeamStatusesLoaded { team_idx, result } => {
            apply_team_statuses_loaded(app, team_idx, result);
        }

        AppEvent::AllStatusesLoaded { team_idx, result } => {
            apply_all_statuses_loaded(app, team_idx, result);
        }

        AppEvent::AllProjectsLoaded { team_idx, result } => {
            apply_all_projects_loaded(app, team_idx, result);
        }

        AppEvent::CreateIssueTypesLoaded { token, result } => {
            apply_create_issuetypes_loaded(app, token, result);
        }

        AppEvent::CreateFieldsLoaded { token, result } => {
            apply_create_fields_loaded(app, token, result);
        }
    }
}

fn apply_create_issuetypes_loaded(
    app: &mut AppState,
    token: u64,
    result: Result<Vec<crate::jira::types::IssueTypeField>, anyhow::Error>,
) {
    let ActionState::CreatingIssue(ref mut form) = app.action_state else {
        return;
    };
    if token != form.meta_token {
        return;
    }
    match result {
        Ok(types) => {
            // Auto-select the first type and fetch its fields under the same token.
            if let Some(first) = types.first().cloned() {
                form.issuetype = Some(first);
                form.fields_state = CacheState::Loading;
                form.needs_field_fetch = true;
            }
            form.issuetypes = CacheState::Loaded(types);
        }
        Err(e) => {
            form.issuetypes = CacheState::Failed(e.to_string());
        }
    }
}

fn apply_create_fields_loaded(
    app: &mut AppState,
    token: u64,
    result: Result<Vec<serde_json::Value>, anyhow::Error>,
) {
    let ActionState::CreatingIssue(ref mut form) = app.action_state else {
        return;
    };
    if token != form.meta_token {
        return;
    }
    match result {
        Ok(values) => {
            form.fields = crate::tui::overlays::create_issue::parse_create_fields(&values);
            form.fields_state = CacheState::Loaded(());
            // Clamp focus to the new field/button range.
            let max = 2 + form.fields.len();
            if form.focus > max {
                form.focus = max;
            }
        }
        Err(e) => {
            form.fields_state = CacheState::Failed(e.to_string());
        }
    }
}

fn apply_team_statuses_loaded(
    app: &mut AppState,
    team_idx: usize,
    result: Result<Vec<String>, anyhow::Error>,
) {
    if team_idx != app.active_team_idx {
        return;
    }
    match result {
        Ok(statuses) => app.status_team_cache = CacheState::Loaded(statuses),
        Err(e) => {
            log::warn!("team statuses fetch failed: {e}");
            app.status_team_cache = CacheState::Failed(e.to_string());
        }
    }
    if status_picker_is_open(app) {
        rebuild_status_picker_items(app);
    }
}

fn apply_all_statuses_loaded(
    app: &mut AppState,
    team_idx: usize,
    result: Result<Vec<StatusInfo>, anyhow::Error>,
) {
    if team_idx != app.active_team_idx {
        return;
    }
    match result {
        Ok(statuses) => app.status_all_cache = CacheState::Loaded(statuses),
        Err(e) => {
            log::warn!("all statuses fetch failed: {e}");
            app.status_all_cache = CacheState::Failed(e.to_string());
        }
    }
    if status_picker_is_open(app) {
        rebuild_status_picker_items(app);
    }
}

fn apply_all_projects_loaded(
    app: &mut AppState,
    team_idx: usize,
    result: Result<Vec<ProjectInfo>, anyhow::Error>,
) {
    if team_idx != app.active_team_idx {
        return;
    }
    match result {
        Ok(projects) => app.project_cache = CacheState::Loaded(projects),
        Err(e) => {
            log::warn!("projects fetch failed: {e}");
            app.project_cache = CacheState::Failed(e.to_string());
        }
    }
    if project_picker_is_open(app) {
        rebuild_project_picker_items(app);
    }
    merge_projects_into_create_form(app);
}

/// If the create-issue form is open, append any newly cached projects not
/// already in `available_projects` (matched by uppercase key).
fn merge_projects_into_create_form(app: &mut AppState) {
    let cache = match app.project_cache.loaded() {
        Some(v) => v.clone(),
        None => return,
    };
    let ActionState::CreatingIssue(ref mut form) = app.action_state else {
        return;
    };
    crate::tui::overlays::create_issue::merge_cached_projects(&mut form.available_projects, &cache);
}

const fn status_picker_is_open(app: &AppState) -> bool {
    matches!(
        app.action_state,
        ActionState::Searching {
            picker: Some(FilterPicker {
                kind: FilterKind::Status,
                ..
            }),
            ..
        }
    )
}

const fn project_picker_is_open(app: &AppState) -> bool {
    matches!(
        app.action_state,
        ActionState::Searching {
            picker: Some(FilterPicker {
                kind: FilterKind::Project,
                ..
            }),
            ..
        }
    )
}

/// Rebuild the Status picker's item list from current cache contents.
/// Team = team-project workflow statuses; Other = all Jira statuses minus team.
/// Within Other, statuses in the `done` category (Done, Closed, Resolved,
/// Cancelled, Rejected, Won't Do…) appear first. Preserves the picker's
/// typeahead query and selected set.
fn rebuild_status_picker_items(app: &mut AppState) {
    use crate::jira::types::StatusCategory;

    let team: Vec<String> = app.status_team_cache.loaded().cloned().unwrap_or_default();
    let all: Vec<StatusInfo> = app.status_all_cache.loaded().cloned().unwrap_or_default();
    let loading = app.status_team_cache.is_pending() || app.status_all_cache.is_pending();

    let team_lower: HashSet<String> = team.iter().map(|s| s.to_lowercase()).collect();

    let mut other: Vec<StatusInfo> = all
        .into_iter()
        .filter(|s| !team_lower.contains(&s.name.to_lowercase()))
        .collect();
    other.sort_by(|a, b| {
        let a_done = a.category == StatusCategory::Done;
        let b_done = b.category == StatusCategory::Done;
        // Done category first; alphabetical within each group.
        b_done
            .cmp(&a_done)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    let mut items: Vec<PickerItem> = Vec::with_capacity(team.len() + other.len());
    for name in &team {
        items.push(PickerItem {
            section: PickerSection::Team,
            value: name.clone(),
            label: name.clone(),
        });
    }
    for s in other {
        items.push(PickerItem {
            section: PickerSection::Other,
            value: s.name.clone(),
            label: s.name,
        });
    }

    let ActionState::Searching {
        picker: Some(ref mut p),
        ..
    } = app.action_state
    else {
        return;
    };
    if p.kind != FilterKind::Status {
        return;
    }
    p.items = items;
    p.loading = loading;
    clamp_picker_cursor(p);
}

/// Rebuild the Project picker's item list. Team = team config projects;
/// Other = all fetched projects minus team.
fn rebuild_project_picker_items(app: &mut AppState) {
    let team_keys = crate::tui::team_project_keys(app);
    let team_keys_set: HashSet<String> = team_keys.iter().map(|k| k.to_uppercase()).collect();

    // Names for team projects come from any loaded issue we've seen.
    let mut team_names: HashMap<String, String> = HashMap::new();
    for issue in app.issues.iter().filter_map(WorkItem::as_jira) {
        let k = issue.fields.project.key.to_uppercase();
        if team_keys_set.contains(&k) {
            team_names
                .entry(k)
                .or_insert_with(|| issue.fields.project.name.clone());
        }
    }
    // Also pull names from the all-projects fetch when available.
    if let Some(all) = app.project_cache.loaded() {
        for p in all {
            let up = p.key.to_uppercase();
            if team_keys_set.contains(&up) {
                team_names.entry(up).or_insert_with(|| p.name.clone());
            }
        }
    }

    let mut items: Vec<PickerItem> = Vec::new();
    for k in &team_keys {
        let label = team_names
            .get(&k.to_uppercase())
            .map_or_else(|| k.clone(), |name| format!("{k}  {name}"));
        items.push(PickerItem {
            section: PickerSection::Team,
            value: k.clone(),
            label,
        });
    }
    if let Some(all) = app.project_cache.loaded() {
        for p in all {
            let up = p.key.to_uppercase();
            if !team_keys_set.contains(&up) {
                items.push(PickerItem {
                    section: PickerSection::Other,
                    value: p.key.clone(),
                    label: format!("{}  {}", p.key, p.name),
                });
            }
        }
    }

    let loading = app.project_cache.is_pending();
    let ActionState::Searching {
        picker: Some(ref mut p),
        ..
    } = app.action_state
    else {
        return;
    };
    if p.kind != FilterKind::Project {
        return;
    }
    p.items = items;
    p.loading = loading;
    clamp_picker_cursor(p);
}

fn clamp_picker_cursor(picker: &mut FilterPicker) {
    let visible = picker_visible_count(picker);
    if visible == 0 {
        picker.cursor = 0;
    } else if picker.cursor >= visible {
        picker.cursor = visible - 1;
    }
}

/// Kick off the background fetches the search overlay needs, if not already
/// loaded or in-flight. Called when the search overlay opens so that by the
/// time the user opens a picker the lists are usually populated.
fn schedule_filter_prefetches(app: &mut AppState) {
    if app.status_team_cache.is_idle() {
        app.status_team_cache = CacheState::Loading;
        app.pending_team_status_fetch = true;
    }
    if app.status_all_cache.is_idle() {
        app.status_all_cache = CacheState::Loading;
        app.pending_all_statuses_fetch = true;
    }
    if app.project_cache.is_idle() {
        app.project_cache = CacheState::Loading;
        app.pending_projects_fetch = true;
    }
}

/// Merge a Jira search response into the active `Searching` state, dropping
/// stale responses (token mismatch) silently.
fn apply_search_jira_result(
    app: &mut AppState,
    token: u64,
    result: Result<Vec<Issue>, anyhow::Error>,
) {
    let ActionState::Searching {
        ref query,
        ref filters,
        ref mut jira_state,
        ref mut selected,
        ref local_results,
        debounce_token,
        ..
    } = app.action_state
    else {
        return;
    };
    if token != debounce_token {
        return;
    }
    match result {
        Ok(issues) => {
            let mut hits = Vec::with_capacity(issues.len());
            let local_keys: std::collections::HashSet<&str> =
                local_results.iter().map(|h| h.issue_key.as_str()).collect();
            for issue in &issues {
                if local_keys.contains(issue.key.as_str()) {
                    continue;
                }
                if let Some(mut hit) = crate::tui::search::score_local_issue(query, filters, issue)
                {
                    hit.origin = crate::tui::search::HitOrigin::Jira;
                    hits.push(hit);
                }
            }
            hits.sort_by(|a, b| b.score.cmp(&a.score));
            *jira_state = JiraSearchState::Loaded { hits, issues };
            let total = local_results.len()
                + match jira_state {
                    JiraSearchState::Loaded { hits, .. } => hits.len(),
                    _ => 0,
                };
            if total > 0 && *selected >= total {
                *selected = total - 1;
            }
        }
        Err(e) => {
            *jira_state = JiraSearchState::Error(e.to_string());
        }
    }
}

/// Patch a refreshed item into both the flat `issues` list and the owning
/// source's item list. Silently drops if the item isn't found in either
/// — e.g. team switched while the refresh was in flight, or the issue was
/// deleted server-side.
fn apply_issue_refresh(
    issues: &mut [WorkItem],
    sources: &mut IndexMap<String, SourceState>,
    item: WorkItem,
) {
    if let Some(slot) = issues.iter_mut().find(|i| i.key() == item.key()) {
        *slot = item.clone();
    }
    if let Some(source_id) = item.source_id().map(str::to_owned)
        && let Some(SourceState::Loaded(source_items)) = sources.get_mut(&source_id)
        && let Some(slot) = source_items.iter_mut().find(|i| i.key() == item.key())
    {
        *slot = item;
    }
}

/// Route an async-action failure. A failed create returns to the form with
/// the error shown inline — the user's draft survives (Jira rejects creates
/// server-side routinely: workflow validators, permission schemes, fields
/// enforced but absent from createmeta). Everything else gets the error overlay.
fn error_action_state(prev: ActionState, e: anyhow::Error) -> ActionState {
    match prev {
        ActionState::AwaitingCreate { mut form } => {
            form.error = Some(format!("{e:#}"));
            ActionState::CreatingIssue(form)
        }
        _ => ActionState::Error {
            error: Arc::new(e),
            scroll: 0,
        },
    }
}

fn handle_action_done(app: &mut AppState, result: ActionResult) {
    match result {
        ActionResult::Error(e) => {
            app.action_state = error_action_state(
                std::mem::replace(&mut app.action_state, ActionState::None),
                e,
            );
        }
        ActionResult::IssueCreated { key } => {
            app.action_state = ActionState::IssueCreatedConfirm { key };
        }
        ActionResult::TaskCompleted { ref item_key } => apply_task_completed(app, item_key),
        ActionResult::Hidden { ref issue_key } => apply_hidden(app, issue_key),
        ActionResult::TransitionApplied {
            ref issue_key,
            ref new_status,
        } => apply_transition_applied(app, issue_key, new_status.as_ref()),
        ActionResult::TransitionsLoaded {
            issue_key,
            transitions,
        } => apply_transitions_loaded(app, issue_key, transitions),
        ActionResult::SprintsLoaded { issue_key, sprints } => {
            apply_sprints_loaded(app, issue_key, sprints);
        }
        ActionResult::MovedToSprint { ref issue_key } => apply_moved_to_sprint(app, issue_key),
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

/// Flip a completed Confluence task's status in both the flat list and the
/// owning source's list (mirrors `apply_issue_refresh`).
fn apply_task_completed(app: &mut AppState, item_key: &str) {
    if let Some(WorkItem::Confluence(task)) = app.issues.iter_mut().find(|i| i.key() == item_key) {
        task.set_complete();
    }
    for state in app.sources.values_mut() {
        if let SourceState::Loaded(items) = state
            && let Some(WorkItem::Confluence(task)) = items.iter_mut().find(|i| i.key() == item_key)
        {
            task.set_complete();
        }
    }
    app.action_state = ActionState::None;
}

fn apply_hidden(app: &mut AppState, issue_key: &str) {
    app.issues.retain(|i| i.key() != issue_key);
    app.rebuild_nav();
    app.action_state = ActionState::None;
}

/// A board fetch (incl. refresh) always starts with the config event, so it
/// doubles as the lane-state reset point: query-lane strategies get a fresh
/// `Loading` marker, other strategies clear any stale entry. No nav rebuild —
/// the `SourceLoaded` that follows on the same channel does it.
fn apply_board_config_loaded(
    app: &mut AppState,
    source_id: String,
    config: crate::jira::types::BoardConfiguration,
) {
    let expects_lanes = source_config_for(app.team_config(), &source_id)
        .and_then(|s| s.board.as_ref())
        .and_then(|b| b.swimlanes.as_ref())
        .is_some_and(|s| {
            matches!(
                s,
                crate::config::types::SwimlaneConfig::Auto
                    | crate::config::types::SwimlaneConfig::Queries { .. }
            )
        });
    if expects_lanes {
        app.board_lanes
            .insert(source_id.clone(), LanesState::Loading);
    } else {
        app.board_lanes.remove(&source_id);
    }
    app.board_configs.insert(source_id, config);
}

fn apply_transitions_loaded(
    app: &mut AppState,
    issue_key: String,
    transitions: Vec<crate::jira::types::Transition>,
) {
    // In board mode the same `t` press becomes "move to column"; the raw
    // picker stays the fallback whenever the column mapping isn't possible,
    // so `t` never dead-ends.
    if let Some(state) = board_column_picker_state(app, &issue_key, &transitions) {
        app.action_state = state;
        return;
    }
    app.action_state = ActionState::SelectingTransition {
        issue_key,
        transitions,
        selected: 0,
    };
}

/// The column-picker state for a board-mode transition, or `None` when the
/// issue is off-board, the config is missing, or no transition reaches any
/// column (fall back to the raw transition list).
fn board_column_picker_state(
    app: &AppState,
    issue_key: &str,
    transitions: &[crate::jira::types::Transition],
) -> Option<ActionState> {
    if !board_mode_active(app) {
        return None;
    }
    let source_id = app.board_view.as_deref()?;
    let config = app.board_configs.get(source_id)?;
    let is_member = match app.sources.get(source_id) {
        Some(SourceState::Loaded(items)) => items.iter().any(|i| i.key() == issue_key),
        _ => false,
    };
    if !is_member {
        return None;
    }
    let status_id = app
        .issues
        .iter()
        .find(|i| i.key() == issue_key)
        .and_then(WorkItem::as_jira)
        .map(|i| i.fields.status.id.clone())?;
    let columns = crate::tui::board::map_transitions_to_columns(
        &config.column_config.columns,
        &status_id,
        transitions,
    );
    if !columns.iter().any(|c| c.transition_id.is_some()) {
        return None;
    }
    // Start on the first reachable non-current column — the likeliest move.
    let selected = columns
        .iter()
        .position(|c| c.transition_id.is_some() && !c.is_current)
        .or_else(|| columns.iter().position(|c| c.transition_id.is_some()))
        .unwrap_or(0);
    Some(ActionState::SelectingBoardColumn {
        issue_key: issue_key.to_owned(),
        transitions: transitions.to_vec(),
        columns,
        selected,
    })
}

fn apply_field_names_loaded(
    app: &mut AppState,
    names: HashMap<String, String>,
    schemas: HashMap<String, FieldSchema>,
    all_fields: bool,
) {
    app.field_names.extend(names);
    app.field_schemas.extend(schemas);
    app.flags.field_names = FieldNamesState::Idle;
    if all_fields {
        app.flags.field_names = FieldNamesState::AllLoaded;
    }
}

fn apply_transition_applied(
    app: &mut AppState,
    issue_key: &str,
    new_status: Option<&crate::jira::types::StatusField>,
) {
    write_transitioned_status(&mut app.issues, issue_key, new_status);
    app.action_state = ActionState::None;
}

/// Write the transition's target status (id + name) onto the flat item. The
/// id matters: the board view re-groups cards by `status.id`.
fn write_transitioned_status(
    issues: &mut [WorkItem],
    issue_key: &str,
    new_status: Option<&crate::jira::types::StatusField>,
) {
    let Some(status) = new_status else { return };
    if let Some(issue) = issues
        .iter_mut()
        .find(|i| i.key() == issue_key)
        .and_then(WorkItem::as_jira_mut)
    {
        issue.fields.status = status.clone();
    }
}

fn apply_moved_to_project(app: &mut AppState, issue_key: &str, project: &str) {
    if let Some(issue) = app.jira_issue_mut(issue_key) {
        issue.fields.project.key = project.to_string();
    }
    app.action_state = ActionState::None;
}

fn apply_attachment_uploaded(
    app: &mut AppState,
    issue_key: &str,
    new_attachment: crate::jira::types::Attachment,
) {
    if let Some(issue) = app.jira_issue_mut(issue_key) {
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
    if let Some(issue) = app.jira_issue_mut(issue_key) {
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
    if let Some(issue) = app.jira_issue_mut(issue_key) {
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
        && let Some(issue) = app.jira_issue_mut(issue_key)
    {
        issue.fields.assignee = Some(crate::jira::types::UserField {
            name: None,
            display_name: Some(me.clone()),
            account_id: Some(me.clone()),
        });
    }
}

fn apply_comment_edit(app: &mut AppState, issue_key: &str, updated_comment: &Comment) {
    if let Some(issue) = app.jira_issue_mut(issue_key)
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
    if let Some(issue) = app.jira_issue_mut(issue_key)
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
    if let Some(issue) = app.jira_issue_mut(issue_key)
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
    if matches!(
        (code, modifiers),
        (KeyCode::Char('c'), KeyModifiers::CONTROL)
    ) {
        app.should_quit = true;
        return true;
    }
    let max_scroll = u16::try_from(
        app.last_confirm_content_h
            .saturating_sub(app.last_confirm_viewport_h),
    )
    .unwrap_or(u16::MAX);
    let ActionState::ConfirmingCommentEdit {
        ref issue_key,
        ref comment_id,
        ref new_text,
        ref mut tab,
        ref mut scroll,
        ..
    } = app.action_state
    else {
        return false;
    };
    match (code, modifiers) {
        (KeyCode::Esc | KeyCode::Char('q'), _) => {
            app.action_state = ActionState::None;
        }
        (KeyCode::Tab, _) => {
            *tab = 1 - *tab;
            *scroll = 0;
        }
        (KeyCode::Left | KeyCode::Char('h'), _) => {
            if *tab != 0 {
                *tab = 0;
                *scroll = 0;
            }
        }
        (KeyCode::Right | KeyCode::Char('l'), _) => {
            if *tab != 1 {
                *tab = 1;
                *scroll = 0;
            }
        }
        (KeyCode::Up | KeyCode::Char('k'), _) => {
            *scroll = scroll.saturating_sub(1);
        }
        (KeyCode::Down | KeyCode::Char('j'), _) => {
            *scroll = scroll.saturating_add(1).min(max_scroll);
        }
        (KeyCode::PageUp, _) => {
            *scroll = scroll.saturating_sub(10);
        }
        (KeyCode::PageDown, _) => {
            *scroll = scroll.saturating_add(10).min(max_scroll);
        }
        (KeyCode::Enter, _) => {
            let issue_key = issue_key.clone();
            let comment_id = comment_id.clone();
            let new_body = new_text.clone();
            app.action_state = ActionState::CommittingCommentEdit {
                issue_key,
                comment_id,
                new_body,
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

#[allow(clippy::too_many_lines)]
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
        ActionState::SelectingBoardColumn { .. } => {
            handle_board_column_input(app, &event);
            return;
        }
        ActionState::SelectingSprint { .. } => {
            handle_sprint_picker_input(app, &event);
            return;
        }
        ActionState::HidePopup { .. } => {
            handle_hide_input(app, event);
            return;
        }
        ActionState::Error { .. } => {
            handle_error_input(app, &event);
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
        ActionState::ConfirmingCompleteTask { .. } => {
            handle_complete_task_confirm_input(app, &event);
            return;
        }
        ActionState::Searching { .. } => {
            handle_search_input(app, event);
            return;
        }
        ActionState::CreatingIssue(_) => {
            crate::tui::overlays::create_issue::handle_create_input(app, event);
            return;
        }
        ActionState::IssueCreatedConfirm { .. } => {
            if let crossterm::event::Event::Key(crossterm::event::KeyEvent { code, .. }) = event
                && matches!(code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q'))
            {
                app.action_state = ActionState::None;
            }
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
        | ActionState::LoadingSprints { .. }
        | ActionState::PendingMoveToSprint { .. }
        | ActionState::PendingTransition { .. }
        | ActionState::PendingCompleteTask { .. }
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
        | ActionState::CommittingCreate { .. }
        | ActionState::AwaitingCreate { .. }
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

    // Board mode remaps navigation to the 3-D board cursor; anything it
    // doesn't consume falls through to the normal arms below (item actions
    // all resolve via selected_item(), so they work unchanged on cards).
    if board_mode_active(app) && handle_board_key(app, code, modifiers) {
        return;
    }

    match (code, modifiers) {
        // Tab switching across the tab list (teams + board tabs).
        (KeyCode::Tab, _) => {
            let len = app.tab_list().len();
            if len > 1 {
                app.activate_tab((app.active_tab_index() + 1) % len);
            }
        }
        (KeyCode::BackTab, _) => {
            let len = app.tab_list().len();
            if len > 1 {
                app.activate_tab((app.active_tab_index() + len - 1) % len);
            }
        }
        // Issue view opened from a search: q/Esc return to the search overlay
        // with its prior state rather than quitting (Ctrl+C still quits). Takes
        // precedence over the board arm below — search is the more recent origin.
        (KeyCode::Char('q') | KeyCode::Esc, m)
            if app.fullscreen_detail
                && app.saved_search.is_some()
                && !m.contains(KeyModifiers::CONTROL) =>
        {
            app.fullscreen_detail = false;
            if let Some(saved) = app.saved_search.take() {
                app.action_state = *saved;
            }
        }
        // Issue view opened from a board: q/Esc step back to the board rather
        // than quitting the app (Ctrl+C still quits — it's a separate arm).
        (KeyCode::Char('q') | KeyCode::Esc, m)
            if app.fullscreen_detail
                && app.board_view.is_some()
                && !m.contains(KeyModifiers::CONTROL) =>
        {
            app.fullscreen_detail = false;
            app.focused_panel = FocusedPanel::List;
        }
        (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        (KeyCode::Esc, _) if app.fullscreen_detail => {
            app.fullscreen_detail = false;
            app.focused_panel = FocusedPanel::List;
        }
        (KeyCode::Left | KeyCode::Char('h'), _) => {
            if app.fullscreen_detail {
                app.fullscreen_detail = false;
                // Left the search-originated detail without returning to search.
                app.saved_search = None;
            }
            app.focused_panel = FocusedPanel::List;
        }
        (KeyCode::Right | KeyCode::Char('l'), _) => {
            app.focused_panel = FocusedPanel::Detail;
            request_detail_load_if_partial(app);
        }
        // Shift+arrows re-rank in backlog tabs; must precede plain arrow nav.
        (KeyCode::Up, KeyModifiers::SHIFT) if backlog_mode_active(app) => key_rank_move(app, true),
        (KeyCode::Down, KeyModifiers::SHIFT) if backlog_mode_active(app) => {
            key_rank_move(app, false);
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
            // Cycle view modes manually. Items without comments/attachments
            // (non-Jira) skip those modes entirely.
            let has_subviews = app.selected_item().is_none_or(WorkItem::supports_comments);
            app.view_mode = match &app.view_mode {
                ViewMode::Default | ViewMode::Custom(_) if has_subviews => ViewMode::Comments,
                ViewMode::Comments if has_subviews => ViewMode::Attachments,
                _ => {
                    // Return to the natural view for the current item
                    app.selected_item().map_or(ViewMode::Default, |item| {
                        auto_view_mode(&item.clone(), app.team_config())
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
            if let Some(item) = app.selected_item() {
                let url = item.browse_url(&app.team_jira().base_url);
                let _ = open::that(url);
            }
        }
        (KeyCode::Char('t'), _) => key_change_status(app),
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
        (KeyCode::Char('R'), _) => key_refresh_all(app),
        (KeyCode::Char('r'), _) => key_refresh_focused(app),
        (KeyCode::Char('P'), _) => key_preload_details(app),
        // Backlog grooming: Shift+K/Shift+J re-rank, `s` sends to a sprint.
        (KeyCode::Char('K'), _) if backlog_mode_active(app) => key_rank_move(app, true),
        (KeyCode::Char('J'), _) if backlog_mode_active(app) => key_rank_move(app, false),
        (KeyCode::Char('s'), _) if backlog_mode_active(app) => key_send_to_sprint(app),
        (KeyCode::Char('/'), _) => key_open_search(app),
        (KeyCode::Char('n'), _) => key_open_create(app),
        _ => {}
    }
}

/// True while the kanban board covers the main area (fullscreen detail on a
/// card temporarily leaves board key handling). Backlog tabs also live in
/// `board_view` but render as a plain list, so they keep list navigation.
fn board_mode_active(app: &AppState) -> bool {
    app.active_tab_source_kind() == Some(crate::config::types::SourceKind::Board)
        && !app.fullscreen_detail
}

/// True while a backlog tab covers the main area — enables the backlog-only
/// keys (rank reorder, send-to-sprint) on top of normal list handling.
fn backlog_mode_active(app: &AppState) -> bool {
    app.active_tab_source_kind() == Some(crate::config::types::SourceKind::Backlog)
        && !app.fullscreen_detail
}

/// Optimistically move the selected backlog issue one step up/down in rank
/// order (both in the source's items and the flat list), then schedule the
/// debounced Jira rank mutation. Consecutive moves of the same issue collapse
/// into one call anchored at the final neighbors.
fn key_rank_move(app: &mut AppState, up: bool) {
    let Some(source_id) = app.board_view.clone() else {
        return;
    };
    let Some(issue_key) = app.selected_item().map(|i| i.key().to_owned()) else {
        return;
    };

    let Some(SourceState::Loaded(items)) = app.sources.get_mut(&source_id) else {
        return;
    };
    let keys: Vec<String> = items.iter().map(|i| i.key().to_owned()).collect();
    let Some((target, anchor)) = crate::tui::backlog::compute_rank_move(&keys, &issue_key, up)
    else {
        return;
    };
    let moved_idx = if up { target + 1 } else { target - 1 };
    items.swap(moved_idx, target);

    // Mirror the swap in the flat list, which holds all team sources.
    let flat_moved = app.issues.iter().position(|i| i.key() == issue_key);
    let flat_neighbor = app.issues.iter().position(|i| i.key() == keys[target]);
    if let (Some(a), Some(b)) = (flat_moved, flat_neighbor) {
        app.issues.swap(a, b);
    }
    app.rebuild_nav();
    // Selection follows the moved issue.
    if let Some(pos) = app.nav_items.iter().position(|n| {
        matches!(n, NavItem::Issue(i) if app.issues.get(*i).map(WorkItem::key) == Some(issue_key.as_str()))
    }) {
        app.nav_idx = pos;
    }

    let rank_field_id = app
        .board_configs
        .get(&source_id)
        .and_then(|c| c.ranking.as_ref())
        .and_then(|r| r.rank_custom_field_id);
    // A different issue's pending move dispatches first, unchanged.
    if let Some(prev) = app.pending_rank.take() {
        if prev.issue_key == issue_key {
            // Collapsed: the new anchor supersedes it.
        } else {
            app.rank_flush_queue.push(prev);
        }
    }
    app.pending_rank = Some(crate::tui::backlog::PendingRank {
        source_id,
        issue_key,
        anchor,
        rank_field_id,
        last_move_at: std::time::Instant::now(),
    });
}

/// Open the send-to-sprint flow for the selected backlog issue: the sprint
/// list is fetched first (`LoadingSprints` → `SelectingSprint`).
fn key_send_to_sprint(app: &mut AppState) {
    let Some(issue_key) = app.selected_issue().map(|i| i.key.clone()) else {
        return;
    };
    app.action_state = ActionState::LoadingSprints { issue_key };
}

/// A rank mutation came back: persist the confirmed order to the cache on
/// success; on failure surface the error and refetch the backlog so the
/// server's order is truth again.
fn apply_issue_ranked(app: &mut AppState, source_id: String, result: Result<(), anyhow::Error>) {
    match result {
        Ok(()) => {
            if let Some(SourceState::Loaded(items)) = app.sources.get(&source_id) {
                crate::sources::cache::write(
                    &app.cache,
                    &source_id,
                    items,
                    app.board_configs.get(&source_id),
                    None,
                );
            }
        }
        Err(e) => {
            app.pending_rank_refetch = Some(source_id);
            // Don't clobber an open overlay; the refetch alone restores the
            // server's order.
            if matches!(app.action_state, ActionState::None) {
                app.action_state = ActionState::Error {
                    error: Arc::new(e),
                    scroll: 0,
                };
            }
        }
    }
}

/// Route the fetched sprint list into the picker. Kanban boards (`None`) and
/// boards without upcoming sprints get a plain explanation instead.
fn apply_sprints_loaded(
    app: &mut AppState,
    issue_key: String,
    sprints: Option<Vec<crate::jira::types::Sprint>>,
) {
    match sprints {
        None => {
            app.action_state = ActionState::Error {
                error: Arc::new(anyhow::anyhow!(
                    "This board has no sprints — kanban boards can't receive backlog issues via a sprint."
                )),
                scroll: 0,
            };
        }
        Some(sprints) if sprints.is_empty() => {
            app.action_state = ActionState::Error {
                error: Arc::new(anyhow::anyhow!(
                    "No active or future sprints on this board. Create a sprint in Jira first."
                )),
                scroll: 0,
            };
        }
        Some(sprints) => {
            // Cursor starts on the first active sprint — the usual target.
            let selected = sprints
                .iter()
                .position(|s| s.state == "active")
                .unwrap_or(0);
            app.action_state = ActionState::SelectingSprint {
                issue_key,
                sprints,
                selected,
            };
        }
    }
}

/// The issue left the backlog for a sprint: drop it from both lists, keep the
/// cache in sync, and close the overlay.
fn apply_moved_to_sprint(app: &mut AppState, issue_key: &str) {
    let source_id = app
        .issues
        .iter()
        .find(|i| i.key() == issue_key)
        .and_then(|i| i.source_id().map(str::to_owned));
    app.issues.retain(|i| i.key() != issue_key);
    if let Some(sid) = &source_id
        && let Some(SourceState::Loaded(items)) = app.sources.get_mut(sid)
    {
        items.retain(|i| i.key() != issue_key);
        if let Some(SourceState::Loaded(items)) = app.sources.get(sid) {
            crate::sources::cache::write(&app.cache, sid, items, app.board_configs.get(sid), None);
        }
    }
    app.rebuild_nav();
    app.action_state = ActionState::None;
}

fn handle_sprint_picker_input(app: &mut AppState, event: &crossterm::event::Event) {
    use crossterm::event::{Event, KeyEvent};
    let ActionState::SelectingSprint {
        ref issue_key,
        ref sprints,
        ref mut selected,
    } = app.action_state
    else {
        return;
    };

    if let Event::Key(KeyEvent { code, .. }) = *event {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                app.action_state = ActionState::None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if *selected + 1 < sprints.len() {
                    *selected += 1;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                *selected = selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                if let Some(sprint) = sprints.get(*selected) {
                    let key = issue_key.clone();
                    let sprint_id = sprint.id;
                    app.action_state = ActionState::PendingMoveToSprint {
                        issue_key: key,
                        sprint_id,
                    };
                }
            }
            _ => {}
        }
    }
}

/// Board-mode key handling. Returns true when the key was consumed.
fn handle_board_key(app: &mut AppState, code: KeyCode, modifiers: KeyModifiers) -> bool {
    use crate::tui::board::{BoardMove, app_board_grouping, cursor_pos};

    // Never swallow control chords (Ctrl+C must keep quitting).
    if modifiers.contains(KeyModifiers::CONTROL) {
        return false;
    }
    let Some(source_id) = app.board_view.clone() else {
        return false;
    };

    let Some(grouping) = app_board_grouping(app, &source_id) else {
        // Config missing (placeholder screen): consume navigation so panel
        // focus doesn't shift underneath; let everything else through.
        return matches!(
            code,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Enter
                | KeyCode::PageUp
                | KeyCode::PageDown
                | KeyCode::Char('h' | 'j' | 'k' | 'l' | 'v')
        );
    };
    let on_board = cursor_pos(&grouping, app.nav_idx).is_some();

    match code {
        KeyCode::Left | KeyCode::Char('h') => board_move(app, &grouping, BoardMove::Left),
        KeyCode::Right | KeyCode::Char('l') => board_move(app, &grouping, BoardMove::Right),
        KeyCode::Down | KeyCode::Char('j') => board_move(app, &grouping, BoardMove::Down),
        KeyCode::Up | KeyCode::Char('k') => board_move(app, &grouping, BoardMove::Up),
        KeyCode::PageDown => {
            for _ in 0..5 {
                board_move(app, &grouping, BoardMove::Down);
            }
        }
        KeyCode::PageUp => {
            for _ in 0..5 {
                board_move(app, &grouping, BoardMove::Up);
            }
        }
        KeyCode::Enter => {
            if on_board {
                app.fullscreen_detail = true;
                // This detail was opened from the board, not from search.
                app.saved_search = None;
                app.focused_panel = FocusedPanel::Detail;
                app.detail_scroll = 0;
                request_detail_load_if_partial(app);
            }
        }
        // Detail view-mode cycling is meaningless with no detail visible.
        KeyCode::Char('v') => {}
        // Item actions on an off-board selection would target an invisible
        // item — consume them as no-ops. On-board they fall through.
        KeyCode::Char('t' | 'o' | 'c' | 'i' | 'a' | 'm' | 'r') if !on_board => {}
        _ => return false,
    }
    true
}

/// Apply a board cursor move to `nav_idx`.
fn board_move(
    app: &mut AppState,
    grouping: &crate::tui::board::BoardGrouping,
    mv: crate::tui::board::BoardMove,
) {
    if let Some(pos) = crate::tui::board::move_cursor(grouping, app.nav_idx, mv) {
        app.nav_idx = pos;
        update_view_mode_on_navigate(app);
    }
}

/// Board-mode gg/G: jump within the current column (derives the grouping).
fn board_jump(app: &mut AppState, mv: crate::tui::board::BoardMove) {
    let Some(source_id) = app.board_view.clone() else {
        return;
    };
    if let Some(grouping) = crate::tui::board::app_board_grouping(app, &source_id) {
        board_move(app, &grouping, mv);
    }
}

/// Open the create-issue form. Seeds the project list with the team's
/// configured projects plus any projects seen in loaded issues; the full
/// list of accessible projects is fetched lazily when the picker opens.
fn key_open_create(app: &mut AppState) {
    use crate::jira::types::ProjectField;
    use crate::tui::overlays::create_issue::{CreateForm, distinct_projects};

    let team_keys = crate::tui::team_project_keys(app);
    let issue_projects = distinct_projects(&app.issues);

    // Build team_keys → ProjectField, borrowing names from loaded issues when
    // available. `id` is empty: payload submission only uses `key`.
    let mut projects: Vec<ProjectField> =
        Vec::with_capacity(team_keys.len() + issue_projects.len());
    let mut seen: HashSet<String> = HashSet::new();
    for key in &team_keys {
        let up = key.to_uppercase();
        if !seen.insert(up.clone()) {
            continue;
        }
        let name = issue_projects
            .iter()
            .find(|p| p.key.to_uppercase() == up)
            .map_or_else(|| key.clone(), |p| p.name.clone());
        projects.push(ProjectField {
            id: String::new(),
            key: key.clone(),
            name,
        });
    }
    for p in issue_projects {
        if seen.insert(p.key.to_uppercase()) {
            projects.push(p);
        }
    }

    let project = app
        .selected_issue()
        .map(|i| i.fields.project.clone())
        .or_else(|| projects.first().cloned());
    let Some(project) = project else {
        // No project context to create against.
        app.action_state = ActionState::Error {
            error: Arc::new(anyhow::anyhow!(
                "No project available to create an issue. Load some issues first."
            )),
            scroll: 0,
        };
        return;
    };
    app.action_state = ActionState::CreatingIssue(CreateForm::open(project, projects));
}

fn key_open_search(app: &mut AppState) {
    let prev_nav_idx = app.nav_idx;
    let filters = SearchFilters::default();
    let mut local_results: Vec<RankedHit> = app
        .issues
        .iter()
        .filter_map(|item| crate::tui::search::score_local("", &filters, item))
        .collect();
    sort_hits(&mut local_results);
    app.action_state = ActionState::Searching {
        query: String::new(),
        cursor: 0,
        filters,
        focus: SearchFocus::Input,
        local_results,
        jira_state: JiraSearchState::Idle,
        selected: 0,
        prev_nav_idx,
        debounce_token: 1,
        last_change_at: std::time::Instant::now(),
        jira_spawned_for_token: true,
        picker: None,
    };
    // Pre-fetch status / project lists in the background so the pickers are
    // populated by the time the user opens them.
    schedule_filter_prefetches(app);
}

fn sort_hits(hits: &mut [RankedHit]) {
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.issue_key.cmp(&b.issue_key))
    });
}

/// Total number of merged results currently shown in the search popup.
const fn search_total_results(local: &[RankedHit], jira: &JiraSearchState) -> usize {
    local.len()
        + match jira {
            JiraSearchState::Loaded { hits, .. } => hits.len(),
            _ => 0,
        }
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
fn handle_search_input(app: &mut AppState, event: crossterm::event::Event) {
    use crossterm::event::{Event, KeyEvent};

    let Event::Key(KeyEvent {
        code, modifiers, ..
    }) = event
    else {
        return;
    };

    // Picker overlay swallows all input while open.
    if matches!(
        app.action_state,
        ActionState::Searching {
            picker: Some(_),
            ..
        }
    ) {
        handle_picker_input(app, code, modifiers);
        return;
    }

    // Alt+1 / Alt+2 open the Status / Project pickers from any focus inside
    // the search overlay. Alt+<char> is reliably distinguishable in terminals
    // (ESC-prefixed encoding), unlike Ctrl+<digit>. Plain digits remain
    // available as query text.
    if modifiers.contains(KeyModifiers::ALT) {
        if code == KeyCode::Char('1') {
            open_status_picker(app);
            return;
        }
        if code == KeyCode::Char('2') {
            open_project_picker(app);
            return;
        }
        // Any other Alt combo is currently a no-op inside the search overlay.
        return;
    }

    // Swallow any other Ctrl+<char> combination — these are mostly garbage
    // from the legacy Ctrl+<digit> encodings (Ctrl+2 → NUL/space) and would
    // otherwise leak into the query.
    if matches!(modifiers, KeyModifiers::CONTROL)
        && !matches!(
            code,
            KeyCode::Char('p' | 'n') // result navigation, handled below
        )
    {
        return;
    }

    // Global keys regardless of focus
    match (code, modifiers) {
        (KeyCode::Esc, _) => {
            cancel_search(app);
            return;
        }
        (KeyCode::Tab, _) => {
            cycle_search_focus(app, true);
            return;
        }
        (KeyCode::BackTab, _) => {
            cycle_search_focus(app, false);
            return;
        }
        (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
            move_search_selection(app, -1);
            return;
        }
        (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
            move_search_selection(app, 1);
            return;
        }
        _ => {}
    }

    let ActionState::Searching { focus, .. } = app.action_state else {
        return;
    };

    // Enter / Space behave differently per slot.
    match (code, focus) {
        (KeyCode::Enter | KeyCode::Char(' '), SearchFocus::StatusSlot) => {
            open_status_picker(app);
            return;
        }
        (KeyCode::Enter | KeyCode::Char(' '), SearchFocus::ProjectSlot) => {
            open_project_picker(app);
            return;
        }
        (KeyCode::Enter, _) => {
            commit_search_selection(app);
            return;
        }
        _ => {}
    }

    if focus != SearchFocus::Input {
        return;
    }

    let ActionState::Searching {
        ref mut query,
        ref mut cursor,
        ref filters,
        ref mut local_results,
        ref mut jira_state,
        ref mut selected,
        ref mut debounce_token,
        ref mut last_change_at,
        ref mut jira_spawned_for_token,
        ..
    } = app.action_state
    else {
        return;
    };

    let before = query.clone();
    edit_text(query, cursor, code);
    if query != &before {
        bump_debounce(
            debounce_token,
            last_change_at,
            jira_spawned_for_token,
            jira_state,
        );
        *local_results = recompute_local(&app.issues, query, filters);
        clamp_selection(selected, local_results.len(), jira_state);
    }
}

fn recompute_local(items: &[WorkItem], query: &str, filters: &SearchFilters) -> Vec<RankedHit> {
    let mut hits: Vec<RankedHit> = items
        .iter()
        .filter_map(|item| crate::tui::search::score_local(query, filters, item))
        .collect();
    sort_hits(&mut hits);
    hits
}

const fn clamp_selection(selected: &mut usize, local_len: usize, jira: &JiraSearchState) {
    let total = local_len
        + match jira {
            JiraSearchState::Loaded { hits, .. } => hits.len(),
            _ => 0,
        };
    if total == 0 {
        *selected = 0;
    } else if *selected >= total {
        *selected = total - 1;
    }
}

fn bump_debounce(
    token: &mut u64,
    last_change_at: &mut std::time::Instant,
    jira_spawned_for_token: &mut bool,
    jira_state: &mut JiraSearchState,
) {
    *token = token.wrapping_add(1);
    *last_change_at = std::time::Instant::now();
    *jira_spawned_for_token = false;
    *jira_state = JiraSearchState::Idle;
}

fn cycle_search_focus(app: &mut AppState, forward: bool) {
    let ActionState::Searching {
        ref mut focus,
        ref local_results,
        ref jira_state,
        ..
    } = app.action_state
    else {
        return;
    };
    let result_count = search_total_results(local_results, jira_state);
    // Cycle: Input → StatusSlot → ProjectSlot → Result(0) → Input.
    let order_len = 3 + usize::from(result_count > 0);
    let cur = match *focus {
        SearchFocus::Input => 0,
        SearchFocus::StatusSlot => 1,
        SearchFocus::ProjectSlot => 2,
        SearchFocus::Result(_) => 3,
    };
    let next = if forward {
        (cur + 1) % order_len
    } else {
        (cur + order_len - 1) % order_len
    };
    *focus = match next {
        0 => SearchFocus::Input,
        1 => SearchFocus::StatusSlot,
        2 => SearchFocus::ProjectSlot,
        _ => SearchFocus::Result(0),
    };
}

/// Indices into `picker.items` of rows the user can navigate over, after
/// applying the typeahead filter.
pub fn picker_visible_indices(picker: &FilterPicker) -> Vec<usize> {
    let q = picker.query.to_lowercase();
    picker
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| q.is_empty() || item.label.to_lowercase().contains(&q))
        .map(|(i, _)| i)
        .collect()
}

pub fn picker_visible_count(picker: &FilterPicker) -> usize {
    picker_visible_indices(picker).len()
}

fn open_status_picker(app: &mut AppState) {
    let selected_values: HashMap<String, FilterChoice> = match &app.action_state {
        ActionState::Searching { filters, .. } => {
            let mut m = HashMap::new();
            for s in &filters.statuses {
                m.insert(s.clone(), FilterChoice::Include);
            }
            for s in &filters.statuses_exclude {
                m.insert(s.clone(), FilterChoice::Exclude);
            }
            m
        }
        _ => HashMap::new(),
    };
    // Ensure the fetches that feed this picker are running.
    if app.status_team_cache.is_idle() {
        app.status_team_cache = CacheState::Loading;
        app.pending_team_status_fetch = true;
    }
    if app.status_all_cache.is_idle() {
        app.status_all_cache = CacheState::Loading;
        app.pending_all_statuses_fetch = true;
    }

    if let ActionState::Searching { ref mut picker, .. } = app.action_state {
        *picker = Some(FilterPicker {
            kind: FilterKind::Status,
            query: String::new(),
            query_cursor: 0,
            items: Vec::new(),
            cursor: 0,
            selected: selected_values,
            loading: true,
        });
    }
    // Populate from whichever caches are already loaded.
    rebuild_status_picker_items(app);
}

fn open_project_picker(app: &mut AppState) {
    let selected_values: HashMap<String, FilterChoice> = match &app.action_state {
        ActionState::Searching { filters, .. } => filters
            .projects
            .iter()
            .map(|p| (p.clone(), FilterChoice::Include))
            .collect(),
        _ => HashMap::new(),
    };
    if app.project_cache.is_idle() {
        app.project_cache = CacheState::Loading;
        app.pending_projects_fetch = true;
    }

    if let ActionState::Searching { ref mut picker, .. } = app.action_state {
        *picker = Some(FilterPicker {
            kind: FilterKind::Project,
            query: String::new(),
            query_cursor: 0,
            items: Vec::new(),
            cursor: 0,
            selected: selected_values,
            loading: true,
        });
    }
    rebuild_project_picker_items(app);
}

fn cancel_picker(app: &mut AppState) {
    if let ActionState::Searching { ref mut picker, .. } = app.action_state {
        *picker = None;
    }
}

fn apply_picker(app: &mut AppState) {
    let (kind, picked) = {
        let ActionState::Searching { ref mut picker, .. } = app.action_state else {
            return;
        };
        let Some(p) = picker.take() else {
            return;
        };
        (p.kind, p.selected)
    };

    let ActionState::Searching {
        ref mut filters,
        ref mut local_results,
        ref query,
        ref mut jira_state,
        selected: ref mut sel,
        ref mut debounce_token,
        ref mut last_change_at,
        ref mut jira_spawned_for_token,
        ..
    } = app.action_state
    else {
        return;
    };

    match kind {
        FilterKind::Status => {
            let (mut include, mut exclude): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
            for (value, choice) in picked {
                match choice {
                    FilterChoice::Include => include.push(value),
                    FilterChoice::Exclude => exclude.push(value),
                }
            }
            include.sort();
            exclude.sort();
            filters.statuses = include;
            filters.statuses_exclude = exclude;
        }
        FilterKind::Project => {
            filters.projects = picked.into_keys().collect();
            filters.projects.sort();
        }
    }
    *local_results = recompute_local(&app.issues, query, filters);
    bump_debounce(
        debounce_token,
        last_change_at,
        jira_spawned_for_token,
        jira_state,
    );
    clamp_selection(sel, local_results.len(), jira_state);
}

#[allow(clippy::too_many_lines)]
fn handle_picker_input(app: &mut AppState, code: KeyCode, modifiers: KeyModifiers) {
    let ActionState::Searching {
        picker: Some(ref mut picker),
        ..
    } = app.action_state
    else {
        return;
    };

    match (code, modifiers) {
        (KeyCode::Esc, _) => {
            cancel_picker(app);
            return;
        }
        (KeyCode::Enter, _) => {
            apply_picker(app);
            return;
        }
        (KeyCode::Up, _) => {
            if picker.cursor > 0 {
                picker.cursor -= 1;
            }
            return;
        }
        (KeyCode::Down, _) => {
            let visible = picker_visible_count(picker);
            if picker.cursor + 1 < visible {
                picker.cursor += 1;
            }
            return;
        }
        (KeyCode::Char(' '), _) => {
            let visible = picker_visible_indices(picker);
            if let Some(&item_idx) = visible.get(picker.cursor) {
                let value = picker.items[item_idx].value.clone();
                let current = picker.selected.get(&value).copied();
                // Project picker is include-only: cycle skips Exclude.
                let next = if picker.kind == FilterKind::Project {
                    match current {
                        None => Some(FilterChoice::Include),
                        Some(_) => None,
                    }
                } else {
                    FilterChoice::next(current)
                };
                match next {
                    Some(choice) => {
                        picker.selected.insert(value, choice);
                    }
                    None => {
                        picker.selected.remove(&value);
                    }
                }
            }
            return;
        }
        _ => {}
    }

    let before = picker.query.clone();
    edit_text(&mut picker.query, &mut picker.query_cursor, code);
    if picker.query != before {
        clamp_picker_cursor(picker);
    }
}

fn move_search_selection(app: &mut AppState, delta: i32) {
    let ActionState::Searching {
        ref mut selected,
        ref local_results,
        ref jira_state,
        ..
    } = app.action_state
    else {
        return;
    };
    let total = search_total_results(local_results, jira_state);
    if total == 0 {
        *selected = 0;
        return;
    }
    let cur = i64::try_from(*selected).unwrap_or(0);
    let new = (cur + i64::from(delta)).rem_euclid(i64::try_from(total).unwrap_or(1));
    *selected = usize::try_from(new).unwrap_or(0);
}

fn cancel_search(app: &mut AppState) {
    if let ActionState::Searching { prev_nav_idx, .. } = app.action_state {
        app.nav_idx = prev_nav_idx.min(app.nav_items.len().saturating_sub(1));
    }
    app.action_state = ActionState::None;
}

fn commit_search_selection(app: &mut AppState) {
    let (key, was_jira) = {
        let ActionState::Searching {
            ref local_results,
            ref jira_state,
            selected,
            ..
        } = app.action_state
        else {
            return;
        };
        let local_len = local_results.len();
        if selected < local_len {
            (local_results[selected].issue_key.clone(), false)
        } else {
            let jira_idx = selected - local_len;
            match jira_state {
                JiraSearchState::Loaded { hits, issues } => {
                    let key = hits.get(jira_idx).map(|h| h.issue_key.clone());
                    let is_in_local = key
                        .as_deref()
                        .is_some_and(|k| app.issues.iter().any(|i| i.key() == k));
                    if let Some(k) = key {
                        if !is_in_local
                            && let Some(issue) = issues.iter().find(|i| i.key == k).cloned()
                        {
                            inject_jira_search_result(app, issue);
                        }
                        (k, true)
                    } else {
                        return;
                    }
                }
                _ => return,
            }
        }
    };

    // Park the search overlay so q/Esc from the issue view can restore it with
    // the same query, filters, results, and selection.
    app.saved_search = Some(Box::new(std::mem::replace(
        &mut app.action_state,
        ActionState::None,
    )));
    // Focus the picked issue in the main list.
    if let Some(pos) = app.nav_items.iter().position(
        |n| matches!(n, NavItem::Issue(idx) if app.issues.get(*idx).is_some_and(|i| i.key() == key)),
    ) {
        app.nav_idx = pos;
    }
    app.focused_panel = FocusedPanel::Detail;
    app.detail_scroll = 0;
    app.fullscreen_detail = true;
    request_detail_load_if_partial(app);
    let _ = was_jira;
}

const SEARCH_RESULTS_SOURCE_ID: &str = "_search_results";

/// Inject a Jira-only search hit into the team's local list under a synthetic
/// "Search Results" source group so the issue stays navigable + actionable.
fn inject_jira_search_result(app: &mut AppState, issue: Issue) {
    let mut item = WorkItem::Jira(issue);
    item.set_source(SEARCH_RESULTS_SOURCE_ID.into(), 0);
    if let Some(SourceState::Loaded(existing)) = app.sources.get_mut(SEARCH_RESULTS_SOURCE_ID) {
        if !existing.iter().any(|i| i.key() == item.key()) {
            existing.push(item);
        }
    } else {
        app.sources.insert(
            SEARCH_RESULTS_SOURCE_ID.into(),
            SourceState::Loaded(vec![item]),
        );
    }
    app.rebuild_issues();
}

/// Refresh is allowed only when no edit/picker/in-flight action is active.
/// `KeybindingsHelp` and `Error` overlays are view-only and don't block.
const fn refresh_allowed(state: &ActionState) -> bool {
    matches!(
        state,
        ActionState::None | ActionState::KeybindingsHelp | ActionState::Error { .. }
    )
}

const fn key_refresh_all(app: &mut AppState) {
    if !refresh_allowed(&app.action_state) {
        return;
    }
    app.pending_refresh_all = true;
}

/// Request a full fetch of every partially-loaded (board-trimmed) issue, so
/// their detail is ready without opening each card. Handled in
/// `dispatch_background_tasks`.
const fn key_preload_details(app: &mut AppState) {
    if !refresh_allowed(&app.action_state) {
        return;
    }
    app.pending_preload = true;
}

fn key_refresh_focused(app: &mut AppState) {
    if !refresh_allowed(&app.action_state) {
        return;
    }
    match app.focused_panel {
        FocusedPanel::List => key_refresh_all(app),
        FocusedPanel::Detail => key_refresh_current_issue(app),
    }
}

fn key_refresh_current_issue(app: &mut AppState) {
    // Single-item refresh is a Jira-only capability (fetch by issue key).
    let Some(issue) = app.selected_issue() else {
        return;
    };
    let key = issue.key.clone();
    if app.refreshing_issues.contains(&key) {
        return;
    }
    let source_id = issue.source_id.clone();
    let subsource_idx = issue.subsource_idx;
    app.refreshing_issues.insert(key.clone());
    app.pending_refresh_issue = Some(RefreshIssueRequest {
        key,
        source_id,
        subsource_idx,
    });
}

/// When a card opened from a lazily-loaded board is only partially fetched
/// (board-display fields only), kick off a full fetch so its description,
/// comments, and custom fields fill in. No-op for already-full issues,
/// non-Jira items, or one already being refreshed.
pub fn request_detail_load_if_partial(app: &mut AppState) {
    let Some(issue) = app.selected_issue() else {
        return;
    };
    if !issue.partial {
        return;
    }
    let key = issue.key.clone();
    if app.refreshing_issues.contains(&key) {
        return;
    }
    let source_id = issue.source_id.clone();
    let subsource_idx = issue.subsource_idx;
    app.refreshing_issues.insert(key.clone());
    app.pending_refresh_issue = Some(RefreshIssueRequest {
        key,
        source_id,
        subsource_idx,
    });
}

fn key_nav_down(app: &mut AppState) {
    if app.focused_panel == FocusedPanel::Detail
        && matches!(app.view_mode, ViewMode::Default | ViewMode::Custom(_))
    {
        let view_cfg = crate::tui::views::custom::current_view_config(app);
        let num_fields = crate::tui::views::custom::num_view_fields(view_cfg, app.selected_item());
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
        let has_widgets = app.selected_item().is_none_or(WorkItem::supports_comments);
        app.detail_focus = match &app.detail_focus {
            DetailFocus::Comments | DetailFocus::Attachments => DetailFocus::Comments,
            DetailFocus::Field(0) => {
                if has_widgets {
                    DetailFocus::Attachments
                } else {
                    DetailFocus::Field(0)
                }
            }
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

/// The first focusable detail element: the Comments widget for Jira issues,
/// the first field for items without comment/attachment widgets.
fn first_detail_focus(item: Option<&WorkItem>) -> DetailFocus {
    if item.is_none_or(WorkItem::supports_comments) {
        DetailFocus::Comments
    } else {
        DetailFocus::Field(0)
    }
}

fn key_jump_first(app: &mut AppState) {
    if board_mode_active(app) {
        board_jump(app, crate::tui::board::BoardMove::Top);
        return;
    }
    if app.focused_panel == FocusedPanel::Detail {
        if matches!(app.view_mode, ViewMode::Default | ViewMode::Custom(_)) {
            app.detail_focus = first_detail_focus(app.selected_item());
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
    if board_mode_active(app) {
        board_jump(app, crate::tui::board::BoardMove::Bottom);
        return;
    }
    if app.focused_panel == FocusedPanel::Detail {
        if matches!(app.view_mode, ViewMode::Default | ViewMode::Custom(_)) {
            let view_cfg = crate::tui::views::custom::current_view_config(app);
            let num_fields =
                crate::tui::views::custom::num_view_fields(view_cfg, app.selected_item());
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

/// `t` — "change status": Jira issues open the transition picker; Confluence
/// tasks get a mark-complete confirmation.
fn key_change_status(app: &mut AppState) {
    let Some(item) = app.selected_item() else {
        return;
    };
    if item.supports_complete() {
        if let Some(task) = item.as_confluence()
            && task.status == crate::confluence::types::TaskStatus::Incomplete
        {
            app.action_state = ActionState::ConfirmingCompleteTask {
                item_key: item.key().to_owned(),
                task_id: task.id.clone(),
                selected: 1, // default to No
            };
        }
        return;
    }
    if let Some(issue) = item.as_jira() {
        app.action_state = ActionState::LoadingTransitions {
            issue_key: issue.key.clone(),
        };
    }
}

fn handle_complete_task_confirm_input(app: &mut AppState, event: &crossterm::event::Event) {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let Event::Key(KeyEvent {
        code, modifiers, ..
    }) = *event
    else {
        return;
    };
    let ActionState::ConfirmingCompleteTask {
        item_key,
        task_id,
        selected,
    } = &app.action_state.clone()
    else {
        return;
    };
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            app.should_quit = true;
        }
        (KeyCode::Esc | KeyCode::Char('q'), _) => {
            app.action_state = ActionState::None;
        }
        (KeyCode::Left | KeyCode::Char('h' | 'l') | KeyCode::Right | KeyCode::Tab, _) => {
            app.action_state = ActionState::ConfirmingCompleteTask {
                item_key: item_key.clone(),
                task_id: task_id.clone(),
                selected: 1 - selected,
            };
        }
        (KeyCode::Enter, _) => {
            if *selected == 0 {
                app.action_state = ActionState::PendingCompleteTask {
                    item_key: item_key.clone(),
                    task_id: task_id.clone(),
                };
            } else {
                app.action_state = ActionState::None;
            }
        }
        _ => {}
    }
}

fn key_hide(app: &mut AppState) {
    if let Some(item) = app.selected_item() {
        let key = item.key().to_owned();
        let can_hide = item
            .source_id()
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
    if let Some(item) = app.selected_item() {
        let item = item.clone();
        app.view_mode = auto_view_mode(&item, app.team_config());
    }
    app.detail_scroll = 0;
    app.overlay = None;
    app.detail_focus = first_detail_focus(app.selected_item());
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
    let Some(item) = app.selected_item() else {
        return;
    };
    let item = item.clone();

    // Nav widget: open as popup overlay (one layer deeper). Only Jira items
    // have these widgets.
    match &app.detail_focus {
        DetailFocus::Comments => {
            if item.supports_comments() {
                app.overlay = Some(SubView::Comments);
                app.overlay_scroll = 0;
            }
            return;
        }
        DetailFocus::Attachments => {
            if item.supports_comments() {
                app.overlay = Some(SubView::Attachments);
                app.overlay_scroll = 0;
                app.overlay_focused_attachment = 0;
                maybe_fetch_attachment_preview(app);
            }
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
        crate::tui::views::custom::view_editable_field_spec(view_cfg, &item, field_idx);

    if field_id.is_empty() {
        return;
    }

    let field_cfg = crate::tui::views::custom::view_field_cfg(view_cfg, Some(&item), field_idx);

    // Readonly fields: open URL if the value is a link, otherwise do nothing.
    // Items without field editing (non-Jira) treat every field as readonly.
    // Slack URLs are opened in the Slack desktop app by default.
    if field_cfg.as_ref().and_then(|f| f.readonly).unwrap_or(false) || !item.supports_field_edit() {
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
        let cfg_kind = field_cfg
            .as_ref()
            .and_then(crate::config::types::CustomViewFieldConfig::effective_type);
        let schema_ty = app.field_schemas.get(&field_id).map(|s| s.ty.as_str());
        let by_schema = matches!(schema_ty, Some("date" | "datetime"));
        if cfg_kind.is_some() || by_schema {
            // Jira `date` fields take `yyyy-MM-dd`; the schema wins over the
            // config flags so we never send a rejected format. The flags
            // decide only when no schema is available.
            let date_only = match schema_ty {
                Some("date") => true,
                Some("datetime") => false,
                _ => cfg_kind == Some(crate::config::types::FieldType::Date),
            };
            let tz = crate::tui::views::custom::resolve_tz(view_cfg);
            let picker = crate::tui::overlays::datetime_picker::DatetimePicker::from_value(
                &original_json,
                tz,
                date_only,
            );
            app.action_state = ActionState::EditingDatetimeField {
                issue_key: item.key().to_owned(),
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
                issue_key: item.key().to_owned(),
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
            issue_key: item.key().to_owned(),
            field_id,
            current_value,
            original_json,
        };
        return;
    }

    set_detail_edit_state(
        app,
        item.key().to_owned(),
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

/// Input for the board column picker: `j/k` skip unreachable columns,
/// `Enter` fires the mapped transition, `t` swaps to the raw transition
/// list (same fetched data), `Esc` cancels.
fn handle_board_column_input(app: &mut AppState, event: &crossterm::event::Event) {
    use crossterm::event::{Event, KeyCode, KeyEvent};
    let ActionState::SelectingBoardColumn {
        ref issue_key,
        ref transitions,
        ref columns,
        ref mut selected,
    } = app.action_state
    else {
        return;
    };

    let reachable_towards = |from: usize, down: bool| -> Option<usize> {
        if down {
            (from + 1..columns.len()).find(|&i| columns[i].transition_id.is_some())
        } else {
            (0..from)
                .rev()
                .find(|&i| columns[i].transition_id.is_some())
        }
    };

    if let Event::Key(KeyEvent { code, .. }) = *event {
        match code {
            KeyCode::Esc => {
                app.action_state = ActionState::None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(next) = reachable_towards(*selected, true) {
                    *selected = next;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(prev) = reachable_towards(*selected, false) {
                    *selected = prev;
                }
            }
            KeyCode::Char('t') => {
                let key = issue_key.clone();
                let transitions = transitions.clone();
                app.action_state = ActionState::SelectingTransition {
                    issue_key: key,
                    transitions,
                    selected: 0,
                };
            }
            KeyCode::Enter => {
                if let Some(transition_id) = columns[*selected].transition_id.clone() {
                    let key = issue_key.clone();
                    app.action_state = ActionState::PendingTransition {
                        issue_key: key,
                        transition_id,
                    };
                }
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

fn handle_error_input(app: &mut AppState, event: &crossterm::event::Event) {
    use crossterm::event::{Event, KeyCode, KeyEvent};
    let max_scroll = u16::try_from(
        app.last_confirm_content_h
            .saturating_sub(app.last_confirm_viewport_h),
    )
    .unwrap_or(u16::MAX);
    let ActionState::Error { ref mut scroll, .. } = app.action_state else {
        return;
    };
    let Event::Key(KeyEvent { code, .. }) = event else {
        return;
    };
    match code {
        KeyCode::Char('q') | KeyCode::Esc | KeyCode::Enter => {
            app.action_state = ActionState::None;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            *scroll = scroll.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            *scroll = scroll.saturating_add(1).min(max_scroll);
        }
        KeyCode::PageUp => {
            *scroll = scroll.saturating_sub(10);
        }
        KeyCode::PageDown => {
            *scroll = scroll.saturating_add(10).min(max_scroll);
        }
        _ => {}
    }
}

fn handle_confirm_field_edit_input(app: &mut AppState, event: &crossterm::event::Event) {
    use crossterm::event::{Event, KeyCode, KeyEvent};
    let max_scroll = u16::try_from(
        app.last_confirm_content_h
            .saturating_sub(app.last_confirm_viewport_h),
    )
    .unwrap_or(u16::MAX);
    let ActionState::ConfirmingFieldEdit {
        ref issue_key,
        ref field_id,
        ref new_value,
        ref mut tab,
        ref mut scroll,
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
            *scroll = 0;
        }
        KeyCode::Left | KeyCode::Char('h') => {
            if *tab != 0 {
                *tab = 0;
                *scroll = 0;
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            if *tab != 1 {
                *tab = 1;
                *scroll = 0;
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            *scroll = scroll.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            *scroll = scroll.saturating_add(1).min(max_scroll);
        }
        KeyCode::PageUp => {
            *scroll = scroll.saturating_sub(10);
        }
        KeyCode::PageDown => {
            *scroll = scroll.saturating_add(10).min(max_scroll);
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

#[allow(clippy::too_many_lines)]
fn handle_offering_template_input(app: &mut AppState, event: &crossterm::event::Event) {
    use crossterm::event::{Event, KeyCode, KeyEvent};

    let max_scroll = u16::try_from(
        app.last_confirm_content_h
            .saturating_sub(app.last_confirm_viewport_h),
    )
    .unwrap_or(u16::MAX);
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
            KeyCode::Char('y' | 'a') | KeyCode::Enter => {
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
            KeyCode::Char('n' | 'd') => {
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
            KeyCode::Up | KeyCode::Char('k') => {
                *scroll = scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *scroll = scroll.saturating_add(1).min(max_scroll);
            }
            KeyCode::PageUp => {
                *scroll = scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                *scroll = scroll.saturating_add(10).min(max_scroll);
            }
            _ => {}
        }
    } else {
        // Dialog mode with template selection
        match code {
            KeyCode::Char('y' | 'a') | KeyCode::Enter => {
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
            KeyCode::Char('n' | 'd') => {
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
            KeyCode::Char('q' | 'c') | KeyCode::Esc => {
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

pub fn edit_text(input: &mut String, cursor: &mut usize, code: KeyCode) {
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
            // Date-only pickers skip the time steps and commit immediately.
            if !picker.date_only && picker.mode == DatetimePickerMode::Date {
                picker.mode = DatetimePickerMode::Time;
                return;
            }
            if !picker.date_only
                && picker.time_focus == crate::tui::overlays::datetime_picker::TimeFocus::Hour
            {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jira::types::{IssueFields, IssueTypeField, ProjectField, StatusField};

    fn make_item(key: &str, status: &str, source_id: Option<&str>) -> WorkItem {
        WorkItem::Jira(make_issue(key, status, source_id))
    }

    fn item_status(item: &WorkItem) -> &str {
        item.status_name()
    }

    use crate::config::types as cfg;

    fn resolved_team(id: &str, sources: Vec<cfg::SourceConfig>) -> ResolvedTeam {
        ResolvedTeam {
            id: id.into(),
            path: "/tmp".into(),
            config: TeamConfig {
                sources,
                ..Default::default()
            },
            jira: cfg::JiraConfig::default(),
            confluence: cfg::JiraConfig::default(),
            open_slack_in_app: true,
            slack_team_id: None,
        }
    }

    fn board_source(id: &str) -> cfg::SourceConfig {
        cfg::SourceConfig {
            id: id.into(),
            kind: cfg::SourceKind::Board,
            board: Some(cfg::BoardFilters {
                board_id: 1,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn jira_source(id: &str) -> cfg::SourceConfig {
        cfg::SourceConfig {
            id: id.into(),
            ..Default::default()
        }
    }

    fn backlog_source(id: &str) -> cfg::SourceConfig {
        cfg::SourceConfig {
            id: id.into(),
            kind: cfg::SourceKind::Backlog,
            board: Some(cfg::BoardFilters {
                board_id: 1,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn backlog_source_gets_its_own_tab_and_stays_out_of_the_list() {
        let teams = vec![resolved_team(
            "platform",
            vec![jira_source("mine"), backlog_source("bl")],
        )];
        let mut app = AppState::new(teams, &cfg::Config::default());
        assert_eq!(app.tab_list(), vec![(0, None), (0, Some("bl".into()))]);

        // On the list tab the backlog source is excluded…
        assert_eq!(app.board_view, None);
        assert!(app.source_in_active_tab("mine"));
        assert!(!app.source_in_active_tab("bl"));

        // …and on the backlog tab it is the only source, in list (not board) mode.
        app.activate_tab(1);
        assert_eq!(app.board_view.as_deref(), Some("bl"));
        assert!(app.source_in_active_tab("bl"));
        assert!(!app.source_in_active_tab("mine"));
        assert_eq!(app.active_tab_source_kind(), Some(cfg::SourceKind::Backlog));
        assert!(!board_mode_active(&app));
    }

    #[test]
    fn backlog_only_team_gets_no_list_tab() {
        let teams = vec![resolved_team("groom", vec![backlog_source("bl")])];
        let app = AppState::new(teams, &cfg::Config::default());
        assert_eq!(app.tab_list(), vec![(0, Some("bl".into()))]);
        assert_eq!(app.board_view.as_deref(), Some("bl"));
    }

    #[test]
    fn board_only_team_gets_no_list_tab() {
        let teams = vec![
            resolved_team("platform", vec![jira_source("mine"), board_source("inc")]),
            resolved_team("inc-board", vec![board_source("inc_board")]),
        ];
        let app = AppState::new(teams, &cfg::Config::default());
        assert_eq!(
            app.tab_list(),
            vec![
                (0, None),
                (0, Some("inc".into())),
                (1, Some("inc_board".into())),
            ],
        );
    }

    #[test]
    fn sourceless_team_keeps_its_list_tab() {
        let teams = vec![resolved_team("empty", Vec::new())];
        let app = AppState::new(teams, &cfg::Config::default());
        assert_eq!(app.tab_list(), vec![(0, None)]);
        assert_eq!(app.board_view, None);
    }

    #[test]
    fn board_only_first_team_starts_on_its_board() {
        let teams = vec![resolved_team("inc-board", vec![board_source("b1")])];
        let app = AppState::new(teams, &cfg::Config::default());
        assert_eq!(app.board_view.as_deref(), Some("b1"));
        assert_eq!(app.active_tab_index(), 0);
    }

    #[test]
    fn switching_to_board_only_team_lands_on_its_board() {
        let teams = vec![
            resolved_team("platform", vec![jira_source("mine")]),
            resolved_team("inc-board", vec![board_source("b1")]),
        ];
        let mut app = AppState::new(teams, &cfg::Config::default());
        assert_eq!(app.active_tab_index(), 0);
        app.activate_tab(1);
        assert_eq!(app.active_team_idx, 1);
        assert_eq!(app.board_view.as_deref(), Some("b1"));
        assert_eq!(app.active_tab_index(), 1);
        // And back to the list team.
        app.activate_tab(0);
        assert_eq!(app.active_team_idx, 0);
        assert_eq!(app.board_view, None);
    }

    fn make_issue(key: &str, status: &str, source_id: Option<&str>) -> Issue {
        Issue {
            id: format!("id-{key}"),
            key: key.to_string(),
            fields: IssueFields {
                summary: format!("Summary {key}"),
                status: StatusField {
                    id: "s1".into(),
                    name: status.into(),
                },
                priority: None,
                assignee: None,
                reporter: None,
                issuetype: IssueTypeField {
                    id: "t1".into(),
                    name: "Task".into(),
                },
                project: ProjectField {
                    id: "p1".into(),
                    key: "PROJ".into(),
                    name: "Project".into(),
                },
                description: None,
                comment: None,
                attachment: None,
                extra: HashMap::new(),
            },
            source_id: source_id.map(str::to_string),
            subsource_idx: 0,
            partial: false,
        }
    }

    #[test]
    fn write_transitioned_status_updates_id_and_name() {
        let mut issues = vec![make_item("A-1", "To Do", None)];
        let target = StatusField {
            id: "s42".into(),
            name: "In Progress".into(),
        };
        write_transitioned_status(&mut issues, "A-1", Some(&target));

        let WorkItem::Jira(issue) = &issues[0] else {
            panic!("expected Jira item");
        };
        assert_eq!(issue.fields.status.id, "s42");
        assert_eq!(issue.fields.status.name, "In Progress");

        // None payload (transition lookup failed) leaves the status untouched.
        write_transitioned_status(&mut issues, "A-1", None);
        assert_eq!(issue_status(&issues, "A-1"), ("s42", "In Progress"));
    }

    fn issue_status<'a>(issues: &'a [WorkItem], key: &str) -> (&'a str, &'a str) {
        let issue = issues
            .iter()
            .find(|i| i.key() == key)
            .and_then(WorkItem::as_jira)
            .unwrap();
        (
            issue.fields.status.id.as_str(),
            issue.fields.status.name.as_str(),
        )
    }

    /// A team with a normal list source plus a backlog of B-1..B-3, with the
    /// backlog tab active.
    fn backlog_app() -> AppState {
        let teams = vec![resolved_team(
            "platform",
            vec![jira_source("mine"), backlog_source("bl")],
        )];
        let mut app = AppState::new(teams, &cfg::Config::default());
        app.sources.insert(
            "mine".into(),
            SourceState::Loaded(vec![make_item("M-1", "To Do", Some("mine"))]),
        );
        app.sources.insert(
            "bl".into(),
            SourceState::Loaded(vec![
                make_item("B-1", "To Do", Some("bl")),
                make_item("B-2", "To Do", Some("bl")),
                make_item("B-3", "To Do", Some("bl")),
            ]),
        );
        app.rebuild_issues();
        app.activate_tab(1);
        assert_eq!(app.board_view.as_deref(), Some("bl"));
        app
    }

    fn select_key(app: &mut AppState, key: &str) {
        let pos = app
            .nav_items
            .iter()
            .position(|n| {
                matches!(n, NavItem::Issue(i) if app.issues.get(*i).map(WorkItem::key) == Some(key))
            })
            .expect("key not navigable");
        app.nav_idx = pos;
    }

    fn source_keys(app: &AppState, source_id: &str) -> Vec<String> {
        let Some(SourceState::Loaded(items)) = app.sources.get(source_id) else {
            panic!("expected Loaded");
        };
        items.iter().map(|i| i.key().to_owned()).collect()
    }

    #[test]
    fn rank_move_reorders_both_lists_and_selection_follows() {
        let mut app = backlog_app();
        select_key(&mut app, "B-2");
        key_rank_move(&mut app, true);

        assert_eq!(source_keys(&app, "bl"), ["B-2", "B-1", "B-3"]);
        // On a backlog tab the flat list holds only the backlog source.
        let flat: Vec<&str> = app.issues.iter().map(WorkItem::key).collect();
        assert_eq!(flat, ["B-2", "B-1", "B-3"]);
        assert_eq!(app.selected_item().map(WorkItem::key), Some("B-2"));

        let pending = app.pending_rank.as_ref().expect("pending rank");
        assert_eq!(pending.source_id, "bl");
        assert_eq!(pending.issue_key, "B-2");
        assert_eq!(
            pending.anchor,
            crate::jira::types::RankAnchor::Before("B-1".into())
        );
        assert!(app.rank_flush_queue.is_empty());
    }

    #[test]
    fn rank_moves_of_one_issue_collapse_into_the_latest_anchor() {
        let mut app = backlog_app();
        select_key(&mut app, "B-2");
        key_rank_move(&mut app, true);
        key_rank_move(&mut app, false); // back where it started

        assert_eq!(source_keys(&app, "bl"), ["B-1", "B-2", "B-3"]);
        let pending = app.pending_rank.as_ref().expect("pending rank");
        assert_eq!(
            pending.anchor,
            crate::jira::types::RankAnchor::After("B-1".into())
        );
        assert!(
            app.rank_flush_queue.is_empty(),
            "same-issue moves must not queue extra API calls"
        );
    }

    #[test]
    fn moving_a_different_issue_flushes_the_pending_one() {
        let mut app = backlog_app();
        select_key(&mut app, "B-2");
        key_rank_move(&mut app, true);
        select_key(&mut app, "B-3");
        key_rank_move(&mut app, true);

        assert_eq!(app.rank_flush_queue.len(), 1);
        assert_eq!(app.rank_flush_queue[0].issue_key, "B-2");
        assert_eq!(
            app.pending_rank.as_ref().map(|p| p.issue_key.as_str()),
            Some("B-3")
        );
    }

    #[test]
    fn rank_move_at_the_edge_is_a_no_op() {
        let mut app = backlog_app();
        select_key(&mut app, "B-1");
        key_rank_move(&mut app, true);

        assert_eq!(source_keys(&app, "bl"), ["B-1", "B-2", "B-3"]);
        assert!(app.pending_rank.is_none());
    }

    #[test]
    fn switching_teams_flushes_the_pending_rank() {
        let teams = vec![
            resolved_team("platform", vec![jira_source("mine"), backlog_source("bl")]),
            resolved_team("other", vec![jira_source("theirs")]),
        ];
        let mut app = AppState::new(teams, &cfg::Config::default());
        app.sources.insert(
            "bl".into(),
            SourceState::Loaded(vec![
                make_item("B-1", "To Do", Some("bl")),
                make_item("B-2", "To Do", Some("bl")),
            ]),
        );
        app.rebuild_issues();
        app.activate_tab(1); // platform's backlog tab
        select_key(&mut app, "B-2");
        key_rank_move(&mut app, true);
        assert!(app.pending_rank.is_some());

        app.switch_team(1);
        assert!(app.pending_rank.is_none());
        assert_eq!(app.rank_flush_queue.len(), 1);
        assert_eq!(app.rank_flush_queue[0].issue_key, "B-2");
    }

    #[test]
    fn moved_to_sprint_leaves_both_lists_and_clamps_selection() {
        let mut app = backlog_app();
        select_key(&mut app, "B-3"); // last item
        app.cache.enabled = false; // no disk writes from a unit test
        apply_moved_to_sprint(&mut app, "B-3");

        assert_eq!(source_keys(&app, "bl"), ["B-1", "B-2"]);
        let flat: Vec<&str> = app.issues.iter().map(WorkItem::key).collect();
        assert_eq!(flat, ["B-1", "B-2"]);
        assert!(app.nav_idx < app.nav_items.len());
        assert!(matches!(app.action_state, ActionState::None));
    }

    #[test]
    fn sprints_loaded_starts_on_the_first_active_sprint() {
        let mut app = backlog_app();
        let sprint = |id: u64, name: &str, state: &str| crate::jira::types::Sprint {
            id,
            name: name.into(),
            state: state.into(),
        };
        apply_sprints_loaded(
            &mut app,
            "B-1".into(),
            Some(vec![
                sprint(1, "Sprint 1", "future"),
                sprint(2, "Sprint 2", "active"),
                sprint(3, "Sprint 3", "future"),
            ]),
        );
        let ActionState::SelectingSprint { selected, .. } = app.action_state else {
            panic!("expected sprint picker");
        };
        assert_eq!(selected, 1);
    }

    #[test]
    fn sprints_loaded_explains_kanban_and_empty_boards() {
        let mut app = backlog_app();
        apply_sprints_loaded(&mut app, "B-1".into(), None);
        assert!(matches!(app.action_state, ActionState::Error { .. }));

        app.action_state = ActionState::None;
        apply_sprints_loaded(&mut app, "B-1".into(), Some(Vec::new()));
        assert!(matches!(app.action_state, ActionState::Error { .. }));
    }

    #[test]
    fn apply_issue_refresh_replaces_in_both_lists() {
        let src = "src-1";
        let mut issues = vec![
            make_item("A-1", "To Do", Some(src)),
            make_item("A-2", "To Do", Some(src)),
        ];
        let mut sources = IndexMap::new();
        sources.insert(src.to_string(), SourceState::Loaded(issues.clone()));

        let refreshed = make_item("A-1", "Done", Some(src));
        apply_issue_refresh(&mut issues, &mut sources, refreshed);

        assert_eq!(item_status(&issues[0]), "Done");
        assert_eq!(item_status(&issues[1]), "To Do");
        let SourceState::Loaded(src_issues) = sources.get(src).unwrap() else {
            panic!("expected Loaded");
        };
        assert_eq!(item_status(&src_issues[0]), "Done");
        assert_eq!(item_status(&src_issues[1]), "To Do");
    }

    #[test]
    fn apply_issue_refresh_silently_drops_missing_key() {
        let src = "src-1";
        let mut issues = vec![make_item("A-1", "To Do", Some(src))];
        let mut sources = IndexMap::new();
        sources.insert(src.to_string(), SourceState::Loaded(issues.clone()));

        let refreshed = make_item("Z-9", "Done", Some(src));
        apply_issue_refresh(&mut issues, &mut sources, refreshed);

        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].key(), "A-1");
        let SourceState::Loaded(src_issues) = sources.get(src).unwrap() else {
            panic!("expected Loaded");
        };
        assert_eq!(src_issues.len(), 1);
        assert_eq!(src_issues[0].key(), "A-1");
    }

    #[test]
    fn apply_issue_refresh_skips_source_when_not_loaded() {
        let src = "src-1";
        let mut issues = vec![make_item("A-1", "To Do", Some(src))];
        let mut sources = IndexMap::new();
        sources.insert(src.to_string(), SourceState::Loading);

        let refreshed = make_item("A-1", "Done", Some(src));
        apply_issue_refresh(&mut issues, &mut sources, refreshed);

        // Flat list updated, source state untouched (still Loading).
        assert_eq!(item_status(&issues[0]), "Done");
        assert!(matches!(sources.get(src), Some(SourceState::Loading)));
    }

    #[test]
    fn create_error_returns_to_form_with_inline_error() {
        let form = crate::tui::overlays::create_issue::CreateForm::open(
            ProjectField {
                id: "p1".into(),
                key: "PROJ".into(),
                name: "Project".into(),
            },
            Vec::new(),
        );
        let state = error_action_state(
            ActionState::AwaitingCreate { form },
            anyhow::anyhow!("Create issue failed 400: workflow validator rejected"),
        );
        let ActionState::CreatingIssue(form) = state else {
            panic!("expected CreatingIssue, got a different state");
        };
        assert!(
            form.error
                .as_deref()
                .unwrap()
                .contains("workflow validator rejected")
        );
    }

    #[test]
    fn non_create_error_shows_error_overlay() {
        let state = error_action_state(
            ActionState::AwaitingAction {
                description: "Applying transition…".into(),
            },
            anyhow::anyhow!("boom"),
        );
        assert!(matches!(state, ActionState::Error { .. }));
    }
}
