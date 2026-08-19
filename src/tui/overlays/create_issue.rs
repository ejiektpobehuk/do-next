//! Create-issue form: a single modal overlay that builds a new Jira issue.
//!
//! Field metadata is fetched lazily via the new split createmeta endpoints
//! (`get_create_issuetypes` / `get_create_fields`). Each field's Jira schema
//! type is mapped to a reusable input widget. The pure transforms
//! (`parse_create_fields`, `schema_to_widget`, `build_create_payload`,
//! `distinct_projects`) are unit-tested below.

use std::collections::HashSet;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Alignment,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use serde_json::{Value, json};

use crate::jira::types::{
    FieldSchema, IssueLinkType, IssueRef, IssueTypeField, ProjectField, ProjectInfo, UserField,
};
use crate::tui::app::{ActionState, AppState, CacheState, edit_text};
use crate::tui::overlays::datetime_picker::{
    self, DatetimePicker, DatetimePickerMode, TimeFocus, handle_date_key, handle_time_key,
};
use crate::tui::theme;

// ── Data model ────────────────────────────────────────────────────────────────

/// How a single field is rendered/edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetKind {
    /// Single-line plain string.
    Text,
    /// Rich text (ADF); converted via `markdown_to_adf` on submit.
    RichText,
    Number,
    /// Date only (`yyyy-MM-dd`), edited via the shared picker in date-only mode.
    Date,
    /// Date with time, edited via the full datetime picker.
    DateTime,
    /// Single-select from `allowedValues`.
    Select,
    /// Multi-select array from `allowedValues`.
    MultiSelect,
    /// A single user with no `allowedValues` to pick from. There is no user
    /// search here yet, so the only value it can take is the current user;
    /// `reporter` starts prefilled with them.
    User,
    /// The issue's epic — the system `parent` link or the legacy Epic Link
    /// custom field. Picked from a project-scoped search, like `User`.
    Epic,
    /// The `issuelinks` field: any number of (relation, issue) pairs. Unlike
    /// every other widget its value does not go in `fields` — Jira only accepts
    /// links on create as `update.issuelinks` add operations.
    IssueLinks,
    /// A field type the TUI can't edit; read-only, blocks submit if required.
    Unsupported,
}

/// A selectable option for a select/multi-select field. `raw` is the original
/// `allowedValue` object so submit re-emits exactly the shape Jira expects.
#[derive(Debug, Clone)]
pub struct CreateOption {
    pub label: String,
    pub raw: Value,
}

/// The in-progress value of a single form field.
#[derive(Debug, Clone)]
pub enum FieldValue {
    Text {
        input: String,
        cursor: usize,
    },
    Number {
        input: String,
        cursor: usize,
    },
    Date {
        value: Option<String>,
    },
    SingleOption(Option<usize>),
    MultiOption(HashSet<usize>),
    /// `None` means "leave to Jira" — for `reporter` that resolves to the creator.
    User(Option<UserField>),
    /// `None` means the issue is created outside any epic.
    Epic(Option<IssueRef>),
    /// The links to create alongside the issue, in the order they were added.
    IssueLinks(Vec<IssueLinkDraft>),
    Unsupported,
}

/// Which half of a link type's relation the new issue is on. Jira stores a link
/// as an (inward, outward) pair, so the direction decides which side the picked
/// issue goes on: "blocks" is the outward half of `Blocks`, "is blocked by" the
/// inward one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkDirection {
    Outward,
    Inward,
}

/// One pickable relation: a link type seen from one end. Every link type yields
/// two of these, since "blocks" and "is blocked by" are different choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTypeChoice {
    /// The link type's name, which is what the payload references.
    pub name: String,
    /// How this end reads ("blocks", "is blocked by") — the picker's label.
    pub label: String,
    pub direction: LinkDirection,
}

/// A link the form will create once the issue exists: a relation plus the issue
/// on the other end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueLinkDraft {
    pub link_type: LinkTypeChoice,
    pub issue: IssueRef,
}

impl IssueLinkDraft {
    /// Relation first, then the issue it points at: "blocks  OPS-42  Fix login".
    fn display(&self) -> String {
        format!("{}  {}", self.link_type.label, self.issue.display())
    }
}

/// One renderable field in the create form.
#[derive(Debug, Clone)]
pub struct FormField {
    pub field_id: String,
    pub label: String,
    pub required: bool,
    pub widget: WidgetKind,
    pub value: FieldValue,
    pub options: Vec<CreateOption>,
}

/// A nested picker overlay opened on top of the form.
#[derive(Debug, Clone)]
pub enum CreatePicker {
    Project {
        query: String,
        query_cursor: usize,
        cursor: usize,
        searching: bool,
    },
    IssueType {
        cursor: usize,
    },
    Select {
        field_idx: usize,
        cursor: usize,
    },
    MultiSelect {
        field_idx: usize,
        cursor: usize,
    },
    Date {
        field_idx: usize,
        picker: DatetimePicker,
    },
    /// Assignee/reporter chooser. Unlike the project picker its list is not
    /// local: `query` is sent to Jira and the matches land in
    /// `CreateForm::user_search`.
    User {
        field_idx: usize,
        query: String,
        query_cursor: usize,
        cursor: usize,
        searching: bool,
    },
    /// Epic chooser. Server-backed in the same way as `User`, with its matches
    /// in `CreateForm::epic_search`.
    Epic {
        field_idx: usize,
        query: String,
        query_cursor: usize,
        cursor: usize,
        searching: bool,
    },
    /// The links added so far, where one is removed and another is started.
    /// Adding walks from here to `LinkType` and then to `LinkIssue`; each step
    /// back returns to the previous one, so this is the only picker whose
    /// successors know where they came from.
    IssueLinks {
        field_idx: usize,
        cursor: usize,
    },
    /// Relation chooser, opened either for a link being added — the first of the
    /// two steps — or to change the relation of one already in the list, which
    /// is what `editing` holds the index of.
    LinkType {
        field_idx: usize,
        editing: Option<usize>,
        cursor: usize,
    },
    /// Issue chooser for a link being added, once `link_type` is settled.
    /// Server-backed like `Epic`, with its matches in `CreateForm::link_search`.
    LinkIssue {
        field_idx: usize,
        link_type: LinkTypeChoice,
        query: String,
        query_cursor: usize,
        cursor: usize,
        searching: bool,
    },
}

/// A row of the user picker. The list is more than the search results: it
/// leads with a way to clear the field, and pins the current user while no
/// query narrows things down.
#[derive(Debug, Clone)]
pub enum UserRow {
    /// Clear the field and let Jira decide — for `reporter`, the creator.
    Unset,
    /// The authenticated user, pinned above the results.
    Me(UserField),
    Found(UserField),
}

/// A row of the epic picker: the matches, led by a way to create the issue
/// outside any epic. There is nothing to pin: unlike "me", no one epic is the
/// likely answer.
#[derive(Debug, Clone)]
pub enum EpicRow {
    Unset,
    Found(IssueRef),
}

/// A row of the linked-issues list: the links already added, then the way to add
/// another. "Add" trails because the links are the content and it is the action
/// under them — and on an empty list it is the only row either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRow {
    /// Index into the field's link vector.
    Existing(usize),
    Add,
}

/// Debounced state of a server-backed picker's search (users, epics). Present
/// only while such a picker is open; recreated on each open so results never
/// leak from one field (or project) to the next.
#[derive(Debug, Clone)]
pub struct PickerSearch<T> {
    /// Query the picker last settled on, already sent or waiting out the debounce.
    pub query: String,
    /// When `query` last changed. The dispatcher waits `PICKER_SEARCH_DEBOUNCE_MS`
    /// past this before spending a request on a half-typed name. `None` for the
    /// opening query: it is empty and nobody is mid-word, so it goes at once.
    pub changed_at: Option<std::time::Instant>,
    /// Whether the request for the current `token` has been spawned.
    pub spawned: bool,
    /// Bumped on every query change so responses to superseded queries are dropped.
    pub token: u64,
    /// Whether a response for the current `token` is still outstanding. Kept
    /// separate from `results` so the previous matches stay on screen while
    /// the next ones load, instead of the list blanking on every keystroke.
    pub pending: bool,
    pub results: CacheState<Vec<T>>,
}

impl<T> PickerSearch<T> {
    const fn new(token: u64) -> Self {
        Self {
            query: String::new(),
            changed_at: None,
            spawned: false,
            token,
            pending: true,
            results: CacheState::Loading,
        }
    }

    /// Restart the search for a changed query under a fresh token.
    fn requery(&mut self, query: String) {
        self.query = query;
        self.changed_at = Some(std::time::Instant::now());
        self.spawned = false;
        self.token += 1;
        self.pending = true;
    }
}

/// Whether the reporter still has to be prefilled with the current user.
/// `Done` guards against re-filling a reporter the user deliberately cleared
/// when `/myself` resolves after the field metadata does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefill {
    Pending,
    Done,
}

/// Form-level interaction mode. `Nav` uses Vim-style keys (j/k/q/Enter to
/// activate). `Edit` is entered when a text-input row is activated and routes
/// every `Char` through `edit_text`; `Esc` returns to `Nav`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormMode {
    Nav,
    Edit,
}

/// Full state of the create-issue form.
// The `needs_*_fetch` flags are four independent one-shot requests to the
// dispatcher, not a state to model: each is set where the thing is first needed
// and cleared by whoever sends it.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct CreateForm {
    pub project: ProjectField,
    pub available_projects: Vec<ProjectField>,
    pub issuetype: Option<IssueTypeField>,
    pub issuetypes: CacheState<Vec<IssueTypeField>>,
    pub fields: Vec<FormField>,
    pub fields_state: CacheState<()>,
    /// The authenticated Jira user, once known: the value user fields toggle to
    /// and what `reporter` is prefilled with. `None` until `/myself` lands.
    pub current_user: Option<UserField>,
    /// Whether the reporter prefill has already run for the current field set.
    pub prefill: Prefill,
    /// Live state of the assignee/reporter chooser; `None` unless one is open.
    pub user_search: Option<PickerSearch<UserField>>,
    /// Live state of the epic chooser; `None` unless it is open.
    pub epic_search: Option<PickerSearch<IssueRef>>,
    /// Live state of the linked-issue chooser; `None` unless it is open.
    pub link_search: Option<PickerSearch<IssueRef>>,
    /// The site's link relations, fetched the first time the relation chooser
    /// opens. Site-wide, so unlike the searches it survives a project change.
    pub link_types: CacheState<Vec<LinkTypeChoice>>,
    /// Flat focus index: 0=project, 1=issue type, 2..=fields, last=Create button.
    pub focus: usize,
    pub mode: FormMode,
    pub picker: Option<CreatePicker>,
    /// State of the exit-confirmation prompt. It has no buttons — the footer
    /// hints are the menu — so there is nothing to track but open/closed.
    pub discard_prompt: DiscardPrompt,
    /// Index into `fields` of the value waiting for `$EDITOR`. Set by `e`/Ctrl+E
    /// and cleared by the main loop: it owns the terminal, so it is the only
    /// place that can suspend the TUI to hand over to an external editor.
    pub pending_editor: Option<usize>,
    /// Generation guard; bumped on any project/issue-type change so stale
    /// metadata responses are dropped.
    pub meta_token: u64,
    pub needs_issuetype_fetch: bool,
    pub needs_field_fetch: bool,
    pub needs_projects_fetch: bool,
    pub needs_link_types_fetch: bool,
    pub error: Option<String>,
}

impl CreateForm {
    /// Build a form for `project`, kicking off the issue-type fetch.
    pub const fn open(project: ProjectField, available_projects: Vec<ProjectField>) -> Self {
        Self {
            project,
            available_projects,
            issuetype: None,
            issuetypes: CacheState::Loading,
            fields: Vec::new(),
            fields_state: CacheState::Idle,
            current_user: None,
            prefill: Prefill::Pending,
            user_search: None,
            epic_search: None,
            link_search: None,
            link_types: CacheState::Idle,
            focus: 1, // start on Issue Type (project is pre-filled)
            mode: FormMode::Nav,
            picker: None,
            discard_prompt: DiscardPrompt::Closed,
            pending_editor: None,
            meta_token: 1,
            needs_issuetype_fetch: true,
            needs_field_fetch: false,
            needs_projects_fetch: false,
            needs_link_types_fetch: false,
            error: None,
        }
    }

    /// Index of the synthetic "Create" button row.
    const fn button_idx(&self) -> usize {
        2 + self.fields.len()
    }

    const fn focus_count(&self) -> usize {
        self.button_idx() + 1
    }

    /// Switch to a new project: reset issue type + fields and re-fetch types.
    fn set_project(&mut self, project: ProjectField) {
        self.project = project;
        self.issuetype = None;
        self.issuetypes = CacheState::Loading;
        self.fields.clear();
        self.fields_state = CacheState::Idle;
        self.prefill = Prefill::Pending;
        // Assignable users and epics are both project-scoped, and the link
        // search falls back to the project when nothing is typed; the old
        // matches no longer apply. `link_types` is site-wide and stays.
        self.user_search = None;
        self.epic_search = None;
        self.link_search = None;
        self.focus = 1;
        self.mode = FormMode::Nav;
        self.meta_token += 1;
        self.needs_issuetype_fetch = true;
        self.error = None;
    }

    /// Switch to a new issue type and re-fetch its fields.
    fn set_issuetype(&mut self, it: IssueTypeField) {
        self.issuetype = Some(it);
        self.fields.clear();
        self.fields_state = CacheState::Loading;
        self.prefill = Prefill::Pending;
        self.meta_token += 1;
        self.needs_field_fetch = true;
        self.error = None;
    }
}

// ── Pure transforms (unit-tested) ──────────────────────────────────────────────

/// Append cached projects to `current`, skipping any whose uppercase key is
/// already present. `id` is left empty: the create payload only uses `key`.
pub fn merge_cached_projects(current: &mut Vec<ProjectField>, cached: &[ProjectInfo]) {
    let mut have: HashSet<String> = current.iter().map(|p| p.key.to_uppercase()).collect();
    for p in cached {
        if have.insert(p.key.to_uppercase()) {
            current.push(ProjectField {
                id: String::new(),
                key: p.key.clone(),
                name: p.name.clone(),
            });
        }
    }
}

/// Distinct projects across the loaded items, deduped by key, order preserved.
/// Only Jira items carry a project.
pub fn distinct_projects(items: &[crate::items::WorkItem]) -> Vec<ProjectField> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for issue in items.iter().filter_map(crate::items::WorkItem::as_jira) {
        if seen.insert(issue.fields.project.key.clone()) {
            out.push(issue.fields.project.clone());
        }
    }
    out
}

/// Map a Jira field `schema` object to a widget. `has_options` is whether the
/// field carries `allowedValues` (decides how `user` fields are handled).
/// `subtask` is whether the issue type being created is a sub-task, whose
/// `parent` is a standard issue and not something the epic picker can find.
pub fn schema_to_widget(schema: &Value, has_options: bool, subtask: bool) -> WidgetKind {
    let ty = schema.get("type").and_then(Value::as_str).unwrap_or("");
    let items = schema.get("items").and_then(Value::as_str);
    let field_schema = FieldSchema {
        ty: ty.to_string(),
        custom: schema
            .get("custom")
            .and_then(Value::as_str)
            .map(str::to_string),
        system: schema
            .get("system")
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    // Checked ahead of `ty`: an epic link is spelled `issuelink` on Cloud and
    // `any` on sites still carrying the Greenhopper field, and neither type is
    // otherwise editable here.
    if field_schema.is_epic_link() {
        return if subtask && field_schema.is_system_parent() {
            WidgetKind::Unsupported
        } else {
            WidgetKind::Epic
        };
    }
    match ty {
        "string" => {
            if field_schema.is_adf() {
                WidgetKind::RichText
            } else {
                WidgetKind::Text
            }
        }
        "number" => WidgetKind::Number,
        "date" => WidgetKind::Date,
        "datetime" => WidgetKind::DateTime,
        "option" | "priority" | "version" | "component" | "resolution" | "group" => {
            WidgetKind::Select
        }
        "user" => {
            if has_options {
                WidgetKind::Select
            } else {
                WidgetKind::User
            }
        }
        "array" => match items {
            Some("option" | "version" | "component" | "group") => WidgetKind::MultiSelect,
            Some("user") if has_options => WidgetKind::MultiSelect,
            Some("issuelinks") => WidgetKind::IssueLinks,
            _ => WidgetKind::Unsupported,
        },
        _ => WidgetKind::Unsupported,
    }
}

/// Display label for an `allowedValue` object.
fn option_label(raw: &Value) -> String {
    for key in ["value", "name", "label"] {
        if let Some(s) = raw.get(key).and_then(Value::as_str) {
            return s.to_string();
        }
    }
    raw.get("key")
        .and_then(Value::as_str)
        .or_else(|| raw.get("id").and_then(Value::as_str))
        .unwrap_or("?")
        .to_string()
}

/// Minimal identifying object Jira accepts for a select/user value.
fn option_ref(raw: &Value) -> Value {
    if let Some(obj) = raw.as_object() {
        for key in ["id", "value", "name", "key"] {
            if let Some(v) = obj.get(key) {
                return json!({ key: v });
            }
        }
    }
    raw.clone()
}

/// How Jira wants a user referenced in a create payload: `accountId` on Cloud,
/// `name` on the older Server/DC shape. `None` if the user carries neither.
fn user_ref(user: &UserField) -> Option<Value> {
    if let Some(id) = &user.account_id {
        return Some(json!({ "accountId": id }));
    }
    user.name.as_ref().map(|n| json!({ "name": n }))
}

/// How Jira wants an epic referenced. The system `parent` field takes an
/// issue object; the legacy Epic Link custom field takes the bare key.
fn epic_ref(field_id: &str, epic: &IssueRef) -> Value {
    if field_id == "parent" {
        json!({ "key": epic.key })
    } else {
        Value::String(epic.key.clone())
    }
}

fn initial_value(widget: WidgetKind) -> FieldValue {
    match widget {
        WidgetKind::Text | WidgetKind::RichText => FieldValue::Text {
            input: String::new(),
            cursor: 0,
        },
        WidgetKind::Number => FieldValue::Number {
            input: String::new(),
            cursor: 0,
        },
        WidgetKind::Date | WidgetKind::DateTime => FieldValue::Date { value: None },
        WidgetKind::Select => FieldValue::SingleOption(None),
        WidgetKind::MultiSelect => FieldValue::MultiOption(HashSet::new()),
        WidgetKind::User => FieldValue::User(None),
        WidgetKind::Epic => FieldValue::Epic(None),
        WidgetKind::IssueLinks => FieldValue::IssueLinks(Vec::new()),
        WidgetKind::Unsupported => FieldValue::Unsupported,
    }
}

/// Parse the raw createmeta field descriptors into renderable form fields.
/// Required fields come first; `project` and `issuetype` are excluded (handled
/// by the top-level selectors). `subtask` is passed through to
/// [`schema_to_widget`].
pub fn parse_create_fields(values: &[Value], subtask: bool) -> Vec<FormField> {
    let mut fields: Vec<FormField> = Vec::new();
    for v in values {
        let field_id = v
            .get("fieldId")
            .or_else(|| v.get("key"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if field_id.is_empty() || field_id == "project" || field_id == "issuetype" {
            continue;
        }
        let label = v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&field_id)
            .to_string();
        let required = v.get("required").and_then(Value::as_bool).unwrap_or(false);
        let schema = v.get("schema").cloned().unwrap_or(Value::Null);

        let options: Vec<CreateOption> = v
            .get("allowedValues")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(|raw| CreateOption {
                        label: option_label(raw),
                        raw: raw.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let widget = schema_to_widget(&schema, !options.is_empty(), subtask);

        fields.push(FormField {
            field_id,
            label,
            required,
            widget,
            value: initial_value(widget),
            options,
        });
    }
    // Required first, otherwise stable (server order).
    fields.sort_by_key(|f| usize::from(!f.required));
    fields
}

/// Prefill `reporter` with the current user. Runs once per field set (guarded
/// by `form.prefill`) so a reporter the user cleared is not silently
/// restored when `/myself` resolves after the field metadata does. A no-op
/// while either the fields or the current user are still loading, and when the
/// project's create screen has no reporter field — Jira then defaults it to the
/// creator, which is the same person.
pub fn apply_reporter_prefill(form: &mut CreateForm) {
    if form.prefill == Prefill::Done || !matches!(form.fields_state, CacheState::Loaded(())) {
        return;
    }
    let Some(me) = form.current_user.clone() else {
        return;
    };
    form.prefill = Prefill::Done;
    if let Some(field) = form
        .fields
        .iter_mut()
        .find(|f| f.field_id == "reporter" && f.widget == WidgetKind::User)
    {
        field.value = FieldValue::User(Some(me));
    }
}

/// Rows of the user picker for the current search state: the unset row, then
/// the current user while the query is empty (they are who you pick most, and
/// an empty search may not even return them first), then the matches — minus
/// the pinned user, so nobody appears twice.
pub fn user_picker_rows(
    current_user: Option<&UserField>,
    query: &str,
    results: &[UserField],
) -> Vec<UserRow> {
    let mut rows = vec![UserRow::Unset];
    let pinned = query.trim().is_empty().then_some(current_user).flatten();
    if let Some(me) = pinned {
        rows.push(UserRow::Me(me.clone()));
    }
    rows.extend(
        results
            .iter()
            .filter(|u| !pinned.is_some_and(|me| same_user(me, u)))
            .cloned()
            .map(UserRow::Found),
    );
    rows
}

/// Same person? Account ids identify users on Cloud, usernames on Server/DC.
fn same_user(a: &UserField, b: &UserField) -> bool {
    match (&a.account_id, &b.account_id) {
        (Some(x), Some(y)) => x == y,
        _ => a.name.is_some() && a.name == b.name,
    }
}

fn user_row_label(row: &UserRow) -> String {
    match row {
        UserRow::Unset => "—  (leave to Jira)".to_string(),
        UserRow::Me(u) => format!("{}  (me)", u.display()),
        UserRow::Found(u) => u.display().to_string(),
    }
}

const fn user_row_value(row: &UserRow) -> Option<&UserField> {
    match row {
        UserRow::Unset => None,
        UserRow::Me(u) | UserRow::Found(u) => Some(u),
    }
}

/// Rows of the epic picker: the unset row, then whatever the search returned.
/// The server already ordered the matches, so they are passed through as they
/// came.
pub fn epic_picker_rows(results: &[IssueRef]) -> Vec<EpicRow> {
    let mut rows = vec![EpicRow::Unset];
    rows.extend(results.iter().cloned().map(EpicRow::Found));
    rows
}

fn epic_row_label(row: &EpicRow) -> String {
    match row {
        EpicRow::Unset => "—  (no epic)".to_string(),
        EpicRow::Found(e) => e.display(),
    }
}

const fn epic_row_value(row: &EpicRow) -> Option<&IssueRef> {
    match row {
        EpicRow::Unset => None,
        EpicRow::Found(e) => Some(e),
    }
}

/// Both ends of every link type, as the relation chooser lists them: for
/// `Blocks` that is "blocks" (outward) and "is blocked by" (inward). A type
/// whose two halves read the same ("relates to") is listed once — picking
/// either direction would create the same link.
pub fn link_type_choices(types: &[IssueLinkType]) -> Vec<LinkTypeChoice> {
    let mut out = Vec::new();
    for ty in types {
        out.push(LinkTypeChoice {
            name: ty.name.clone(),
            label: ty.outward.clone(),
            direction: LinkDirection::Outward,
        });
        if ty.inward != ty.outward {
            out.push(LinkTypeChoice {
                name: ty.name.clone(),
                label: ty.inward.clone(),
                direction: LinkDirection::Inward,
            });
        }
    }
    out
}

/// Rows of the linked-issues list for `count` links already added.
pub fn link_rows(count: usize) -> Vec<LinkRow> {
    let mut rows: Vec<LinkRow> = (0..count).map(LinkRow::Existing).collect();
    rows.push(LinkRow::Add);
    rows
}

/// The links held by field `field_idx`, or an empty slice if it holds anything
/// else.
fn field_links(form: &CreateForm, field_idx: usize) -> &[IssueLinkDraft] {
    match form.fields.get(field_idx).map(|f| &f.value) {
        Some(FieldValue::IssueLinks(links)) => links.as_slice(),
        _ => &[],
    }
}

/// The `update.issuelinks` operation for one drafted link.
///
/// Jira stores a link as an (inward, outward) pair, and an issue reads its own
/// links from the far end: the side named here is the *other* issue, so the
/// relation the user picked applies from the new issue outwards. Picking
/// "blocks" therefore puts the target on `outwardIssue`, leaving the new issue
/// as the inward one — which is exactly how Jira renders "NEW blocks OPS-42".
fn issue_link_op(link: &IssueLinkDraft) -> Value {
    let side = match link.link_type.direction {
        LinkDirection::Outward => "outwardIssue",
        LinkDirection::Inward => "inwardIssue",
    };
    json!({
        "add": {
            "type": { "name": link.link_type.name },
            side: { "key": link.issue.key },
        }
    })
}

/// The JSON value to emit for a field, or `None` if it should be omitted.
/// `IssueLinks` always falls through to `None` here: links never travel in
/// `fields` — [`build_create_payload`] puts them in `update` instead.
fn field_payload_value(field: &FormField) -> Option<Value> {
    match (&field.widget, &field.value) {
        (WidgetKind::Text, FieldValue::Text { input, .. }) if !input.trim().is_empty() => {
            Some(Value::String(input.clone()))
        }
        (WidgetKind::RichText, FieldValue::Text { input, .. }) if !input.trim().is_empty() => {
            Some(crate::jira::adf::markdown_to_adf(input))
        }
        (WidgetKind::Number, FieldValue::Number { input, .. }) if !input.trim().is_empty() => {
            let t = input.trim();
            t.parse::<i64>()
                .ok()
                .map(|n| json!(n))
                .or_else(|| t.parse::<f64>().ok().map(|n| json!(n)))
        }
        (WidgetKind::Date | WidgetKind::DateTime, FieldValue::Date { value: Some(iso) }) => {
            Some(Value::String(iso.clone()))
        }
        (WidgetKind::Select, FieldValue::SingleOption(Some(i))) => {
            field.options.get(*i).map(|o| option_ref(&o.raw))
        }
        (WidgetKind::User, FieldValue::User(Some(user))) => user_ref(user),
        (WidgetKind::Epic, FieldValue::Epic(Some(epic))) => Some(epic_ref(&field.field_id, epic)),
        (WidgetKind::MultiSelect, FieldValue::MultiOption(set)) if !set.is_empty() => {
            let mut idxs: Vec<usize> = set.iter().copied().collect();
            idxs.sort_unstable();
            Some(Value::Array(
                idxs.iter()
                    .filter_map(|i| field.options.get(*i))
                    .map(|o| option_ref(&o.raw))
                    .collect(),
            ))
        }
        _ => None,
    }
}

/// Assemble the `{ "fields": { … } }` payload, validating required fields.
pub fn build_create_payload(form: &CreateForm) -> Result<Value, String> {
    let it = form
        .issuetype
        .as_ref()
        .ok_or_else(|| "Select an issue type".to_string())?;

    // Without field metadata `form.fields` is empty and the payload would be
    // missing required fields (e.g. summary) — surface that before the server does.
    match &form.fields_state {
        CacheState::Loaded(()) => {}
        CacheState::Failed(e) => {
            return Err(format!(
                "Fields failed to load: {e} — reselect Type to retry"
            ));
        }
        CacheState::Idle | CacheState::Loading => {
            return Err("Fields are still loading…".to_string());
        }
    }

    let mut fields = serde_json::Map::new();
    fields.insert("project".into(), json!({ "key": form.project.key }));
    fields.insert("issuetype".into(), json!({ "id": it.id }));
    // Links are the one field Jira will not take in `fields` on create; they go
    // in a parallel `update` block as add operations.
    let mut update = serde_json::Map::new();

    for field in &form.fields {
        if field.required && field.widget == WidgetKind::Unsupported {
            return Err(format!(
                "Required field “{}” can't be set here — create in browser",
                field.label
            ));
        }
        if field.widget == WidgetKind::IssueLinks {
            let ops: Vec<Value> = match &field.value {
                FieldValue::IssueLinks(links) => links.iter().map(issue_link_op).collect(),
                _ => Vec::new(),
            };
            if ops.is_empty() {
                if field.required {
                    return Err(format!("Required field “{}” is empty", field.label));
                }
            } else {
                update.insert(field.field_id.clone(), Value::Array(ops));
            }
            continue;
        }
        match field_payload_value(field) {
            Some(v) => {
                fields.insert(field.field_id.clone(), v);
            }
            None => {
                if field.required {
                    return Err(format!("Required field “{}” is empty", field.label));
                }
            }
        }
    }

    let mut payload = serde_json::Map::new();
    payload.insert("fields".into(), Value::Object(fields));
    if !update.is_empty() {
        payload.insert("update".into(), Value::Object(update));
    }
    Ok(Value::Object(payload))
}

// ── Input handling ──────────────────────────────────────────────────────────────

#[allow(clippy::needless_pass_by_value)]
pub fn handle_create_input(app: &mut AppState, event: Event) {
    let Event::Key(KeyEvent {
        code, modifiers, ..
    }) = event
    else {
        return;
    };

    // Discard-confirm prompt swallows input first.
    let confirm_open = matches!(
        app.action_state,
        ActionState::CreatingIssue(ref f) if f.discard_prompt == DiscardPrompt::Open
    );
    if confirm_open {
        handle_discard_confirm_input(app, code);
        return;
    }

    // Picker overlay swallows all input while open.
    let picker_open = matches!(
        app.action_state,
        ActionState::CreatingIssue(ref f) if f.picker.is_some()
    );
    if picker_open {
        handle_picker_input(app, code, modifiers);
        return;
    }

    // Alt+Enter submits from anywhere (Ctrl+S would conflict with XOFF flow control).
    if modifiers.contains(KeyModifiers::ALT) && code == KeyCode::Enter {
        submit(app);
        return;
    }

    let mut do_submit = false;
    let mut do_close = false;
    {
        let ActionState::CreatingIssue(ref mut form) = app.action_state else {
            return;
        };

        if form.mode == FormMode::Edit {
            handle_edit_mode_key(form, code, modifiers);
        } else {
            match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    do_close = true;
                }
                KeyCode::Tab | KeyCode::Down | KeyCode::Char('j') => {
                    let n = form.focus_count();
                    form.focus = (form.focus + 1) % n;
                }
                KeyCode::BackTab | KeyCode::Up | KeyCode::Char('k') => {
                    let n = form.focus_count();
                    form.focus = (form.focus + n - 1) % n;
                }
                KeyCode::Char('e') => {
                    request_editor(form);
                }
                KeyCode::Enter => {
                    if form.focus == form.button_idx() {
                        do_submit = true;
                    } else if form.focus == 0 {
                        open_project_picker(form);
                    } else if form.focus == 1 {
                        open_issuetype_picker(form);
                    } else {
                        activate_field(form);
                    }
                }
                _ => {}
            }
        }
    }
    if do_submit {
        submit(app);
    }
    if do_close {
        let ActionState::CreatingIssue(ref mut form) = app.action_state else {
            return;
        };
        if form_is_dirty(form) {
            form.discard_prompt = DiscardPrompt::Open;
        } else {
            app.action_state = ActionState::None;
        }
    }
}

/// Activate the currently focused field (focus index ≥ 2, not the button).
/// Text-like widgets switch the form into `Edit` mode; pickers open directly.
fn activate_field(form: &mut CreateForm) {
    let idx = form.focus - 2;
    let Some(widget) = form.fields.get(idx).map(|f| f.widget) else {
        return;
    };
    match widget {
        WidgetKind::Text | WidgetKind::Number | WidgetKind::RichText => {
            form.mode = FormMode::Edit;
        }
        WidgetKind::Select => open_select_picker(form, idx),
        WidgetKind::MultiSelect => open_multiselect_picker(form, idx),
        WidgetKind::Date | WidgetKind::DateTime => open_date_picker(form, idx),
        WidgetKind::User => open_user_picker(form, idx),
        WidgetKind::Epic => open_epic_picker(form, idx),
        WidgetKind::IssueLinks => open_links_picker(form, idx, 0),
        WidgetKind::Unsupported => {}
    }
}

/// Edit-mode key routing. Only Text/Number/RichText rows should be focused
/// while `mode == Edit`. Esc/Tab return to `Nav`.
fn handle_edit_mode_key(form: &mut CreateForm, code: KeyCode, modifiers: KeyModifiers) {
    // Ctrl+E hands the value over to `$EDITOR` mid-typing. Leaves Edit mode so
    // the form is back in Nav when the editor returns.
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('e') {
        form.mode = FormMode::Nav;
        request_editor(form);
        return;
    }
    match code {
        KeyCode::Esc => {
            form.mode = FormMode::Nav;
            return;
        }
        KeyCode::Tab => {
            form.mode = FormMode::Nav;
            let n = form.focus_count();
            form.focus = (form.focus + 1) % n;
            return;
        }
        KeyCode::BackTab => {
            form.mode = FormMode::Nav;
            let n = form.focus_count();
            form.focus = (form.focus + n - 1) % n;
            return;
        }
        _ => {}
    }

    let idx = form.focus.saturating_sub(2);
    let n = form.focus_count();
    let Some(widget) = form.fields.get(idx).map(|f| f.widget) else {
        form.mode = FormMode::Nav;
        return;
    };
    match widget {
        WidgetKind::Text | WidgetKind::Number => {
            if code == KeyCode::Enter {
                // Single-line: Enter commits & advances, returning to Nav.
                form.mode = FormMode::Nav;
                form.focus = (form.focus + 1) % n;
            } else if let Some(
                FieldValue::Text { input, cursor } | FieldValue::Number { input, cursor },
            ) = form.fields.get_mut(idx).map(|f| &mut f.value)
            {
                edit_text(input, cursor, code);
            }
        }
        WidgetKind::RichText => {
            // Multi-line: Enter inserts a newline; Esc/Tab (handled above) leave Edit.
            if let Some(FieldValue::Text { input, cursor }) =
                form.fields.get_mut(idx).map(|f| &mut f.value)
            {
                let code = if code == KeyCode::Enter {
                    KeyCode::Char('\n')
                } else {
                    code
                };
                edit_text(input, cursor, code);
            }
        }
        _ => {
            // Shouldn't happen for non-text widgets; fall back to Nav.
            form.mode = FormMode::Nav;
        }
    }
}

/// Write text that came back from `$EDITOR` into field `idx`, leaving the
/// cursor at the end so returning to Edit mode continues where the editor left
/// off. Rich text keeps its line breaks; a single-line Jira string would carry
/// them straight into the payload, so there they collapse to spaces.
pub fn apply_editor_text(form: &mut CreateForm, idx: usize, text: &str) {
    let Some(field) = form.fields.get_mut(idx) else {
        return;
    };
    let folded = if field.widget == WidgetKind::RichText {
        text.to_string()
    } else {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    if let FieldValue::Text {
        ref mut input,
        ref mut cursor,
    } = field.value
    {
        *cursor = folded.chars().count();
        *input = folded;
    }
}

/// Ask the main loop to open `$EDITOR` for the focused field. Only the two
/// string widgets qualify: everything else is a picker, and a number is not
/// worth suspending the TUI for.
fn request_editor(form: &mut CreateForm) {
    let Some(idx) = form.focus.checked_sub(2) else {
        return;
    };
    if focused_widget_is_text(form, idx) {
        form.pending_editor = Some(idx);
    }
}

fn focused_widget_is_text(form: &CreateForm, idx: usize) -> bool {
    form.fields
        .get(idx)
        .is_some_and(|f| matches!(f.widget, WidgetKind::Text | WidgetKind::RichText))
}

/// Whether the focused row can be handed to `$EDITOR` (drives the hint bar).
fn editor_available(form: &CreateForm) -> bool {
    form.focus
        .checked_sub(2)
        .is_some_and(|idx| focused_widget_is_text(form, idx))
}

fn form_is_dirty(form: &CreateForm) -> bool {
    form.fields.iter().any(|f| match &f.value {
        FieldValue::Text { input, .. } | FieldValue::Number { input, .. } => !input.is_empty(),
        FieldValue::Date { value } => value.is_some(),
        FieldValue::SingleOption(opt) => opt.is_some(),
        FieldValue::MultiOption(set) => !set.is_empty(),
        // An epic, unlike the prefilled reporter, is always something the user
        // went and picked.
        FieldValue::Epic(epic) => epic.is_some(),
        FieldValue::IssueLinks(links) => !links.is_empty(),
        // The prefilled reporter is not something the user typed, so it must
        // not trigger the discard prompt on an otherwise untouched form.
        FieldValue::User(_) | FieldValue::Unsupported => false,
    })
}

/// Whether the exit-confirmation prompt is showing over the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscardPrompt {
    Closed,
    Open,
}

/// What a keystroke means to the discard prompt; any other key is ignored.
#[derive(Debug, PartialEq, Eq)]
enum DiscardChoice {
    Discard,
    KeepEditing,
}

const fn discard_confirm_choice(code: KeyCode) -> Option<DiscardChoice> {
    match code {
        KeyCode::Esc | KeyCode::Char('q') => Some(DiscardChoice::KeepEditing),
        KeyCode::Enter | KeyCode::Char('y') => Some(DiscardChoice::Discard),
        _ => None,
    }
}

fn handle_discard_confirm_input(app: &mut AppState, code: KeyCode) {
    let ActionState::CreatingIssue(ref mut form) = app.action_state else {
        return;
    };
    if form.discard_prompt == DiscardPrompt::Closed {
        return;
    }
    match discard_confirm_choice(code) {
        Some(DiscardChoice::Discard) => app.action_state = ActionState::None,
        // Dismissing the prompt returns to the form with everything intact.
        Some(DiscardChoice::KeepEditing) => form.discard_prompt = DiscardPrompt::Closed,
        None => {}
    }
}

fn open_project_picker(form: &mut CreateForm) {
    form.picker = Some(CreatePicker::Project {
        query: String::new(),
        query_cursor: 0,
        cursor: 0,
        searching: false,
    });
    form.needs_projects_fetch = true;
}

fn open_issuetype_picker(form: &mut CreateForm) {
    if form.issuetypes.loaded().is_some() {
        form.picker = Some(CreatePicker::IssueType { cursor: 0 });
    }
}

/// Open the assignee/reporter chooser on an empty query — the dispatcher turns
/// that into a request for the project's assignable users.
fn open_user_picker(form: &mut CreateForm, field_idx: usize) {
    let token = form.user_search.as_ref().map_or(1, |s| s.token + 1);
    form.user_search = Some(PickerSearch::new(token));
    form.picker = Some(CreatePicker::User {
        field_idx,
        query: String::new(),
        query_cursor: 0,
        cursor: 0,
        searching: false,
    });
}

/// Open the epic chooser on an empty query — the dispatcher turns that into a
/// request for the project's open epics, newest first.
fn open_epic_picker(form: &mut CreateForm, field_idx: usize) {
    let token = form.epic_search.as_ref().map_or(1, |s| s.token + 1);
    form.epic_search = Some(PickerSearch::new(token));
    form.picker = Some(CreatePicker::Epic {
        field_idx,
        query: String::new(),
        query_cursor: 0,
        cursor: 0,
        searching: false,
    });
}

/// Open the list of links added so far, with `cursor` on `row`. Nothing is
/// fetched yet: the relation chooser is one step further in, and an empty list
/// is a valid thing to see.
fn open_links_picker(form: &mut CreateForm, field_idx: usize, cursor: usize) {
    form.link_search = None;
    form.picker = Some(CreatePicker::IssueLinks { field_idx, cursor });
}

/// Open the relation chooser, asking for the site's link types the first time.
/// A failed fetch is retried on the next open — the cache goes back to `Idle`
/// so the picker is not stuck on an error it cannot clear.
///
/// `editing` is the link whose relation is being changed, or `None` when this is
/// the first step of adding one. Editing starts on the relation the link already
/// has, so a wrong pick is one keystroke from being right.
fn open_link_type_picker(form: &mut CreateForm, field_idx: usize, editing: Option<usize>) {
    if matches!(form.link_types, CacheState::Idle | CacheState::Failed(_)) {
        form.link_types = CacheState::Loading;
        form.needs_link_types_fetch = true;
    }
    let cursor = editing
        .and_then(|i| field_links(form, field_idx).get(i))
        .and_then(|link| {
            picker_link_type_rows(form)
                .iter()
                .position(|c| *c == link.link_type)
        })
        .unwrap_or(0);
    form.picker = Some(CreatePicker::LinkType {
        field_idx,
        editing,
        cursor,
    });
}

/// Open the issue chooser for a relation just picked, on an empty query — the
/// dispatcher turns that into a request for the project's recent issues.
fn open_link_issue_picker(form: &mut CreateForm, field_idx: usize, link_type: LinkTypeChoice) {
    let token = form.link_search.as_ref().map_or(1, |s| s.token + 1);
    form.link_search = Some(PickerSearch::new(token));
    form.picker = Some(CreatePicker::LinkIssue {
        field_idx,
        link_type,
        query: String::new(),
        query_cursor: 0,
        cursor: 0,
        searching: false,
    });
}

fn open_select_picker(form: &mut CreateForm, field_idx: usize) {
    let cursor = match form.fields.get(field_idx).map(|f| &f.value) {
        Some(FieldValue::SingleOption(Some(i))) => *i,
        _ => 0,
    };
    form.picker = Some(CreatePicker::Select { field_idx, cursor });
}

fn open_multiselect_picker(form: &mut CreateForm, field_idx: usize) {
    form.picker = Some(CreatePicker::MultiSelect {
        field_idx,
        cursor: 0,
    });
}

fn open_date_picker(form: &mut CreateForm, field_idx: usize) {
    let tz = crate::tui::views::custom::resolve_tz(None);
    let date_only = form
        .fields
        .get(field_idx)
        .is_some_and(|f| f.widget == WidgetKind::Date);
    let start = match form.fields.get(field_idx).map(|f| &f.value) {
        Some(FieldValue::Date { value: Some(iso) }) => Value::String(iso.clone()),
        _ => Value::Null,
    };
    form.picker = Some(CreatePicker::Date {
        field_idx,
        picker: DatetimePicker::from_value(&start, tz, date_only),
    });
}

/// Validate and either move to the committing state or show an inline error.
/// The form travels with the commit so a server-side rejection can restore it.
fn submit(app: &mut AppState) {
    let state = std::mem::replace(&mut app.action_state, ActionState::None);
    app.action_state = match state {
        ActionState::CreatingIssue(mut form) => match build_create_payload(&form) {
            Ok(payload) => {
                form.error = None;
                ActionState::CommittingCreate { payload, form }
            }
            Err(msg) => {
                form.error = Some(msg);
                ActionState::CreatingIssue(form)
            }
        },
        other => other,
    };
}

fn filtered_project_idxs(form: &CreateForm, query: &str) -> Vec<usize> {
    let q = query.to_lowercase();
    form.available_projects
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            q.is_empty() || p.key.to_lowercase().contains(&q) || p.name.to_lowercase().contains(&q)
        })
        .map(|(i, _)| i)
        .collect()
}

/// Rows currently shown by the open user picker.
fn picker_user_rows(form: &CreateForm, query: &str) -> Vec<UserRow> {
    let results: &[UserField] = form
        .user_search
        .as_ref()
        .and_then(|s| s.results.loaded())
        .map_or(&[], Vec::as_slice);
    user_picker_rows(form.current_user.as_ref(), query, results)
}

/// Rows currently shown by the open epic picker.
fn picker_epic_rows(form: &CreateForm) -> Vec<EpicRow> {
    let results: &[IssueRef] = form
        .epic_search
        .as_ref()
        .and_then(|s| s.results.loaded())
        .map_or(&[], Vec::as_slice);
    epic_picker_rows(results)
}

/// Rows currently shown by the open linked-issue chooser.
fn picker_link_issue_rows(form: &CreateForm) -> &[IssueRef] {
    form.link_search
        .as_ref()
        .and_then(|s| s.results.loaded())
        .map_or(&[], Vec::as_slice)
}

/// Relations currently shown by the open relation chooser.
fn picker_link_type_rows(form: &CreateForm) -> &[LinkTypeChoice] {
    form.link_types.loaded().map_or(&[], Vec::as_slice)
}

/// Key routing for the linked-issues list: edit a link's relation, remove one,
/// start another, or leave. Takes the picker's fields rather than the picker:
/// unlike the searching ones it owns nothing, and leaving without re-setting
/// `form.picker` closes it.
fn handle_links_picker_key(
    form: &mut CreateForm,
    code: KeyCode,
    field_idx: usize,
    mut cursor: usize,
) {
    let rows = link_rows(field_links(form, field_idx).len());

    match code {
        KeyCode::Esc | KeyCode::Char('q') => return, // picker already taken → closed
        KeyCode::Down | KeyCode::Char('j') => {
            cursor = (cursor + 1).min(rows.len().saturating_sub(1));
        }
        KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
        // Enter follows the highlighted row: on a link it reopens the relation
        // chooser for it, on the trailing row it starts a new one. `n` starts a
        // new one from anywhere, so a full list needs no walk to the bottom.
        KeyCode::Enter => {
            let editing = match rows.get(cursor) {
                Some(LinkRow::Existing(i)) => Some(*i),
                _ => None,
            };
            open_link_type_picker(form, field_idx, editing);
            return;
        }
        KeyCode::Char('n') => {
            open_link_type_picker(form, field_idx, None);
            return;
        }
        KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => {
            if let Some(LinkRow::Existing(i)) = rows.get(cursor).cloned()
                && let Some(FieldValue::IssueLinks(links)) =
                    form.fields.get_mut(field_idx).map(|f| &mut f.value)
            {
                links.remove(i);
                // The list just got shorter; keep the cursor on a row.
                cursor = cursor.min(links.len());
            }
        }
        _ => {}
    }

    form.picker = Some(CreatePicker::IssueLinks { field_idx, cursor });
}

/// Key routing for the relation chooser. Its list is site-wide metadata, not a
/// search, so it navigates like the issue-type picker. Leaving goes back to the
/// list of links rather than to the form: this is the middle of editing one.
fn handle_link_type_picker_key(
    form: &mut CreateForm,
    code: KeyCode,
    field_idx: usize,
    editing: Option<usize>,
    mut cursor: usize,
) {
    let count = picker_link_type_rows(form).len();
    // Where the walk started: the link being edited, or the row that adds one.
    let came_from = editing.unwrap_or_else(|| field_links(form, field_idx).len());

    match code {
        KeyCode::Esc | KeyCode::Char('q') => {
            open_links_picker(form, field_idx, came_from);
            return;
        }
        KeyCode::Down | KeyCode::Char('j') => cursor = (cursor + 1).min(count.saturating_sub(1)),
        KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
        KeyCode::Enter => {
            if let Some(choice) = picker_link_type_rows(form).get(cursor).cloned() {
                match editing {
                    // Editing changes the relation in place; the issue stays put.
                    Some(i) => {
                        if let Some(FieldValue::IssueLinks(links)) =
                            form.fields.get_mut(field_idx).map(|f| &mut f.value)
                            && let Some(link) = links.get_mut(i)
                        {
                            link.link_type = choice;
                        }
                        open_links_picker(form, field_idx, i);
                    }
                    None => open_link_issue_picker(form, field_idx, choice),
                }
                return;
            }
        }
        _ => {}
    }

    form.picker = Some(CreatePicker::LinkType {
        field_idx,
        editing,
        cursor,
    });
}

/// Key routing for the linked-issue chooser. Searches like the epic chooser;
/// picking appends the link and returns to the list, where another can be added
/// straight away. There is no unset row — a link with no issue is not a link,
/// and `q` backs out to the relation chooser instead.
fn handle_link_issue_picker_key(form: &mut CreateForm, code: KeyCode, picker: CreatePicker) {
    let CreatePicker::LinkIssue {
        field_idx,
        link_type,
        mut query,
        mut query_cursor,
        mut cursor,
        mut searching,
    } = picker
    else {
        return;
    };

    if searching {
        match code {
            // Leave search mode but keep the query (and its matches) applied.
            KeyCode::Esc | KeyCode::Enter => searching = false,
            _ => {
                let before = query.clone();
                edit_text(&mut query, &mut query_cursor, code);
                if query != before {
                    cursor = 0;
                    if let Some(search) = form.link_search.as_mut() {
                        search.requery(query.clone());
                    }
                }
            }
        }
    } else {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                form.link_search = None;
                // Back to the relation that was picked, not to the top of the
                // list: backing out is usually "wrong issue", not "wrong verb".
                let cursor = picker_link_type_rows(form)
                    .iter()
                    .position(|c| *c == link_type)
                    .unwrap_or(0);
                form.picker = Some(CreatePicker::LinkType {
                    field_idx,
                    editing: None,
                    cursor,
                });
                return;
            }
            KeyCode::Char('/') => searching = true,
            KeyCode::Enter => {
                let chosen = picker_link_issue_rows(form).get(cursor).cloned();
                if let Some(issue) = chosen {
                    add_link(form, field_idx, link_type, issue);
                    return;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let count = picker_link_issue_rows(form).len();
                cursor = (cursor + 1).min(count.saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
            _ => {}
        }
    }

    form.picker = Some(CreatePicker::LinkIssue {
        field_idx,
        link_type,
        query,
        query_cursor,
        cursor,
        searching,
    });
}

/// Append a link and return to the list with the cursor on it. A repeat of one
/// already added is dropped: Jira would reject the duplicate operation, and the
/// second row would read identically to the first.
fn add_link(form: &mut CreateForm, field_idx: usize, link_type: LinkTypeChoice, issue: IssueRef) {
    let draft = IssueLinkDraft { link_type, issue };
    let at = if let Some(FieldValue::IssueLinks(links)) =
        form.fields.get_mut(field_idx).map(|f| &mut f.value)
    {
        links.iter().position(|l| *l == draft).unwrap_or_else(|| {
            links.push(draft);
            links.len() - 1
        })
    } else {
        0
    };
    open_links_picker(form, field_idx, at);
}

/// Key routing for the epic chooser. Mirrors the user chooser: `/` types a
/// query that goes to the server, Enter picks the highlighted row.
fn handle_epic_picker_key(form: &mut CreateForm, code: KeyCode, picker: CreatePicker) {
    let CreatePicker::Epic {
        field_idx,
        mut query,
        mut query_cursor,
        mut cursor,
        mut searching,
    } = picker
    else {
        return;
    };

    if searching {
        match code {
            // Leave search mode but keep the query (and its matches) applied.
            KeyCode::Esc | KeyCode::Enter => searching = false,
            _ => {
                let before = query.clone();
                edit_text(&mut query, &mut query_cursor, code);
                if query != before {
                    cursor = 0;
                    if let Some(search) = form.epic_search.as_mut() {
                        search.requery(query.clone());
                    }
                }
            }
        }
    } else {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                form.epic_search = None;
                return; // picker already taken → closed
            }
            KeyCode::Char('/') => searching = true,
            KeyCode::Enter => {
                let chosen = picker_epic_rows(form)
                    .get(cursor)
                    .map(|row| epic_row_value(row).cloned());
                if let Some(epic) = chosen {
                    if let Some(field) = form.fields.get_mut(field_idx) {
                        field.value = FieldValue::Epic(epic);
                    }
                    form.epic_search = None;
                    return;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let count = picker_epic_rows(form).len();
                cursor = (cursor + 1).min(count.saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
            _ => {}
        }
    }

    form.picker = Some(CreatePicker::Epic {
        field_idx,
        query,
        query_cursor,
        cursor,
        searching,
    });
}

/// Key routing for the assignee/reporter chooser. `picker` is the taken
/// `CreatePicker::User`; leaving it dropped closes the chooser.
fn handle_user_picker_key(form: &mut CreateForm, code: KeyCode, picker: CreatePicker) {
    let CreatePicker::User {
        field_idx,
        mut query,
        mut query_cursor,
        mut cursor,
        mut searching,
    } = picker
    else {
        return;
    };

    if searching {
        match code {
            // Leave search mode but keep the query (and its matches) applied.
            KeyCode::Esc | KeyCode::Enter => searching = false,
            _ => {
                let before = query.clone();
                edit_text(&mut query, &mut query_cursor, code);
                if query != before {
                    cursor = 0;
                    if let Some(search) = form.user_search.as_mut() {
                        search.requery(query.clone());
                    }
                }
            }
        }
    } else {
        match code {
            KeyCode::Esc | KeyCode::Char('q') => {
                form.user_search = None;
                return; // picker already taken → closed
            }
            KeyCode::Char('/') => searching = true,
            KeyCode::Enter => {
                let chosen = picker_user_rows(form, &query)
                    .get(cursor)
                    .map(|row| user_row_value(row).cloned());
                if let Some(user) = chosen {
                    if let Some(field) = form.fields.get_mut(field_idx) {
                        field.value = FieldValue::User(user);
                    }
                    form.user_search = None;
                    return;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let count = picker_user_rows(form, &query).len();
                cursor = (cursor + 1).min(count.saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => cursor = cursor.saturating_sub(1),
            _ => {}
        }
    }

    form.picker = Some(CreatePicker::User {
        field_idx,
        query,
        query_cursor,
        cursor,
        searching,
    });
}

#[allow(clippy::too_many_lines)]
fn handle_picker_input(app: &mut AppState, code: KeyCode, _modifiers: KeyModifiers) {
    let ActionState::CreatingIssue(ref mut form) = app.action_state else {
        return;
    };
    let Some(picker) = form.picker.take() else {
        return;
    };

    match picker {
        // Split out: these two are the pickers whose lists come from the
        // server, so their keys also drive a search.
        p @ CreatePicker::User { .. } => handle_user_picker_key(form, code, p),
        p @ CreatePicker::Epic { .. } => handle_epic_picker_key(form, code, p),
        // The three steps of adding a link; each hands back to the previous.
        CreatePicker::IssueLinks { field_idx, cursor } => {
            handle_links_picker_key(form, code, field_idx, cursor);
        }
        CreatePicker::LinkType {
            field_idx,
            editing,
            cursor,
        } => handle_link_type_picker_key(form, code, field_idx, editing, cursor),
        p @ CreatePicker::LinkIssue { .. } => handle_link_issue_picker_key(form, code, p),
        CreatePicker::Project {
            mut query,
            mut query_cursor,
            mut cursor,
            mut searching,
        } => {
            let visible = filtered_project_idxs(form, &query);
            if searching {
                match code {
                    KeyCode::Esc | KeyCode::Enter => {
                        // Leave search mode but keep the query (and filter) applied.
                        searching = false;
                        form.picker = Some(CreatePicker::Project {
                            query,
                            query_cursor,
                            cursor,
                            searching,
                        });
                    }
                    _ => {
                        edit_text(&mut query, &mut query_cursor, code);
                        cursor = 0;
                        form.picker = Some(CreatePicker::Project {
                            query,
                            query_cursor,
                            cursor,
                            searching,
                        });
                    }
                }
            } else {
                match code {
                    KeyCode::Esc | KeyCode::Char('q') => {} // picker taken → closed
                    KeyCode::Char('/') => {
                        searching = true;
                        form.picker = Some(CreatePicker::Project {
                            query,
                            query_cursor,
                            cursor,
                            searching,
                        });
                    }
                    KeyCode::Enter => {
                        if let Some(&pi) = visible.get(cursor) {
                            let project = form.available_projects[pi].clone();
                            form.set_project(project);
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        cursor = (cursor + 1).min(visible.len().saturating_sub(1));
                        form.picker = Some(CreatePicker::Project {
                            query,
                            query_cursor,
                            cursor,
                            searching,
                        });
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        cursor = cursor.saturating_sub(1);
                        form.picker = Some(CreatePicker::Project {
                            query,
                            query_cursor,
                            cursor,
                            searching,
                        });
                    }
                    _ => {
                        form.picker = Some(CreatePicker::Project {
                            query,
                            query_cursor,
                            cursor,
                            searching,
                        });
                    }
                }
            }
        }
        CreatePicker::IssueType { mut cursor } => {
            let count = form.issuetypes.loaded().map_or(0, Vec::len);
            match code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    cursor = (cursor + 1).min(count.saturating_sub(1));
                    form.picker = Some(CreatePicker::IssueType { cursor });
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    cursor = cursor.saturating_sub(1);
                    form.picker = Some(CreatePicker::IssueType { cursor });
                }
                KeyCode::Enter => {
                    if let Some(it) = form
                        .issuetypes
                        .loaded()
                        .and_then(|v| v.get(cursor))
                        .cloned()
                    {
                        form.set_issuetype(it);
                    }
                }
                _ => form.picker = Some(CreatePicker::IssueType { cursor }),
            }
        }
        CreatePicker::Select {
            field_idx,
            mut cursor,
        } => {
            let count = form.fields.get(field_idx).map_or(0, |f| f.options.len());
            match code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    cursor = (cursor + 1).min(count.saturating_sub(1));
                    form.picker = Some(CreatePicker::Select { field_idx, cursor });
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    cursor = cursor.saturating_sub(1);
                    form.picker = Some(CreatePicker::Select { field_idx, cursor });
                }
                KeyCode::Enter => {
                    if let Some(field) = form.fields.get_mut(field_idx) {
                        field.value = FieldValue::SingleOption(Some(cursor));
                    }
                }
                _ => form.picker = Some(CreatePicker::Select { field_idx, cursor }),
            }
        }
        CreatePicker::MultiSelect {
            field_idx,
            mut cursor,
        } => {
            let count = form.fields.get(field_idx).map_or(0, |f| f.options.len());
            match code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    cursor = (cursor + 1).min(count.saturating_sub(1));
                    form.picker = Some(CreatePicker::MultiSelect { field_idx, cursor });
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    cursor = cursor.saturating_sub(1);
                    form.picker = Some(CreatePicker::MultiSelect { field_idx, cursor });
                }
                KeyCode::Char(' ') => {
                    if let Some(FieldValue::MultiOption(set)) =
                        form.fields.get_mut(field_idx).map(|f| &mut f.value)
                    {
                        if set.contains(&cursor) {
                            set.remove(&cursor);
                        } else {
                            set.insert(cursor);
                        }
                    }
                    form.picker = Some(CreatePicker::MultiSelect { field_idx, cursor });
                }
                _ => form.picker = Some(CreatePicker::MultiSelect { field_idx, cursor }),
            }
        }
        CreatePicker::Date {
            field_idx,
            mut picker,
        } => match code {
            KeyCode::Esc | KeyCode::Char('q') => {}
            KeyCode::Enter => {
                // Date-only pickers commit straight from Date mode; otherwise
                // Enter walks Date → Time/Hour → Minute → commit.
                if !picker.date_only && picker.mode == DatetimePickerMode::Date {
                    picker.mode = DatetimePickerMode::Time;
                    form.picker = Some(CreatePicker::Date { field_idx, picker });
                } else if !picker.date_only && picker.time_focus == TimeFocus::Hour {
                    picker.time_focus = TimeFocus::Minute;
                    form.picker = Some(CreatePicker::Date { field_idx, picker });
                } else if let Some(field) = form.fields.get_mut(field_idx) {
                    field.value = FieldValue::Date {
                        value: Some(picker.to_iso_string()),
                    };
                }
            }
            _ => {
                match picker.mode {
                    DatetimePickerMode::Date => handle_date_key(&mut picker, code),
                    DatetimePickerMode::Time => handle_time_key(&mut picker, code),
                }
                form.picker = Some(CreatePicker::Date { field_idx, picker });
            }
        },
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────────

/// Status row shown under the Type field while its field metadata isn't loaded.
/// Text and style of the line standing in for the field list while it loads or
/// after it failed, if either applies.
fn fields_status_line(form: &CreateForm) -> Option<(String, Style)> {
    form.issuetype.as_ref()?;
    match &form.fields_state {
        CacheState::Loading | CacheState::Idle => Some((
            "(loading fields…)".to_string(),
            Style::default()
                .fg(theme::MUTED)
                .add_modifier(Modifier::ITALIC),
        )),
        CacheState::Failed(e) => Some((
            format!("(fields failed: {e} — reselect Type to retry)"),
            Style::default().fg(Color::Red),
        )),
        CacheState::Loaded(()) => None,
    }
}

/// Word-wrap `text` to `width`, hard-breaking any single word too long to fit.
/// The form draws unwrapped so that rows and screen lines stay one to one;
/// prose that has to be read in full is wrapped here instead.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    for word in text.split_whitespace() {
        let len = word.chars().count();
        let room = width - row.chars().count().min(width);
        if !row.is_empty() && len + 1 > room {
            rows.push(std::mem::take(&mut row));
        }
        if len > width {
            // Nothing to break on: fill the row and carry the rest over.
            for ch in word.chars() {
                if row.chars().count() == width {
                    rows.push(std::mem::take(&mut row));
                }
                row.push(ch);
            }
            continue;
        }
        if !row.is_empty() {
            row.push(' ');
        }
        row.push_str(word);
    }
    if !row.is_empty() || rows.is_empty() {
        rows.push(row);
    }
    rows
}

/// Push `text` as however many indented rows it takes to read it in full.
fn push_wrapped(lines: &mut Vec<Line<'static>>, text: &str, style: Style, inner_width: u16) {
    let indent = MARKER_COLS;
    let width = (inner_width as usize).saturating_sub(indent);
    for row in wrap_words(text, width) {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(indent)),
            Span::styled(row, style),
        ]));
    }
}

/// Columns the label column occupies. Values line up at `MARKER_COLS + LABEL_COLS`
/// unless a label is longer than that, in which case its row shifts right.
const LABEL_COLS: usize = 16;
/// Width of the `▶ ` focus marker every row is prefixed with.
const MARKER_COLS: usize = 2;
/// Ceiling on the rows a rich-text field expands to while being typed into, so a
/// long description cannot push the Create button off the overlay.
const MAX_EDIT_ROWS: usize = 8;

fn label_cols(label: &str, required: bool) -> usize {
    (label.chars().count() + usize::from(required)).max(LABEL_COLS)
}

/// Screen column a field's value starts at.
fn value_col(label: &str, required: bool) -> usize {
    MARKER_COLS + label_cols(label, required)
}

/// Clip `value` to `cols`, marking the cut with an ellipsis. The form draws
/// without wrapping — one field, one row — so anything too long is elided here
/// rather than silently sheared off at the border.
fn elide(value: &str, cols: usize) -> String {
    if value.chars().count() <= cols {
        return value.to_string();
    }
    if cols == 0 {
        return String::new();
    }
    let mut out: String = value.chars().take(cols - 1).collect();
    out.push('…');
    out
}

/// Break `input` into the rows the inline editor draws, and locate the caret
/// among them as `(row, column)`. Wrapping is by cell, not by word: the caret
/// has to map to an exact position, and word breaks in a column this narrow
/// would cost more than they buy.
fn wrap_edit_rows(input: &str, cursor: usize, width: usize) -> (Vec<String>, usize, usize) {
    let width = width.max(1);
    let mut rows: Vec<String> = Vec::new();
    let mut row = String::new();
    let mut col = 0usize;
    let mut caret = (0usize, 0usize);
    // The row the next character would land on is always drawn, so a value that
    // ends exactly at the edge — or on a newline — still shows where typing
    // continues.
    for (idx, ch) in input.chars().enumerate() {
        if idx == cursor {
            caret = (rows.len(), col);
        }
        if ch == '\n' {
            rows.push(std::mem::take(&mut row));
            col = 0;
        } else {
            row.push(ch);
            col += 1;
            // A row filled to the edge closes here, so the next char — and a
            // caret sitting on it — lands at the start of the next row.
            if col == width {
                rows.push(std::mem::take(&mut row));
                col = 0;
            }
        }
    }
    if cursor >= input.chars().count() {
        caret = (rows.len(), col);
    }
    rows.push(row);
    (rows, caret.0, caret.1)
}

/// First of `total` rows to draw so that `caret_row` stays visible in a `max`-row
/// window. Only scrolls once the caret would fall out the bottom.
fn row_window_offset(total: usize, caret_row: usize, max: usize) -> usize {
    if total <= max {
        return 0;
    }
    caret_row
        .saturating_sub(max - 1)
        .min(total.saturating_sub(max))
}

/// First char of a single-line value to draw so the caret stays inside `width`.
/// `len + 1` positions are reachable: the caret also sits one past the last char.
fn hscroll_offset(len: usize, cursor: usize, width: usize) -> usize {
    let width = width.max(1);
    if len < width {
        return 0;
    }
    cursor.saturating_sub(width - 1).min(len + 1 - width)
}

/// The row(s) of the field currently being typed into, plus the caret's cell
/// relative to the overlay's inner area. `first_row` is where these rows land.
///
/// The value is drawn as an underlined well — an input field you can see the
/// extent of — with the real terminal cursor inside it, which is what actually
/// blinks. Rich text expands to as many rows as it needs (capped at
/// `MAX_EDIT_ROWS`); everything else scrolls horizontally within its one row.
fn edit_rows(
    field: &FormField,
    input: &str,
    cursor: usize,
    inner_width: u16,
    first_row: u16,
) -> (Vec<Line<'static>>, (u16, u16)) {
    let col = value_col(&field.label, field.required);
    let width = (inner_width as usize).saturating_sub(col).max(1);
    let well = Style::default().add_modifier(Modifier::UNDERLINED);
    let marker = Span::styled("▶ ", Style::default().fg(Color::Blue));
    let label = Span::styled(
        format!(
            "{:<w$}",
            format!("{}{}", field.label, if field.required { "*" } else { "" }),
            w = label_cols(&field.label, field.required)
        ),
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
    );

    if field.widget == WidgetKind::RichText {
        let (rows, caret_row, caret_col) = wrap_edit_rows(input, cursor, width);
        let offset = row_window_offset(rows.len(), caret_row, MAX_EDIT_ROWS);
        let mut lines = Vec::new();
        for (i, row) in rows.iter().skip(offset).take(MAX_EDIT_ROWS).enumerate() {
            let text = Span::styled(format!("{row:<width$}"), well);
            lines.push(if i == 0 {
                Line::from(vec![marker.clone(), label.clone(), text])
            } else {
                Line::from(vec![Span::raw(" ".repeat(col)), text])
            });
        }
        let caret = (
            u16::try_from(col + caret_col).unwrap_or(u16::MAX),
            first_row + u16::try_from(caret_row - offset).unwrap_or(0),
        );
        return (lines, caret);
    }

    let len = input.chars().count();
    let offset = hscroll_offset(len, cursor, width);
    let shown: String = input.chars().skip(offset).take(width).collect();
    let line = Line::from(vec![
        marker,
        label,
        Span::styled(format!("{shown:<width$}"), well),
    ]);
    let caret = (
        u16::try_from(col + cursor.saturating_sub(offset)).unwrap_or(u16::MAX),
        first_row,
    );
    (vec![line], caret)
}

pub fn render_create_issue_overlay(f: &mut Frame, app: &AppState) {
    let ActionState::CreatingIssue(ref form) = app.action_state else {
        return;
    };

    let area = crate::tui::render::centered_rect(70, 80, f.area());
    f.render_widget(Clear, area);

    let dim = form.picker.is_some();
    // Typing into a field is a distinct mode, so it gets a distinct frame:
    // blue border, and the title says which field has the keystrokes.
    let editing_label = (!dim && form.mode == FormMode::Edit)
        .then(|| form.focus.checked_sub(2).and_then(|i| form.fields.get(i)))
        .flatten()
        .map(|f| f.label.clone());
    let title_style = if dim {
        Style::default()
            .fg(theme::MUTED)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let border_color = if dim {
        theme::MUTED
    } else if editing_label.is_some() {
        Color::Blue
    } else {
        Color::Reset
    };
    let title = editing_label.as_ref().map_or_else(
        || " New Issue ".to_string(),
        |label| format!(" New Issue · editing {label} "),
    );
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(title, title_style));
    if !dim {
        block = block.title_bottom(hints_line(form.mode, editor_available(form)));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    let (lines, caret) = form_lines(form, inner.width, dim);

    // No wrapping: every line above is exactly one screen row, which is what
    // lets the caret's row be counted off the `lines` vector.
    f.render_widget(Paragraph::new(lines), inner);

    // The terminal's own cursor is what blinks, so put it where the next
    // keystroke will land. Suppressed while a picker covers the form.
    if let Some((col, row)) = caret
        && !dim
        && row < inner.height
    {
        f.set_cursor_position((
            inner.x + col.min(inner.width.saturating_sub(1)),
            inner.y + row,
        ));
    }

    if let Some(picker) = &form.picker {
        render_picker(f, form, picker, app.project_cache.is_pending());
    }

    if form.discard_prompt == DiscardPrompt::Open {
        render_discard_confirm_overlay(f);
    }
}

/// Every row of the form, top to bottom, plus the caret's cell relative to the
/// overlay's inner area when a field is being typed into. One field is one row
/// (the form draws unwrapped) except the rich-text field being edited, which
/// expands — so the caret's row is simply its index in the returned vector.
fn form_lines(
    form: &CreateForm,
    inner_width: u16,
    dim: bool,
) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut caret: Option<(u16, u16)> = None;
    let cols = |label: &str, required: bool| {
        (inner_width as usize).saturating_sub(value_col(label, required))
    };
    lines.push(field_row(
        "Project",
        false,
        &format!("{}  {}  ▾", form.project.key, form.project.name),
        form.focus == 0 && !dim,
        false,
        cols("Project", false),
    ));

    let type_text = match &form.issuetypes {
        CacheState::Loading | CacheState::Idle => "(loading types…)".to_string(),
        CacheState::Failed(e) => format!("(failed: {e})"),
        CacheState::Loaded(_) => form
            .issuetype
            .as_ref()
            .map_or_else(|| "(select)  ▾".to_string(), |it| format!("{}  ▾", it.name)),
    };
    lines.push(field_row(
        "Type",
        true,
        &type_text,
        form.focus == 1 && !dim,
        false,
        cols("Type", true),
    ));

    if let Some((text, style)) = fields_status_line(form) {
        push_wrapped(&mut lines, &text, style, inner_width);
    }

    for (i, field) in form.fields.iter().enumerate() {
        let focused = form.focus == 2 + i && !dim;
        let editing = focused && form.mode == FormMode::Edit;
        let typed = match &field.value {
            FieldValue::Text { input, cursor } | FieldValue::Number { input, cursor } => {
                Some((input, *cursor))
            }
            _ => None,
        };
        // The row being typed into is drawn as a live input well; every other
        // row is a one-line summary, multi-line bodies collapsed.
        if let (true, Some((input, cursor))) = (editing, typed) {
            let first_row = u16::try_from(lines.len()).unwrap_or(u16::MAX);
            let (rows, at) = edit_rows(field, input, cursor, inner_width, first_row);
            lines.extend(rows);
            caret = Some(at);
        } else {
            lines.push(field_row(
                &field.label,
                field.required,
                &field_value_display(field),
                focused,
                field.widget == WidgetKind::Unsupported,
                cols(&field.label, field.required),
            ));
        }
    }

    // Spacer + error + Create button.
    lines.push(Line::from(""));
    if let Some(err) = &form.error {
        push_wrapped(
            &mut lines,
            err,
            Style::default().fg(Color::Red),
            inner_width,
        );
    }
    let btn_focused = form.focus == form.button_idx() && !dim;
    let btn_style = if btn_focused {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().fg(Color::Green)
    };
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("  Create  ", btn_style),
    ]));

    (lines, caret)
}

fn field_row(
    label: &str,
    required: bool,
    value: &str,
    focused: bool,
    unsupported: bool,
    value_cols: usize,
) -> Line<'static> {
    let marker = if focused { "▶ " } else { "  " };
    let label_owned = format!("{label}{}", if required { "*" } else { "" });
    let label_style = if required {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::MUTED)
    };
    let value_style = if unsupported {
        Style::default()
            .fg(theme::MUTED)
            .add_modifier(Modifier::DIM)
    } else if focused {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::raw(marker.to_string()),
        Span::styled(
            format!("{label_owned:<w$}", w = label_cols(label, required)),
            label_style,
        ),
        Span::styled(elide(value, value_cols), value_style),
    ])
}

/// One-line summary of a field's value for a row that is not being typed into —
/// the row being edited is drawn by `edit_rows` instead.
fn field_value_display(field: &FormField) -> String {
    match (&field.widget, &field.value) {
        (WidgetKind::Text | WidgetKind::RichText, FieldValue::Text { input, .. })
        | (WidgetKind::Number, FieldValue::Number { input, .. }) => {
            if input.is_empty() {
                "—".to_string()
            } else {
                collapse_lines(input)
            }
        }
        (WidgetKind::Date | WidgetKind::DateTime, FieldValue::Date { value }) => {
            value.clone().unwrap_or_else(|| "— ▾".to_string())
        }
        (WidgetKind::Select, FieldValue::SingleOption(sel)) => sel
            .and_then(|i| field.options.get(i))
            .map_or_else(|| "(select)  ▾".to_string(), |o| format!("{}  ▾", o.label)),
        (WidgetKind::MultiSelect, FieldValue::MultiOption(set)) => {
            if set.is_empty() {
                "(none)  ▾".to_string()
            } else {
                format!("{} selected  ▾", set.len())
            }
        }
        (WidgetKind::User, FieldValue::User(user)) => user
            .as_ref()
            .map_or_else(|| "—".to_string(), |u| u.display().to_string()),
        (WidgetKind::Epic, FieldValue::Epic(epic)) => epic.as_ref().map_or_else(
            || "(none)  ▾".to_string(),
            |e| format!("{}  ▾", e.display()),
        ),
        (WidgetKind::IssueLinks, FieldValue::IssueLinks(links)) => match links.len() {
            0 => "(none)  ▾".to_string(),
            // One link fits, and reading "blocks OPS-42" beats reading "1 link".
            1 => format!("{}  ▾", links[0].display()),
            n => format!("{n} links  ▾"),
        },
        (WidgetKind::Unsupported, _) => "(set in browser)".to_string(),
        _ => String::new(),
    }
}

/// One-line stand-in for a multi-line value: its first line plus a count of
/// what is hidden, so a description written in `$EDITOR` does not take over the
/// form.
fn collapse_lines(input: &str) -> String {
    let mut lines = input.lines();
    let first = lines.next().unwrap_or_default();
    let rest = input.lines().count().saturating_sub(1);
    if rest == 0 {
        return first.to_string();
    }
    let plural = if rest == 1 { "line" } else { "lines" };
    format!("{first} … (+{rest} {plural})")
}

fn render_picker(f: &mut Frame, form: &CreateForm, picker: &CreatePicker, projects_loading: bool) {
    match picker {
        CreatePicker::Project {
            query,
            cursor,
            searching,
            ..
        } => {
            let visible = filtered_project_idxs(form, query);
            let items: Vec<String> = visible
                .iter()
                .filter_map(|i| form.available_projects.get(*i))
                .map(|p| format!("{}  {}", p.key, p.name))
                .collect();
            let title = if *searching {
                format!(" Project — /{query}▏ ")
            } else if query.is_empty() {
                " Project ".to_string()
            } else {
                format!(" Project — /{query} ")
            };
            render_list(
                f,
                &title,
                &items,
                *cursor,
                None,
                projects_loading.then_some("(loading more…)"),
                Some(project_picker_hints_line(*searching)),
            );
        }
        CreatePicker::IssueType { cursor } => {
            let items: Vec<String> = form
                .issuetypes
                .loaded()
                .map(|v| v.iter().map(|it| it.name.clone()).collect())
                .unwrap_or_default();
            render_list(f, " Issue Type ", &items, *cursor, None, None, None);
        }
        CreatePicker::Select { field_idx, cursor } => {
            let (title, items) = field_option_items(form, *field_idx);
            render_list(f, &title, &items, *cursor, None, None, None);
        }
        CreatePicker::User {
            field_idx,
            query,
            cursor,
            searching,
            ..
        } => render_user_picker(f, form, *field_idx, query, *cursor, *searching),
        CreatePicker::Epic {
            field_idx,
            query,
            cursor,
            searching,
            ..
        } => render_epic_picker(f, form, *field_idx, query, *cursor, *searching),
        CreatePicker::IssueLinks { field_idx, cursor } => {
            render_links_picker(f, form, *field_idx, *cursor);
        }
        CreatePicker::LinkType {
            field_idx,
            editing,
            cursor,
        } => render_link_type_picker(f, form, *field_idx, *editing, *cursor),
        CreatePicker::LinkIssue {
            link_type,
            query,
            cursor,
            searching,
            ..
        } => render_link_issue_picker(f, form, link_type, query, *cursor, *searching),
        CreatePicker::MultiSelect { field_idx, cursor } => {
            let (title, items) = field_option_items(form, *field_idx);
            let marks = match form.fields.get(*field_idx).map(|f| &f.value) {
                Some(FieldValue::MultiOption(set)) => Some(set.clone()),
                _ => None,
            };
            render_list(f, &title, &items, *cursor, marks.as_ref(), None, None);
        }
        CreatePicker::Date { field_idx, picker } => {
            let area = crate::tui::render::centered_rect(40, 50, f.area());
            let label = form
                .fields
                .get(*field_idx)
                .map_or("Date", |fld| fld.label.as_str());
            datetime_picker::render_datetime_picker_in(f, area, picker, label, None);
        }
    }
}

/// Discard prompt for a dirty form. No buttons: like the rest of the app, the
/// footer hints are the menu. One key per action is hinted; `y` and `Esc` work
/// too, but spelling out every alias is what made the old footer noisy.
fn render_discard_confirm_overlay(f: &mut Frame) {
    let area = crate::tui::render::centered_rect(40, 20, f.area());
    f.render_widget(Clear, area);
    let hint = Line::from(vec![
        Span::raw("┤ "),
        Span::styled("↵", Style::default().fg(Color::Red)),
        Span::raw(" discard | "),
        Span::styled("q", Style::default().fg(Color::Magenta)),
        Span::raw(" keep editing ├──"),
    ])
    .alignment(Alignment::Right);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(" Discard new issue? ")
        .title_bottom(hint);
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new("You have unsaved fields. Discard them?"),
        inner,
    );
}

/// Small confirmation popup shown after a successful create.
pub fn render_created_confirm_overlay(f: &mut Frame, key: &str) {
    let area = crate::tui::render::centered_rect(40, 20, f.area());
    f.render_widget(Clear, area);
    let hint = Line::from(vec![
        Span::raw("┤ "),
        Span::styled("Enter", Style::default().fg(Color::Magenta)),
        Span::raw(" dismiss ├──"),
    ])
    .alignment(Alignment::Right);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .title(" Issue created ")
        .title_bottom(hint);
    let inner = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("✓ Created ", Style::default().fg(Color::Green)),
            Span::styled(
                key.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]))
        .alignment(Alignment::Center),
        inner,
    );
}

/// The assignee/reporter chooser. Its list is the search results plus the
/// unset/me rows, and it reports search progress in the footer.
fn render_user_picker(
    f: &mut Frame,
    form: &CreateForm,
    field_idx: usize,
    query: &str,
    cursor: usize,
    searching: bool,
) {
    let label = form
        .fields
        .get(field_idx)
        .map_or("User", |f| f.label.as_str());
    let title = if searching {
        format!(" {label} — /{query}\u{258f} ")
    } else if query.is_empty() {
        format!(" {label} ")
    } else {
        format!(" {label} — /{query} ")
    };
    let hints = Some(project_picker_hints_line(searching));
    let search = form.user_search.as_ref();

    // A failed search would otherwise leave an unexplained one-row list.
    if let Some(CacheState::Failed(e)) = search.map(|s| &s.results) {
        let items = [format!("(user search failed: {e})")];
        render_list(f, &title, &items, 0, None, None, hints);
        return;
    }

    let items: Vec<String> = picker_user_rows(form, query)
        .iter()
        .map(user_row_label)
        .collect();
    let note = search
        .is_some_and(|s| s.pending)
        .then_some("(searching\u{2026})");
    render_list(f, &title, &items, cursor, None, note, hints);
}

/// The epic chooser. Like the user chooser its list is server-side, so it
/// reports search progress and failure in place of the rows.
fn render_epic_picker(
    f: &mut Frame,
    form: &CreateForm,
    field_idx: usize,
    query: &str,
    cursor: usize,
    searching: bool,
) {
    let label = form
        .fields
        .get(field_idx)
        .map_or("Epic", |f| f.label.as_str());
    let title = if searching {
        format!(" {label} — /{query}\u{258f} ")
    } else if query.is_empty() {
        format!(" {label} ")
    } else {
        format!(" {label} — /{query} ")
    };
    let hints = Some(project_picker_hints_line(searching));
    let search = form.epic_search.as_ref();

    // A failed search would otherwise leave an unexplained one-row list.
    if let Some(CacheState::Failed(e)) = search.map(|s| &s.results) {
        let items = [format!("(epic search failed: {e})")];
        render_list(f, &title, &items, 0, None, None, hints);
        return;
    }

    let items: Vec<String> = picker_epic_rows(form).iter().map(epic_row_label).collect();
    let note = search
        .is_some_and(|s| s.pending)
        .then_some("(searching\u{2026})");
    render_list(f, &title, &items, cursor, None, note, hints);
}

/// The links added so far, with the row that starts another one on top.
fn render_links_picker(f: &mut Frame, form: &CreateForm, field_idx: usize, cursor: usize) {
    let label = form
        .fields
        .get(field_idx)
        .map_or("Linked Issues", |f| f.label.as_str());
    let links = field_links(form, field_idx);
    let items: Vec<String> = link_rows(links.len())
        .iter()
        .map(|row| match row {
            LinkRow::Existing(i) => links[*i].display(),
            LinkRow::Add => "+  Add link…".to_string(),
        })
        .collect();
    render_list(
        f,
        &format!(" {label} "),
        &items,
        cursor,
        None,
        None,
        Some(links_picker_hints_line()),
    );
}

/// The relation chooser: both ends of every link type the site defines.
fn render_link_type_picker(
    f: &mut Frame,
    form: &CreateForm,
    field_idx: usize,
    editing: Option<usize>,
    cursor: usize,
) {
    let label = form
        .fields
        .get(field_idx)
        .map_or("Linked Issues", |f| f.label.as_str());
    // Editing names the issue on the other end, so it is clear the pick applies
    // to that link rather than starting another.
    let title = editing
        .and_then(|i| field_links(form, field_idx).get(i))
        .map_or_else(
            || format!(" {label} — relation "),
            |link| format!(" {} — relation ", link.issue.key),
        );

    // A failed or pending fetch would otherwise show as an empty list with no
    // explanation of which it was.
    let items: Vec<String> = match &form.link_types {
        CacheState::Failed(e) => vec![format!("(link types failed: {e} — q, then retry)")],
        CacheState::Idle | CacheState::Loading => vec!["(loading link types…)".to_string()],
        CacheState::Loaded(choices) => choices.iter().map(|c| c.label.clone()).collect(),
    };
    render_list(f, &title, &items, cursor, None, None, None);
}

/// The issue chooser for a link being added. Server-backed like the epic
/// chooser, and titled with the relation so it is clear what is being linked.
fn render_link_issue_picker(
    f: &mut Frame,
    form: &CreateForm,
    link_type: &LinkTypeChoice,
    query: &str,
    cursor: usize,
    searching: bool,
) {
    let title = if searching {
        format!(" {} — /{query}\u{258f} ", link_type.label)
    } else if query.is_empty() {
        format!(" {} … ", link_type.label)
    } else {
        format!(" {} — /{query} ", link_type.label)
    };
    let hints = Some(project_picker_hints_line(searching));
    let search = form.link_search.as_ref();

    if let Some(CacheState::Failed(e)) = search.map(|s| &s.results) {
        let items = [format!("(issue search failed: {e})")];
        render_list(f, &title, &items, 0, None, None, hints);
        return;
    }

    let items: Vec<String> = picker_link_issue_rows(form)
        .iter()
        .map(IssueRef::display)
        .collect();
    let note = search
        .is_some_and(|s| s.pending)
        .then_some("(searching\u{2026})");
    render_list(f, &title, &items, cursor, None, note, hints);
}

fn field_option_items(form: &CreateForm, field_idx: usize) -> (String, Vec<String>) {
    form.fields.get(field_idx).map_or_else(
        || (" Select ".to_string(), Vec::new()),
        |field| {
            (
                format!(" {} ", field.label),
                field.options.iter().map(|o| o.label.clone()).collect(),
            )
        },
    )
}

/// Shared picker list. `loading_note` is the footer shown under the list
/// while more rows are on their way.
fn render_list(
    f: &mut Frame,
    title: &str,
    items: &[String],
    cursor: usize,
    marks: Option<&HashSet<usize>>,
    loading_note: Option<&str>,
    hints: Option<Line<'static>>,
) {
    use ratatui::layout::{Constraint, Direction, Layout};

    let area = crate::tui::render::centered_rect(50, 60, f.area());
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER_FOCUS))
        .title(title.to_string())
        .title_bottom(hints.unwrap_or_else(|| picker_hints_line(marks.is_some())));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let (list_area, hint_area) = if loading_note.is_some() {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        (chunks[0], Some(chunks[1]))
    } else {
        (inner, None)
    };

    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                "No options",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::ITALIC),
            )),
            list_area,
        );
    } else {
        let list_items: Vec<ListItem> = items
            .iter()
            .enumerate()
            .map(|(i, label)| {
                marks.map_or_else(
                    || ListItem::new(Line::from(label.clone())),
                    |set| {
                        let check = if set.contains(&i) { "[✓] " } else { "[ ] " };
                        ListItem::new(Line::from(format!("{check}{label}")))
                    },
                )
            })
            .collect();
        let mut state = ListState::default();
        state.select(Some(cursor.min(items.len().saturating_sub(1))));
        let list = List::new(list_items)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
            .highlight_symbol("▶ ");
        f.render_stateful_widget(list, list_area, &mut state);
    }

    if let (Some(area), Some(note)) = (hint_area, loading_note) {
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("  {note}"),
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::ITALIC),
            )),
            area,
        );
    }
}

fn hints_line(mode: FormMode, editor: bool) -> Line<'static> {
    let mut spans = match mode {
        FormMode::Nav => vec![
            Span::raw("┤ "),
            Span::styled("j/k", Style::default().fg(Color::Blue)),
            Span::raw(" move | "),
            Span::styled("↵", Style::default().fg(Color::Blue)),
            Span::raw(" edit/pick | "),
        ],
        FormMode::Edit => vec![
            Span::raw("┤ "),
            Span::styled("↵", Style::default().fg(Color::Blue)),
            Span::raw(" next | "),
            Span::styled("Tab", Style::default().fg(Color::Blue)),
            Span::raw(" move | "),
        ],
    };
    if editor {
        spans.push(Span::styled(
            match mode {
                FormMode::Nav => "e",
                FormMode::Edit => "Ctrl+e",
            },
            Style::default().fg(Color::Blue),
        ));
        spans.push(Span::raw(" $EDITOR | "));
    }
    spans.push(Span::styled("Alt+↵", Style::default().fg(Color::Green)));
    spans.push(Span::raw(" create | "));
    match mode {
        FormMode::Nav => {
            spans.push(Span::styled("q", Style::default().fg(Color::Magenta)));
            spans.push(Span::raw(" cancel ├──"));
        }
        FormMode::Edit => {
            spans.push(Span::styled("Esc", Style::default().fg(Color::Magenta)));
            spans.push(Span::raw(" done ├──"));
        }
    }
    Line::from(spans).alignment(Alignment::Right)
}

fn picker_hints_line(multi: bool) -> Line<'static> {
    let mut spans = vec![
        Span::raw("┤ "),
        Span::styled("j/k", Style::default().fg(Color::Blue)),
        Span::raw(" nav | "),
    ];
    if multi {
        spans.push(Span::styled("Space", Style::default().fg(Color::Blue)));
        spans.push(Span::raw(" toggle | "));
    }
    spans.push(Span::styled("↵", Style::default().fg(Color::Green)));
    spans.push(Span::raw(if multi { " done | " } else { " select | " }));
    spans.push(Span::styled("q", Style::default().fg(Color::Magenta)));
    spans.push(Span::raw(" back ├──"));
    Line::from(spans).alignment(Alignment::Right)
}

/// Footer of the linked-issues list. `d` removes rather than Space toggling:
/// the rows are links already made, not options to check off.
fn links_picker_hints_line() -> Line<'static> {
    Line::from(vec![
        Span::raw("┤ "),
        Span::styled("j/k", Style::default().fg(Color::Blue)),
        Span::raw(" nav | "),
        Span::styled("↵", Style::default().fg(Color::Blue)),
        Span::raw(" relation | "),
        Span::styled("n", Style::default().fg(Color::Green)),
        Span::raw(" new | "),
        Span::styled("d", Style::default().fg(Color::Red)),
        Span::raw(" remove | "),
        Span::styled("q", Style::default().fg(Color::Magenta)),
        Span::raw(" back ├──"),
    ])
    .alignment(Alignment::Right)
}

fn project_picker_hints_line(searching: bool) -> Line<'static> {
    let spans = if searching {
        vec![
            Span::raw("┤ "),
            Span::styled("type", Style::default().fg(Color::Blue)),
            Span::raw(" filter | "),
            Span::styled("Esc", Style::default().fg(Color::Magenta)),
            Span::raw(" to list ├──"),
        ]
    } else {
        vec![
            Span::raw("┤ "),
            Span::styled("j/k", Style::default().fg(Color::Blue)),
            Span::raw(" nav | "),
            Span::styled("/", Style::default().fg(Color::Blue)),
            Span::raw(" search | "),
            Span::styled("↵", Style::default().fg(Color::Green)),
            Span::raw(" select | "),
            Span::styled("q", Style::default().fg(Color::Magenta)),
            Span::raw(" back ├──"),
        ]
    };
    Line::from(spans).alignment(Alignment::Right)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(ty: &str) -> Value {
        json!({ "type": ty })
    }

    #[test]
    fn summary_string_is_text_and_required_first() {
        let values = vec![
            json!({ "fieldId": "description", "name": "Description", "required": false,
                    "schema": { "type": "string", "system": "description" } }),
            json!({ "fieldId": "summary", "name": "Summary", "required": true,
                    "schema": { "type": "string", "system": "summary" } }),
        ];
        let fields = parse_create_fields(&values, false);
        assert_eq!(fields.len(), 2);
        // Required summary sorted first.
        assert_eq!(fields[0].field_id, "summary");
        assert_eq!(fields[0].widget, WidgetKind::Text);
        assert!(fields[0].required);
        // Description detected as ADF rich text.
        assert_eq!(fields[1].widget, WidgetKind::RichText);
    }

    #[test]
    fn project_and_issuetype_excluded() {
        let values = vec![
            json!({ "fieldId": "project", "name": "Project", "required": true,
                    "schema": { "type": "project", "system": "project" } }),
            json!({ "fieldId": "issuetype", "name": "Issue Type", "required": true,
                    "schema": { "type": "issuetype", "system": "issuetype" } }),
            json!({ "fieldId": "summary", "name": "Summary", "required": true,
                    "schema": { "type": "string" } }),
        ];
        let fields = parse_create_fields(&values, false);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_id, "summary");
    }

    #[test]
    fn schema_mapping_table() {
        assert_eq!(
            schema_to_widget(&schema("string"), false, false),
            WidgetKind::Text
        );
        assert_eq!(
            schema_to_widget(
                &json!({ "type": "string", "system": "description" }),
                false,
                false
            ),
            WidgetKind::RichText
        );
        assert_eq!(
            schema_to_widget(&schema("number"), false, false),
            WidgetKind::Number
        );
        assert_eq!(
            schema_to_widget(&schema("date"), false, false),
            WidgetKind::Date
        );
        assert_eq!(
            schema_to_widget(&schema("datetime"), false, false),
            WidgetKind::DateTime
        );
        assert_eq!(
            schema_to_widget(&schema("option"), true, false),
            WidgetKind::Select
        );
        assert_eq!(
            schema_to_widget(&json!({ "type": "array", "items": "option" }), true, false),
            WidgetKind::MultiSelect
        );
        assert_eq!(
            schema_to_widget(&schema("user"), true, false),
            WidgetKind::Select
        );
        assert_eq!(
            schema_to_widget(&schema("user"), false, false),
            WidgetKind::User
        );
        assert_eq!(
            schema_to_widget(&parent_schema(), false, false),
            WidgetKind::Epic
        );
        assert_eq!(
            schema_to_widget(&epic_link_schema(), false, false),
            WidgetKind::Epic
        );
        // A sub-task's parent is a story, which this picker cannot find.
        assert_eq!(
            schema_to_widget(&parent_schema(), false, true),
            WidgetKind::Unsupported
        );
        // The legacy Epic Link field always means an epic, sub-task or not.
        assert_eq!(
            schema_to_widget(&epic_link_schema(), false, true),
            WidgetKind::Epic
        );
        assert_eq!(
            schema_to_widget(&schema("timetracking"), false, false),
            WidgetKind::Unsupported
        );
    }

    #[test]
    fn parse_select_captures_option_shapes() {
        let values = vec![json!({
            "fieldId": "priority", "name": "Priority", "required": false,
            "schema": { "type": "priority" },
            "allowedValues": [ { "id": "1", "name": "High" }, { "id": "2", "name": "Low" } ]
        })];
        let fields = parse_create_fields(&values, false);
        assert_eq!(fields[0].widget, WidgetKind::Select);
        assert_eq!(fields[0].options.len(), 2);
        assert_eq!(fields[0].options[0].label, "High");
    }

    fn base_form() -> CreateForm {
        let mut form = CreateForm::open(
            ProjectField {
                id: "10000".into(),
                key: "PROJ".into(),
                name: "Project".into(),
            },
            Vec::new(),
        );
        form.issuetype = Some(IssueTypeField {
            id: "10001".into(),
            name: "Task".into(),
            subtask: false,
        });
        form.fields_state = CacheState::Loaded(());
        form
    }

    #[test]
    fn payload_has_project_issuetype_and_summary() {
        let mut form = base_form();
        form.fields = vec![FormField {
            field_id: "summary".into(),
            label: "Summary".into(),
            required: true,
            widget: WidgetKind::Text,
            value: FieldValue::Text {
                input: "Fix the bug".into(),
                cursor: 0,
            },
            options: Vec::new(),
        }];
        let payload = build_create_payload(&form).expect("valid");
        assert_eq!(payload["fields"]["project"]["key"], json!("PROJ"));
        assert_eq!(payload["fields"]["issuetype"]["id"], json!("10001"));
        assert_eq!(payload["fields"]["summary"], json!("Fix the bug"));
    }

    #[test]
    fn description_emitted_as_adf_only_when_filled() {
        let mut form = base_form();
        form.fields = vec![FormField {
            field_id: "description".into(),
            label: "Description".into(),
            required: false,
            widget: WidgetKind::RichText,
            value: FieldValue::Text {
                input: String::new(),
                cursor: 0,
            },
            options: Vec::new(),
        }];
        // Empty optional → omitted.
        let payload = build_create_payload(&form).expect("valid");
        assert!(payload["fields"].get("description").is_none());

        // Filled → ADF doc.
        if let FieldValue::Text { input, .. } = &mut form.fields[0].value {
            *input = "Hello".into();
        }
        let payload = build_create_payload(&form).expect("valid");
        assert_eq!(payload["fields"]["description"]["type"], json!("doc"));
    }

    #[test]
    fn select_emits_id_shape() {
        let mut form = base_form();
        form.fields = vec![FormField {
            field_id: "priority".into(),
            label: "Priority".into(),
            required: false,
            widget: WidgetKind::Select,
            value: FieldValue::SingleOption(Some(0)),
            options: vec![CreateOption {
                label: "High".into(),
                raw: json!({ "id": "1", "name": "High" }),
            }],
        }];
        let payload = build_create_payload(&form).expect("valid");
        assert_eq!(payload["fields"]["priority"], json!({ "id": "1" }));
    }

    #[test]
    fn multiselect_emits_array() {
        let mut form = base_form();
        let mut set = HashSet::new();
        set.insert(0);
        set.insert(1);
        form.fields = vec![FormField {
            field_id: "labels".into(),
            label: "Components".into(),
            required: false,
            widget: WidgetKind::MultiSelect,
            value: FieldValue::MultiOption(set),
            options: vec![
                CreateOption {
                    label: "a".into(),
                    raw: json!({ "name": "a" }),
                },
                CreateOption {
                    label: "b".into(),
                    raw: json!({ "name": "b" }),
                },
            ],
        }];
        let payload = build_create_payload(&form).expect("valid");
        assert_eq!(
            payload["fields"]["labels"],
            json!([{ "name": "a" }, { "name": "b" }])
        );
    }

    #[test]
    fn required_empty_errors() {
        let mut form = base_form();
        form.fields = vec![FormField {
            field_id: "summary".into(),
            label: "Summary".into(),
            required: true,
            widget: WidgetKind::Text,
            value: FieldValue::Text {
                input: "   ".into(),
                cursor: 0,
            },
            options: Vec::new(),
        }];
        assert!(build_create_payload(&form).is_err());
    }

    #[test]
    fn required_unsupported_errors() {
        let mut form = base_form();
        form.fields = vec![FormField {
            field_id: "customfield_1".into(),
            label: "Sprint".into(),
            required: true,
            widget: WidgetKind::Unsupported,
            value: FieldValue::Unsupported,
            options: Vec::new(),
        }];
        assert!(build_create_payload(&form).is_err());
    }

    #[test]
    fn missing_issuetype_errors() {
        let mut form = base_form();
        form.issuetype = None;
        assert!(build_create_payload(&form).is_err());
    }

    #[test]
    fn failed_fields_fetch_blocks_submit() {
        let mut form = base_form();
        form.fields_state = CacheState::Failed("HTTP 500".into());
        let err = build_create_payload(&form).expect_err("must not submit without fields");
        assert!(
            err.contains("HTTP 500"),
            "error should surface the cause: {err}"
        );
    }

    #[test]
    fn pending_fields_fetch_blocks_submit() {
        let mut form = base_form();
        form.fields_state = CacheState::Loading;
        assert!(build_create_payload(&form).is_err());
    }

    fn me() -> UserField {
        UserField {
            name: None,
            display_name: Some("Vlad Petrov".into()),
            account_id: Some("acct-1".into()),
        }
    }

    /// Reporter + assignee as createmeta returns them: `user` schema, no
    /// `allowedValues` (Jira omits them for user pickers).
    fn text_form() -> CreateForm {
        let mut form = base_form();
        form.fields = vec![
            FormField {
                field_id: "summary".into(),
                label: "Summary".into(),
                required: true,
                widget: WidgetKind::Text,
                value: FieldValue::Text {
                    input: String::new(),
                    cursor: 0,
                },
                options: Vec::new(),
            },
            FormField {
                field_id: "description".into(),
                label: "Description".into(),
                required: false,
                widget: WidgetKind::RichText,
                value: FieldValue::Text {
                    input: String::new(),
                    cursor: 0,
                },
                options: Vec::new(),
            },
            FormField {
                field_id: "priority".into(),
                label: "Priority".into(),
                required: false,
                widget: WidgetKind::Select,
                value: FieldValue::SingleOption(None),
                options: Vec::new(),
            },
        ];
        form
    }

    #[test]
    fn editor_is_offered_for_string_fields_only() {
        let mut form = text_form();
        for (focus, wanted) in [
            (0, None),                 // project picker
            (1, None),                 // issue type
            (2, Some(0)),              // summary — Text
            (3, Some(1)),              // description — RichText
            (4, None),                 // priority — Select
            (form.button_idx(), None), // Create button
        ] {
            form.focus = focus;
            form.pending_editor = None;
            request_editor(&mut form);
            assert_eq!(form.pending_editor, wanted, "focus {focus}");
            assert_eq!(editor_available(&form), wanted.is_some(), "focus {focus}");
        }
    }

    #[test]
    fn ctrl_e_leaves_edit_mode_and_requests_the_editor() {
        let mut form = text_form();
        form.focus = 3; // description
        form.mode = FormMode::Edit;
        handle_edit_mode_key(&mut form, KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert_eq!(form.mode, FormMode::Nav);
        assert_eq!(form.pending_editor, Some(1));
    }

    #[test]
    fn plain_e_in_edit_mode_is_just_a_character() {
        let mut form = text_form();
        form.focus = 3;
        form.mode = FormMode::Edit;
        handle_edit_mode_key(&mut form, KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(form.mode, FormMode::Edit);
        assert_eq!(form.pending_editor, None);
        assert_eq!(field_value_display(&form.fields[1]), "e");
    }

    #[test]
    fn editor_text_keeps_rich_text_line_breaks() {
        let mut form = text_form();
        apply_editor_text(&mut form, 1, "First line\n\n- a bullet");
        let FieldValue::Text { ref input, cursor } = form.fields[1].value else {
            panic!("description should stay a text value");
        };
        assert_eq!(input, "First line\n\n- a bullet");
        assert_eq!(cursor, input.chars().count());
    }

    #[test]
    fn editor_text_folds_line_breaks_in_single_line_fields() {
        let mut form = text_form();
        apply_editor_text(&mut form, 0, "Fix the\n  bug");
        let FieldValue::Text { ref input, .. } = form.fields[0].value else {
            panic!("summary should stay a text value");
        };
        assert_eq!(input, "Fix the bug");
    }

    #[test]
    fn editor_text_can_clear_a_field() {
        let mut form = text_form();
        apply_editor_text(&mut form, 1, "written then wiped");
        apply_editor_text(&mut form, 1, "");
        let FieldValue::Text { ref input, cursor } = form.fields[1].value else {
            panic!("description should stay a text value");
        };
        assert!(input.is_empty());
        assert_eq!(cursor, 0);
    }

    #[test]
    fn editor_text_ignores_a_field_that_is_gone() {
        let mut form = text_form();
        apply_editor_text(&mut form, 99, "nowhere");
    }

    #[test]
    fn multi_line_values_collapse_to_one_row() {
        let mut form = text_form();
        apply_editor_text(&mut form, 1, "Steps to reproduce\n\n1. open\n2. crash");
        assert_eq!(
            field_value_display(&form.fields[1]),
            "Steps to reproduce … (+3 lines)"
        );
    }

    #[test]
    fn single_line_values_collapse_to_themselves() {
        let mut form = text_form();
        apply_editor_text(&mut form, 0, "Fix the bug");
        assert_eq!(field_value_display(&form.fields[0]), "Fix the bug");
    }

    #[test]
    fn one_hidden_line_reads_singular() {
        let mut form = text_form();
        apply_editor_text(&mut form, 1, "first\nsecond");
        assert_eq!(field_value_display(&form.fields[1]), "first … (+1 line)");
    }

    #[test]
    fn caret_sits_after_the_typed_text() {
        let (rows, row, col) = wrap_edit_rows("abc", 3, 10);
        assert_eq!(rows, vec!["abc"]);
        assert_eq!((row, col), (0, 3));
    }

    #[test]
    fn caret_can_sit_mid_word() {
        let (_, row, col) = wrap_edit_rows("abcdef", 2, 10);
        assert_eq!((row, col), (0, 2));
    }

    #[test]
    fn explicit_newlines_start_new_rows() {
        let (rows, row, col) = wrap_edit_rows("ab\ncd", 4, 10);
        assert_eq!(rows, vec!["ab", "cd"]);
        // Cursor 4 is between 'c' and 'd' on the second row.
        assert_eq!((row, col), (1, 1));
    }

    #[test]
    fn a_trailing_newline_leaves_an_empty_row_to_type_into() {
        let (rows, row, col) = wrap_edit_rows("ab\n", 3, 10);
        assert_eq!(rows, vec!["ab", ""]);
        assert_eq!((row, col), (1, 0));
    }

    #[test]
    fn long_rows_wrap_at_the_field_width() {
        let (rows, row, col) = wrap_edit_rows("abcdef", 6, 3);
        assert_eq!(rows, vec!["abc", "def", ""]);
        // A row filled to the edge pushes the caret onto the next row.
        assert_eq!((row, col), (2, 0));
    }

    #[test]
    fn caret_lands_on_the_wrapped_row_it_belongs_to() {
        let (rows, row, col) = wrap_edit_rows("abcdef", 4, 3);
        // The landing row is always drawn, wherever the caret happens to be.
        assert_eq!(rows, vec!["abc", "def", ""]);
        assert_eq!((row, col), (1, 1));
    }

    #[test]
    fn an_empty_value_still_has_a_row_and_a_caret() {
        let (rows, row, col) = wrap_edit_rows("", 0, 10);
        assert_eq!(rows, vec![""]);
        assert_eq!((row, col), (0, 0));
    }

    #[test]
    fn a_cursor_past_the_end_clamps_to_the_end() {
        let (_, row, col) = wrap_edit_rows("ab", 99, 10);
        assert_eq!((row, col), (0, 2));
    }

    #[test]
    fn short_bodies_are_not_scrolled() {
        assert_eq!(row_window_offset(3, 2, 8), 0);
        assert_eq!(row_window_offset(8, 7, 8), 0);
    }

    #[test]
    fn the_row_window_follows_the_caret_down_and_stops_at_the_end() {
        assert_eq!(row_window_offset(20, 7, 8), 0);
        assert_eq!(row_window_offset(20, 8, 8), 1);
        assert_eq!(row_window_offset(20, 19, 8), 12);
        // Never scrolls past the last row.
        assert_eq!(row_window_offset(20, 25, 8), 12);
    }

    #[test]
    fn single_line_values_scroll_only_once_they_outgrow_the_row() {
        // 9 chars in a 10-wide row: the caret at the end still fits.
        assert_eq!(hscroll_offset(9, 9, 10), 0);
        // 10 chars: the caret one past the end needs a cell, so scroll by one.
        assert_eq!(hscroll_offset(10, 10, 10), 1);
        // Caret back at the start scrolls all the way back.
        assert_eq!(hscroll_offset(30, 0, 10), 0);
        // Mid-value: the caret sits on the last visible cell.
        assert_eq!(hscroll_offset(30, 15, 10), 6);
        // Never scrolls past the reachable end.
        assert_eq!(hscroll_offset(30, 30, 10), 21);
    }

    #[test]
    fn caret_column_accounts_for_the_label_column() {
        let form = text_form();
        let (rows, (col, row)) = edit_rows(&form.fields[0], "Fix", 3, 60, 4);
        assert_eq!(rows.len(), 1);
        // 2 marker cols + 16 label cols + 3 typed chars.
        assert_eq!((col, row), (21, 4));
    }

    #[test]
    fn a_long_label_pushes_the_value_column_right() {
        let mut form = text_form();
        form.fields[0].label = "A rather long field label".into();
        let (_, (col, _)) = edit_rows(&form.fields[0], "", 0, 60, 0);
        // 2 marker cols + 25 label chars + the required marker.
        assert_eq!(col, 28);
    }

    #[test]
    fn rich_text_grows_downwards_and_the_caret_follows() {
        let form = text_form();
        let (rows, (col, row)) = edit_rows(&form.fields[1], "one\ntwo", 7, 60, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!((col, row), (18 + 3, 3));
    }

    #[test]
    fn rich_text_never_grows_past_its_cap() {
        let form = text_form();
        let body = "x\n".repeat(40);
        let cursor = body.chars().count();
        let (rows, (_, row)) = edit_rows(&form.fields[1], &body, cursor, 60, 2);
        assert_eq!(rows.len(), MAX_EDIT_ROWS);
        // The caret stays on the last drawn row rather than running off-screen.
        assert_eq!(row as usize, 2 + MAX_EDIT_ROWS - 1);
    }

    #[test]
    fn values_too_wide_for_their_row_are_elided() {
        assert_eq!(elide("short", 10), "short");
        assert_eq!(elide("exactly-10", 10), "exactly-10");
        assert_eq!(elide("far too long to fit", 10), "far too l…");
        assert_eq!(elide("anything", 0), "");
    }

    #[test]
    fn the_caret_row_counts_the_rows_above_it() {
        let mut form = text_form();
        form.mode = FormMode::Edit;
        // Project and Type sit above the first field.
        form.focus = 2; // Summary
        assert_eq!(form_lines(&form, 60, false).1, Some((18, 2)));
        form.focus = 3; // Description
        assert_eq!(form_lines(&form, 60, false).1, Some((18, 3)));
    }

    #[test]
    fn an_expanded_rich_text_field_pushes_the_rows_below_it_down() {
        let mut form = text_form();
        form.mode = FormMode::Edit;
        form.focus = 3;
        let plain = form_lines(&form, 60, false).0.len();

        apply_editor_text(&mut form, 1, "one\ntwo\nthree");
        let (lines, caret) = form_lines(&form, 60, false);
        // Caret left at the end of "three", on the third row of the block.
        assert_eq!(caret, Some((18 + 5, 5)));
        // Two extra rows for the body; Priority, spacer and Create moved down.
        assert_eq!(lines.len(), plain + 2);
    }

    #[test]
    fn a_covered_form_draws_no_caret() {
        let mut form = text_form();
        form.mode = FormMode::Edit;
        form.focus = 2;
        assert!(form_lines(&form, 60, true).1.is_none());
    }

    #[test]
    fn nav_mode_draws_no_caret() {
        let mut form = text_form();
        form.focus = 2;
        assert!(form_lines(&form, 60, false).1.is_none());
    }

    #[test]
    fn prose_wraps_on_word_boundaries() {
        assert_eq!(wrap_words("one two three", 20), vec!["one two three"]);
        assert_eq!(wrap_words("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap_words("", 10), vec![""]);
    }

    #[test]
    fn a_word_too_long_to_fit_is_broken() {
        assert_eq!(
            wrap_words("ab abcdefghij", 4),
            vec!["ab", "abcd", "efgh", "ij"]
        );
    }

    fn user_form(me: Option<UserField>) -> CreateForm {
        let mut form = base_form();
        form.current_user = me;
        form.fields = parse_create_fields(
            &[
                json!({ "fieldId": "reporter", "name": "Reporter", "required": false,
                        "schema": { "type": "user", "system": "reporter" } }),
                json!({ "fieldId": "assignee", "name": "Assignee", "required": false,
                        "schema": { "type": "user", "system": "assignee" } }),
            ],
            false,
        );
        form
    }

    fn reporter_of(form: &CreateForm) -> Option<&UserField> {
        match &form.fields.iter().find(|f| f.field_id == "reporter")?.value {
            FieldValue::User(u) => u.as_ref(),
            _ => None,
        }
    }

    #[test]
    fn reporter_prefilled_with_current_user() {
        let mut form = user_form(Some(me()));
        apply_reporter_prefill(&mut form);
        assert_eq!(
            reporter_of(&form).and_then(|u| u.account_id.clone()),
            Some("acct-1".into())
        );
        // Only the reporter: other user fields stay for Jira to default.
        let assignee = form.fields.iter().find(|f| f.field_id == "assignee");
        assert!(matches!(
            assignee.map(|f| &f.value),
            Some(FieldValue::User(None))
        ));
    }

    #[test]
    fn prefilled_reporter_is_submitted_as_account_id() {
        let mut form = user_form(Some(me()));
        apply_reporter_prefill(&mut form);
        let payload = build_create_payload(&form).expect("valid");
        assert_eq!(
            payload["fields"]["reporter"],
            json!({ "accountId": "acct-1" })
        );
        // Unset user fields are omitted so Jira applies its own default.
        assert!(payload["fields"].get("assignee").is_none());
    }

    #[test]
    fn server_user_without_account_id_submits_name() {
        let mut form = user_form(Some(UserField {
            name: Some("vpetrov".into()),
            display_name: Some("Vlad Petrov".into()),
            account_id: None,
        }));
        apply_reporter_prefill(&mut form);
        let payload = build_create_payload(&form).expect("valid");
        assert_eq!(payload["fields"]["reporter"], json!({ "name": "vpetrov" }));
    }

    #[test]
    fn prefill_waits_for_the_current_user_and_runs_once() {
        // Fields arrive before /myself does: nothing to fill in yet.
        let mut form = user_form(None);
        apply_reporter_prefill(&mut form);
        assert!(reporter_of(&form).is_none());
        assert_eq!(
            form.prefill,
            Prefill::Pending,
            "must retry once the user is known"
        );

        // The user lands and the reporter fills in.
        form.current_user = Some(me());
        apply_reporter_prefill(&mut form);
        assert!(reporter_of(&form).is_some());

        // A reporter the user cleared afterwards stays cleared.
        if let Some(field) = form.fields.iter_mut().find(|f| f.field_id == "reporter") {
            field.value = FieldValue::User(None);
        }
        apply_reporter_prefill(&mut form);
        assert!(reporter_of(&form).is_none());
    }

    #[test]
    fn prefill_does_not_run_before_fields_load() {
        let mut form = user_form(Some(me()));
        form.fields_state = CacheState::Loading;
        apply_reporter_prefill(&mut form);
        assert!(reporter_of(&form).is_none());
        assert_eq!(form.prefill, Prefill::Pending);
    }

    fn parent_schema() -> Value {
        json!({ "type": "issuelink", "system": "parent" })
    }

    fn epic_link_schema() -> Value {
        json!({ "type": "any", "custom": "com.pyxis.greenhopper.jira:gh-epic-link" })
    }

    fn epic(key: &str) -> IssueRef {
        IssueRef {
            key: key.into(),
            summary: format!("{key} epic"),
        }
    }

    /// A form whose one field is the epic link, in either spelling.
    fn epic_form(field_id: &str, schema: Value) -> CreateForm {
        let mut form = base_form();
        form.fields = parse_create_fields(
            &[
                json!({ "fieldId": field_id, "name": "Parent", "required": false,
                      "schema": schema }),
            ],
            false,
        );
        form
    }

    #[test]
    fn epic_starts_unset_and_is_omitted_from_the_payload() {
        let form = epic_form("parent", parent_schema());
        assert_eq!(form.fields[0].widget, WidgetKind::Epic);
        assert!(matches!(form.fields[0].value, FieldValue::Epic(None)));
        let payload = build_create_payload(&form).expect("valid");
        assert!(payload["fields"].get("parent").is_none());
    }

    #[test]
    fn parent_field_submits_the_epic_as_an_issue_object() {
        let mut form = epic_form("parent", parent_schema());
        form.fields[0].value = FieldValue::Epic(Some(epic("PROJ-7")));
        let payload = build_create_payload(&form).expect("valid");
        assert_eq!(payload["fields"]["parent"], json!({ "key": "PROJ-7" }));
    }

    #[test]
    fn legacy_epic_link_submits_the_bare_key() {
        let mut form = epic_form("customfield_10014", epic_link_schema());
        form.fields[0].value = FieldValue::Epic(Some(epic("PROJ-7")));
        let payload = build_create_payload(&form).expect("valid");
        assert_eq!(payload["fields"]["customfield_10014"], json!("PROJ-7"));
    }

    #[test]
    fn epic_rows_lead_with_a_way_to_pick_none() {
        let rows = epic_picker_rows(&[epic("PROJ-1"), epic("PROJ-2")]);
        assert_eq!(rows.len(), 3);
        assert!(epic_row_value(&rows[0]).is_none());
        assert_eq!(epic_row_label(&rows[0]), "—  (no epic)");
        assert_eq!(
            epic_row_value(&rows[1]).map(|e| e.key.as_str()),
            Some("PROJ-1")
        );
        assert_eq!(epic_row_label(&rows[1]), "PROJ-1  PROJ-1 epic");
    }

    /// Enter on a row writes it into the field and closes the picker; Enter on
    /// the unset row clears it again.
    #[test]
    fn picking_an_epic_sets_the_field_and_closes_the_picker() {
        let mut form = epic_form("parent", parent_schema());
        form.focus = 2;
        activate_field(&mut form);
        assert!(matches!(form.picker, Some(CreatePicker::Epic { .. })));
        assert!(form.epic_search.is_some(), "opening asks for the epics");

        // The search lands, and the second row (first match) is chosen.
        if let Some(search) = form.epic_search.as_mut() {
            search.pending = false;
            search.results = CacheState::Loaded(vec![epic("PROJ-7")]);
        }
        let picker = form.picker.take().expect("open");
        handle_epic_picker_key(&mut form, KeyCode::Char('j'), picker);
        let picker = form.picker.take().expect("still open");
        handle_epic_picker_key(&mut form, KeyCode::Enter, picker);

        assert!(form.picker.is_none(), "choosing closes the picker");
        assert!(
            form.epic_search.is_none(),
            "matches do not outlive the picker"
        );
        assert!(matches!(
            &form.fields[0].value,
            FieldValue::Epic(Some(e)) if e.key == "PROJ-7"
        ));
        assert!(form_is_dirty(&form), "a chosen epic is unsaved work");

        // Reopen and take the unset row: back to letting Jira decide.
        form.focus = 2;
        activate_field(&mut form);
        let picker = form.picker.take().expect("open");
        handle_epic_picker_key(&mut form, KeyCode::Enter, picker);
        assert!(matches!(form.fields[0].value, FieldValue::Epic(None)));
    }

    // ── Linked issues ──────────────────────────────────────────────────────

    fn link_types() -> Vec<IssueLinkType> {
        vec![
            IssueLinkType {
                name: "Blocks".into(),
                inward: "is blocked by".into(),
                outward: "blocks".into(),
            },
            IssueLinkType {
                name: "Relates".into(),
                inward: "relates to".into(),
                outward: "relates to".into(),
            },
        ]
    }

    fn issue(key: &str) -> IssueRef {
        IssueRef {
            key: key.into(),
            summary: format!("{key} summary"),
        }
    }

    /// A form whose one field is `issuelinks`, as createmeta describes it.
    fn links_form() -> CreateForm {
        let mut form = base_form();
        form.fields = parse_create_fields(
            &[json!({
                "fieldId": "issuelinks", "name": "Linked Issues", "required": false,
                "schema": { "type": "array", "items": "issuelinks", "system": "issuelinks" }
            })],
            false,
        );
        form.link_types = CacheState::Loaded(link_type_choices(&link_types()));
        form
    }

    /// Route `code` through the open linked-issues list, as
    /// `handle_picker_input` does: take the picker, then hand on its fields.
    fn press_links(form: &mut CreateForm, code: KeyCode) {
        let Some(CreatePicker::IssueLinks { field_idx, cursor }) = form.picker.take() else {
            panic!("the links list is not open");
        };
        handle_links_picker_key(form, code, field_idx, cursor);
    }

    /// Same for the relation chooser.
    fn press_link_type(form: &mut CreateForm, code: KeyCode) {
        let Some(CreatePicker::LinkType {
            field_idx,
            editing,
            cursor,
        }) = form.picker.take()
        else {
            panic!("the relation chooser is not open");
        };
        handle_link_type_picker_key(form, code, field_idx, editing, cursor);
    }

    #[test]
    fn issuelinks_field_is_a_link_widget_starting_empty() {
        let form = links_form();
        assert_eq!(form.fields[0].widget, WidgetKind::IssueLinks);
        assert!(matches!(
            form.fields[0].value,
            FieldValue::IssueLinks(ref l) if l.is_empty()
        ));
        // Nothing added → no `update` block at all, not an empty one.
        let payload = build_create_payload(&form).expect("valid");
        assert!(payload.get("update").is_none());
        assert!(payload["fields"].get("issuelinks").is_none());
        assert!(!form_is_dirty(&form));
    }

    #[test]
    fn link_type_choices_expand_both_ends_and_collapse_symmetric_ones() {
        let choices = link_type_choices(&link_types());
        // Blocks gives two readings, "relates to" only one.
        assert_eq!(choices.len(), 3);
        assert_eq!(choices[0].label, "blocks");
        assert_eq!(choices[0].direction, LinkDirection::Outward);
        assert_eq!(choices[1].label, "is blocked by");
        assert_eq!(choices[1].direction, LinkDirection::Inward);
        assert_eq!(choices[2].label, "relates to");
        assert_eq!(choices[2].name, "Relates");
    }

    /// The picked relation reads from the new issue outwards, so the target sits
    /// on the opposite side: "blocks" puts it on `outwardIssue`.
    #[test]
    fn links_travel_in_update_with_the_target_on_the_far_side() {
        let mut form = links_form();
        let choices = link_type_choices(&link_types());
        form.fields[0].value = FieldValue::IssueLinks(vec![
            IssueLinkDraft {
                link_type: choices[0].clone(), // blocks (outward)
                issue: issue("OPS-42"),
            },
            IssueLinkDraft {
                link_type: choices[1].clone(), // is blocked by (inward)
                issue: issue("OPS-9"),
            },
        ]);
        let payload = build_create_payload(&form).expect("valid");
        assert!(
            payload["fields"].get("issuelinks").is_none(),
            "Jira rejects issuelinks in `fields` on create"
        );
        assert_eq!(
            payload["update"]["issuelinks"],
            json!([
                { "add": { "type": { "name": "Blocks" }, "outwardIssue": { "key": "OPS-42" } } },
                { "add": { "type": { "name": "Blocks" }, "inwardIssue": { "key": "OPS-9" } } },
            ])
        );
    }

    #[test]
    fn required_links_field_with_nothing_added_errors() {
        let mut form = links_form();
        form.fields[0].required = true;
        let err = build_create_payload(&form).expect_err("must not submit");
        assert!(err.contains("Linked Issues"), "{err}");
    }

    #[test]
    fn link_rows_end_with_the_add_row() {
        assert_eq!(link_rows(0), vec![LinkRow::Add]);
        assert_eq!(
            link_rows(2),
            vec![LinkRow::Existing(0), LinkRow::Existing(1), LinkRow::Add]
        );
    }

    /// The full add walk: list → relation → issue, and back to the list with
    /// the new link under the cursor.
    #[test]
    fn adding_a_link_walks_both_pickers_and_returns_to_the_list() {
        let mut form = links_form();
        form.focus = 2;
        activate_field(&mut form);
        assert!(matches!(form.picker, Some(CreatePicker::IssueLinks { .. })));

        // Enter on the list opens the relation chooser.
        press_links(&mut form, KeyCode::Enter);
        assert!(matches!(form.picker, Some(CreatePicker::LinkType { .. })));
        assert!(
            !form.needs_link_types_fetch,
            "already-loaded relations are not refetched"
        );

        // Second relation ("is blocked by") opens the issue chooser.
        press_link_type(&mut form, KeyCode::Char('j'));
        press_link_type(&mut form, KeyCode::Enter);
        assert!(matches!(
            form.picker,
            Some(CreatePicker::LinkIssue { ref link_type, .. }) if link_type.label == "is blocked by"
        ));
        assert!(form.link_search.is_some(), "opening asks for candidates");

        // The search lands and its first match is taken.
        if let Some(search) = form.link_search.as_mut() {
            search.pending = false;
            search.results = CacheState::Loaded(vec![issue("OPS-9")]);
        }
        let picker = form.picker.take().expect("open");
        handle_link_issue_picker_key(&mut form, KeyCode::Enter, picker);

        assert!(
            matches!(
                form.picker,
                Some(CreatePicker::IssueLinks { cursor: 0, .. })
            ),
            "back on the list, cursor on the new link: {:?}",
            form.picker
        );
        assert!(
            form.link_search.is_none(),
            "matches do not outlive the picker"
        );
        let FieldValue::IssueLinks(ref links) = form.fields[0].value else {
            panic!("still a links field");
        };
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].issue.key, "OPS-9");
        assert_eq!(links[0].link_type.direction, LinkDirection::Inward);
        assert!(form_is_dirty(&form), "an added link is unsaved work");
    }

    #[test]
    fn the_same_link_twice_is_not_added_twice() {
        let mut form = links_form();
        let choice = link_type_choices(&link_types())[0].clone();
        add_link(&mut form, 0, choice.clone(), issue("OPS-42"));
        add_link(&mut form, 0, choice, issue("OPS-42"));
        let FieldValue::IssueLinks(ref links) = form.fields[0].value else {
            panic!("still a links field");
        };
        assert_eq!(links.len(), 1, "Jira would reject the duplicate operation");
        assert!(
            matches!(
                form.picker,
                Some(CreatePicker::IssueLinks { cursor: 0, .. })
            ),
            "the cursor lands on the existing row: {:?}",
            form.picker
        );
    }

    #[test]
    fn d_removes_the_highlighted_link_and_keeps_the_cursor_on_a_row() {
        let mut form = links_form();
        let choices = link_type_choices(&link_types());
        add_link(&mut form, 0, choices[0].clone(), issue("OPS-1"));
        add_link(&mut form, 0, choices[0].clone(), issue("OPS-2"));

        // Cursor sits on the second link (row 1); remove it.
        press_links(&mut form, KeyCode::Char('d'));
        let FieldValue::IssueLinks(ref links) = form.fields[0].value else {
            panic!("still a links field");
        };
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].issue.key, "OPS-1");
        assert!(
            matches!(
                form.picker,
                Some(CreatePicker::IssueLinks { cursor: 1, .. })
            ),
            "cursor clamps to a row that still exists — here the add row: {:?}",
            form.picker
        );

        // `d` on the add row removes nothing.
        press_links(&mut form, KeyCode::Char('d'));
        assert_eq!(field_links(&form, 0).len(), 1);

        // Back onto the remaining link, which `d` does remove.
        press_links(&mut form, KeyCode::Char('k'));
        press_links(&mut form, KeyCode::Char('d'));
        assert!(field_links(&form, 0).is_empty());
    }

    #[test]
    fn n_starts_a_new_link_from_a_row_that_already_has_one() {
        let mut form = links_form();
        let choices = link_type_choices(&link_types());
        add_link(&mut form, 0, choices[0].clone(), issue("OPS-1"));

        // Cursor is on the existing link, where Enter would edit it.
        press_links(&mut form, KeyCode::Char('n'));
        assert!(
            matches!(
                form.picker,
                Some(CreatePicker::LinkType { editing: None, .. })
            ),
            "n adds rather than edits: {:?}",
            form.picker
        );
    }

    /// Enter on a link reopens the relation chooser for it, starting on the
    /// relation it has; picking another swaps it and leaves the issue alone.
    #[test]
    fn enter_on_a_link_edits_its_relation_in_place() {
        let mut form = links_form();
        let choices = link_type_choices(&link_types());
        add_link(&mut form, 0, choices[1].clone(), issue("OPS-1")); // is blocked by

        press_links(&mut form, KeyCode::Enter);
        assert!(
            matches!(
                form.picker,
                Some(CreatePicker::LinkType {
                    editing: Some(0),
                    cursor: 1,
                    ..
                })
            ),
            "starts on the relation the link already has: {:?}",
            form.picker
        );

        // Up one row: "blocks".
        press_link_type(&mut form, KeyCode::Char('k'));
        press_link_type(&mut form, KeyCode::Enter);

        let links = field_links(&form, 0);
        assert_eq!(links.len(), 1, "editing must not add a second link");
        assert_eq!(links[0].link_type.label, "blocks");
        assert_eq!(links[0].link_type.direction, LinkDirection::Outward);
        assert_eq!(links[0].issue.key, "OPS-1", "the issue is untouched");
        assert!(
            matches!(
                form.picker,
                Some(CreatePicker::IssueLinks { cursor: 0, .. })
            ),
            "back on the list, on the link just edited: {:?}",
            form.picker
        );
    }

    /// Backing out of a step lands on the previous one, not on the form.
    #[test]
    fn backing_out_of_the_add_walk_retreats_one_step_at_a_time() {
        let mut form = links_form();
        form.focus = 2;
        activate_field(&mut form);
        press_links(&mut form, KeyCode::Enter);
        press_link_type(&mut form, KeyCode::Enter);

        // Issue chooser → relation chooser → list → form.
        let picker = form.picker.take().expect("open");
        handle_link_issue_picker_key(&mut form, KeyCode::Char('q'), picker);
        assert!(matches!(form.picker, Some(CreatePicker::LinkType { .. })));
        assert!(form.link_search.is_none(), "the search is abandoned too");

        press_link_type(&mut form, KeyCode::Char('q'));
        assert!(
            matches!(
                form.picker,
                Some(CreatePicker::IssueLinks { cursor: 0, .. })
            ),
            "back on the add row the walk started from: {:?}",
            form.picker
        );

        press_links(&mut form, KeyCode::Char('q'));
        assert!(
            form.picker.is_none(),
            "leaving the list returns to the form"
        );
    }

    #[test]
    fn leaving_the_issue_chooser_returns_to_the_relation_that_was_picked() {
        let mut form = links_form();
        form.focus = 2;
        activate_field(&mut form);
        press_links(&mut form, KeyCode::Enter);
        // Third relation ("relates to").
        press_link_type(&mut form, KeyCode::Char('j'));
        press_link_type(&mut form, KeyCode::Char('j'));
        press_link_type(&mut form, KeyCode::Enter);

        let picker = form.picker.take().expect("open");
        handle_link_issue_picker_key(&mut form, KeyCode::Char('q'), picker);
        assert!(
            matches!(
                form.picker,
                Some(CreatePicker::LinkType {
                    editing: None,
                    cursor: 2,
                    ..
                })
            ),
            "{:?}",
            form.picker
        );
    }

    #[test]
    fn opening_the_relation_chooser_asks_for_the_link_types_once() {
        let mut form = links_form();
        form.link_types = CacheState::Idle;
        open_link_type_picker(&mut form, 0, None);
        assert!(form.needs_link_types_fetch);
        assert!(matches!(form.link_types, CacheState::Loading));

        // The dispatcher takes the flag; reopening while loading must not refetch.
        form.needs_link_types_fetch = false;
        open_link_type_picker(&mut form, 0, None);
        assert!(!form.needs_link_types_fetch);

        // A failure is retried on the next open, though.
        form.link_types = CacheState::Failed("HTTP 500".into());
        open_link_type_picker(&mut form, 0, None);
        assert!(form.needs_link_types_fetch);
    }

    #[test]
    fn typing_in_the_link_picker_requeries_under_a_fresh_token() {
        let mut form = links_form();
        let choice = link_type_choices(&link_types())[0].clone();
        open_link_issue_picker(&mut form, 0, choice);
        let first_token = form.link_search.as_ref().expect("open").token;

        let picker = form.picker.take().expect("open");
        handle_link_issue_picker_key(&mut form, KeyCode::Char('/'), picker);
        let picker = form.picker.take().expect("open");
        handle_link_issue_picker_key(&mut form, KeyCode::Char('o'), picker);

        let search = form.link_search.as_ref().expect("open");
        assert_eq!(search.query, "o");
        assert!(
            search.token > first_token,
            "stale responses must be droppable"
        );
        assert!(!search.spawned, "the dispatcher has yet to send it");
        assert!(
            search.changed_at.is_some(),
            "a typed query waits out the debounce"
        );
    }

    #[test]
    fn the_field_row_shows_one_link_in_full_and_counts_the_rest() {
        let mut form = links_form();
        assert_eq!(field_value_display(&form.fields[0]), "(none)  ▾");
        let choices = link_type_choices(&link_types());
        add_link(&mut form, 0, choices[0].clone(), issue("OPS-1"));
        assert_eq!(
            field_value_display(&form.fields[0]),
            "blocks  OPS-1  OPS-1 summary  ▾"
        );
        add_link(&mut form, 0, choices[0].clone(), issue("OPS-2"));
        assert_eq!(field_value_display(&form.fields[0]), "2 links  ▾");
    }

    #[test]
    fn typing_in_the_epic_picker_requeries_under_a_fresh_token() {
        let mut form = epic_form("parent", parent_schema());
        form.focus = 2;
        activate_field(&mut form);
        let first_token = form.epic_search.as_ref().expect("open").token;

        let picker = form.picker.take().expect("open");
        handle_epic_picker_key(&mut form, KeyCode::Char('/'), picker);
        let picker = form.picker.take().expect("open");
        handle_epic_picker_key(&mut form, KeyCode::Char('p'), picker);

        let search = form.epic_search.as_ref().expect("open");
        assert_eq!(search.query, "p");
        assert!(
            search.token > first_token,
            "stale responses must be droppable"
        );
        assert!(!search.spawned, "the dispatcher has yet to send it");
        assert!(
            search.changed_at.is_some(),
            "a typed query waits out the debounce"
        );
    }

    #[test]
    fn switching_project_drops_epic_matches() {
        let mut form = epic_form("parent", parent_schema());
        form.focus = 2;
        activate_field(&mut form);
        form.set_project(ProjectField {
            id: "10001".into(),
            key: "OTHER".into(),
            name: "Other".into(),
        });
        assert!(form.epic_search.is_none(), "epics are project-scoped");
    }

    #[test]
    fn discard_prompt_keys_have_no_selector() {
        use DiscardChoice::{Discard, KeepEditing};
        assert_eq!(discard_confirm_choice(KeyCode::Enter), Some(Discard));
        assert_eq!(discard_confirm_choice(KeyCode::Char('y')), Some(Discard));
        assert_eq!(discard_confirm_choice(KeyCode::Esc), Some(KeepEditing));
        assert_eq!(
            discard_confirm_choice(KeyCode::Char('q')),
            Some(KeepEditing)
        );
        // The two-button selector is gone, so its movement keys do nothing.
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Tab,
            KeyCode::Char('h'),
            KeyCode::Char('l'),
        ] {
            assert_eq!(discard_confirm_choice(code), None, "{code:?} must be inert");
        }
    }

    #[test]
    fn prefilled_reporter_alone_is_not_dirty() {
        let mut form = user_form(Some(me()));
        apply_reporter_prefill(&mut form);
        assert!(
            !form_is_dirty(&form),
            "an untouched form must close without the discard prompt"
        );
    }

    fn user(id: &str, display: &str) -> UserField {
        UserField {
            name: None,
            display_name: Some(display.into()),
            account_id: Some(id.into()),
        }
    }

    /// Feed one key to the open user picker, the way `handle_picker_input`
    /// does: the picker is taken, then handed to the handler.
    fn press(form: &mut CreateForm, code: KeyCode) {
        let picker = form.picker.take().expect("picker open");
        handle_user_picker_key(form, code, picker);
    }

    fn row_labels(rows: &[UserRow]) -> Vec<String> {
        rows.iter().map(user_row_label).collect()
    }

    #[test]
    fn picker_leads_with_unset_then_pins_me() {
        let results = vec![user("acct-2", "Ada"), me(), user("acct-3", "Grace")];
        let rows = user_picker_rows(Some(&me()), "", &results);
        assert_eq!(
            row_labels(&rows),
            vec![
                "—  (leave to Jira)",
                "Vlad Petrov  (me)",
                "Ada",
                "Grace" // the pinned user is not repeated among the results
            ]
        );
    }

    #[test]
    fn picker_drops_the_pin_once_a_query_narrows_the_list() {
        // With a query the server decides who matches — including whether the
        // current user does, so pinning them would be a lie.
        let rows = user_picker_rows(Some(&me()), "ada", &[user("acct-2", "Ada")]);
        assert_eq!(row_labels(&rows), vec!["—  (leave to Jira)", "Ada"]);
    }

    #[test]
    fn picker_offers_unset_even_before_results_arrive() {
        let rows = user_picker_rows(None, "", &[]);
        assert_eq!(row_labels(&rows), vec!["—  (leave to Jira)"]);
        assert!(user_row_value(&rows[0]).is_none());
    }

    /// Server/DC users carry no account id, so identity falls back to username.
    #[test]
    fn picker_dedupes_server_users_by_name() {
        let named = |n: &str| UserField {
            name: Some(n.into()),
            display_name: Some(n.into()),
            account_id: None,
        };
        let rows = user_picker_rows(Some(&named("vpetrov")), "", &[named("vpetrov")]);
        assert_eq!(
            row_labels(&rows),
            vec!["—  (leave to Jira)", "vpetrov  (me)"]
        );
    }

    /// The picker's cursor indexes the rows, so the row list and the value it
    /// resolves to must stay in step.
    #[test]
    fn picking_a_row_yields_that_user() {
        let rows = user_picker_rows(Some(&me()), "", &[user("acct-2", "Ada")]);
        assert!(user_row_value(&rows[0]).is_none());
        assert_eq!(
            user_row_value(&rows[1]).and_then(|u| u.account_id.clone()),
            Some("acct-1".into())
        );
        assert_eq!(
            user_row_value(&rows[2]).and_then(|u| u.account_id.clone()),
            Some("acct-2".into())
        );
    }

    #[test]
    fn typing_in_the_picker_restarts_the_search_under_a_new_token() {
        let mut form = user_form(Some(me()));
        let idx = form
            .fields
            .iter()
            .position(|f| f.field_id == "assignee")
            .expect("assignee field");
        open_user_picker(&mut form, idx);
        let first = form.user_search.as_ref().expect("search started").token;

        // Results for the empty query land, then the user starts typing.
        if let Some(search) = form.user_search.as_mut() {
            search.spawned = true;
            search.pending = false;
            search.results = CacheState::Loaded(vec![user("acct-2", "Ada")]);
        }
        if let Some(CreatePicker::User { searching, .. }) = form.picker.as_mut() {
            *searching = true;
        }
        press(&mut form, KeyCode::Char('a'));

        let search = form.user_search.as_ref().expect("search still open");
        assert_eq!(search.query, "a");
        assert!(search.token > first, "stale responses must be droppable");
        assert!(!search.spawned, "the new query still has to be dispatched");
        assert!(
            search.results.loaded().is_some(),
            "previous matches stay on screen while the next ones load"
        );
    }

    #[test]
    fn choosing_a_user_sets_the_field_and_closes_the_picker() {
        let mut form = user_form(Some(me()));
        let idx = form
            .fields
            .iter()
            .position(|f| f.field_id == "assignee")
            .expect("assignee field");
        open_user_picker(&mut form, idx);
        if let Some(search) = form.user_search.as_mut() {
            search.results = CacheState::Loaded(vec![user("acct-2", "Ada")]);
        }

        // Rows are [unset, me, Ada]; move to Ada and take her.
        for _ in 0..2 {
            press(&mut form, KeyCode::Char('j'));
        }
        press(&mut form, KeyCode::Enter);

        assert!(form.picker.is_none(), "choosing closes the picker");
        assert!(form.user_search.is_none(), "and drops the search state");
        let payload = build_create_payload(&form).expect("valid");
        assert_eq!(
            payload["fields"]["assignee"],
            json!({ "accountId": "acct-2" })
        );
    }

    #[test]
    fn choosing_unset_clears_a_prefilled_reporter() {
        let mut form = user_form(Some(me()));
        apply_reporter_prefill(&mut form);
        let idx = form
            .fields
            .iter()
            .position(|f| f.field_id == "reporter")
            .expect("reporter field");
        open_user_picker(&mut form, idx);

        // Cursor starts on the unset row.
        press(&mut form, KeyCode::Enter);
        assert!(reporter_of(&form).is_none());
        let payload = build_create_payload(&form).expect("valid");
        assert!(payload["fields"].get("reporter").is_none());
    }

    #[test]
    fn leaving_the_picker_keeps_the_field_as_it_was() {
        let mut form = user_form(Some(me()));
        apply_reporter_prefill(&mut form);
        let idx = form
            .fields
            .iter()
            .position(|f| f.field_id == "reporter")
            .expect("reporter field");
        open_user_picker(&mut form, idx);
        press(&mut form, KeyCode::Char('q'));

        assert!(form.picker.is_none());
        assert_eq!(
            reporter_of(&form).and_then(|u| u.account_id.clone()),
            Some("acct-1".into())
        );
    }

    #[test]
    fn required_user_field_left_empty_errors() {
        let mut form = user_form(None);
        form.fields[0].required = true;
        assert!(build_create_payload(&form).is_err());
    }

    #[test]
    fn merge_cached_appends_new_and_skips_dupes() {
        let mut current = vec![
            ProjectField {
                id: "10".into(),
                key: "DEV".into(),
                name: "Development".into(),
            },
            ProjectField {
                id: "11".into(),
                key: "OPS".into(),
                name: "Operations".into(),
            },
        ];
        let cached = vec![
            ProjectInfo {
                key: "dev".into(),
                name: "Development (cached)".into(),
            }, // dupe (case)
            ProjectInfo {
                key: "MKTG".into(),
                name: "Marketing".into(),
            },
            ProjectInfo {
                key: "HR".into(),
                name: "HR".into(),
            },
            ProjectInfo {
                key: "MKTG".into(),
                name: "Dup".into(),
            }, // dupe within cache
        ];
        merge_cached_projects(&mut current, &cached);
        // Original two preserved in order, then new ones appended in cache order,
        // case-insensitive dedup against current and within cache.
        assert_eq!(current.len(), 4);
        assert_eq!(current[0].key, "DEV");
        assert_eq!(current[0].name, "Development"); // original kept, not overwritten
        assert_eq!(current[1].key, "OPS");
        assert_eq!(current[2].key, "MKTG");
        assert_eq!(current[2].id, ""); // promoted ProjectInfo has empty id
        assert_eq!(current[3].key, "HR");
    }

    #[test]
    fn distinct_projects_dedupes_by_key() {
        let mk = |key: &str| {
            crate::items::WorkItem::Jira(crate::jira::types::Issue {
                id: "1".into(),
                key: format!("{key}-1"),
                fields: crate::jira::types::IssueFields {
                    summary: String::new(),
                    status: crate::jira::types::StatusField {
                        id: "1".into(),
                        name: "Open".into(),
                    },
                    priority: None,
                    assignee: None,
                    reporter: None,
                    issuetype: IssueTypeField {
                        id: "1".into(),
                        name: "Task".into(),
                        subtask: false,
                    },
                    project: ProjectField {
                        id: "1".into(),
                        key: key.into(),
                        name: key.into(),
                    },
                    description: None,
                    comment: None,
                    attachment: None,
                    extra: std::collections::HashMap::new(),
                },
                source_id: None,
                subsource_idx: 0,
                partial: false,
                changelog: None,
            })
        };
        let issues = vec![mk("AAA"), mk("BBB"), mk("AAA")];
        let projects = distinct_projects(&issues);
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].key, "AAA");
        assert_eq!(projects[1].key, "BBB");
    }
}
