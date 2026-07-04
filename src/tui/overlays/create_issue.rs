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
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use serde_json::{Value, json};

use crate::jira::types::{FieldSchema, Issue, IssueTypeField, ProjectField, ProjectInfo};
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
    Text { input: String, cursor: usize },
    Number { input: String, cursor: usize },
    Date { value: Option<String> },
    SingleOption(Option<usize>),
    MultiOption(HashSet<usize>),
    Unsupported,
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
#[derive(Debug, Clone)]
pub struct CreateForm {
    pub project: ProjectField,
    pub available_projects: Vec<ProjectField>,
    pub issuetype: Option<IssueTypeField>,
    pub issuetypes: CacheState<Vec<IssueTypeField>>,
    pub fields: Vec<FormField>,
    pub fields_state: CacheState<()>,
    /// Flat focus index: 0=project, 1=issue type, 2..=fields, last=Create button.
    pub focus: usize,
    pub mode: FormMode,
    pub picker: Option<CreatePicker>,
    /// `Some(selected)` while the discard-changes prompt is up (0=Discard, 1=Keep editing).
    pub discard_confirm: Option<usize>,
    /// Generation guard; bumped on any project/issue-type change so stale
    /// metadata responses are dropped.
    pub meta_token: u64,
    pub needs_issuetype_fetch: bool,
    pub needs_field_fetch: bool,
    pub needs_projects_fetch: bool,
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
            focus: 1, // start on Issue Type (project is pre-filled)
            mode: FormMode::Nav,
            picker: None,
            discard_confirm: None,
            meta_token: 1,
            needs_issuetype_fetch: true,
            needs_field_fetch: false,
            needs_projects_fetch: false,
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

/// Distinct projects across the loaded issues, deduped by key, order preserved.
pub fn distinct_projects(issues: &[Issue]) -> Vec<ProjectField> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for issue in issues {
        if seen.insert(issue.fields.project.key.clone()) {
            out.push(issue.fields.project.clone());
        }
    }
    out
}

/// Map a Jira field `schema` object to a widget. `has_options` is whether the
/// field carries `allowedValues` (decides how `user` fields are handled).
pub fn schema_to_widget(schema: &Value, has_options: bool) -> WidgetKind {
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
                WidgetKind::Text
            }
        }
        "array" => match items {
            Some("option" | "version" | "component" | "group") => WidgetKind::MultiSelect,
            Some("user") if has_options => WidgetKind::MultiSelect,
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
        WidgetKind::Unsupported => FieldValue::Unsupported,
    }
}

/// Parse the raw createmeta field descriptors into renderable form fields.
/// Required fields come first; `project` and `issuetype` are excluded (handled
/// by the top-level selectors).
pub fn parse_create_fields(values: &[Value]) -> Vec<FormField> {
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

        let widget = schema_to_widget(&schema, !options.is_empty());

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

/// The JSON value to emit for a field, or `None` if it should be omitted.
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

    for field in &form.fields {
        if field.required && field.widget == WidgetKind::Unsupported {
            return Err(format!(
                "Required field “{}” can't be set here — create in browser",
                field.label
            ));
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

    Ok(json!({ "fields": Value::Object(fields) }))
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
        ActionState::CreatingIssue(ref f) if f.discard_confirm.is_some()
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
            handle_edit_mode_key(form, code);
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
            form.discard_confirm = Some(1);
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
        WidgetKind::Unsupported => {}
    }
}

/// Edit-mode key routing. Only Text/Number/RichText rows should be focused
/// while `mode == Edit`. Esc/Tab return to `Nav`.
fn handle_edit_mode_key(form: &mut CreateForm, code: KeyCode) {
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

fn form_is_dirty(form: &CreateForm) -> bool {
    form.fields.iter().any(|f| match &f.value {
        FieldValue::Text { input, .. } | FieldValue::Number { input, .. } => !input.is_empty(),
        FieldValue::Date { value } => value.is_some(),
        FieldValue::SingleOption(opt) => opt.is_some(),
        FieldValue::MultiOption(set) => !set.is_empty(),
        FieldValue::Unsupported => false,
    })
}

fn handle_discard_confirm_input(app: &mut AppState, code: KeyCode) {
    let ActionState::CreatingIssue(ref mut form) = app.action_state else {
        return;
    };
    let Some(selected) = form.discard_confirm else {
        return;
    };
    match code {
        KeyCode::Esc | KeyCode::Char('q') => {
            form.discard_confirm = None;
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Char('h' | 'l') | KeyCode::Tab => {
            form.discard_confirm = Some(1 - selected);
        }
        KeyCode::Enter => {
            if selected == 0 {
                app.action_state = ActionState::None;
            } else {
                form.discard_confirm = None;
            }
        }
        _ => {}
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

#[allow(clippy::too_many_lines)]
fn handle_picker_input(app: &mut AppState, code: KeyCode, _modifiers: KeyModifiers) {
    let ActionState::CreatingIssue(ref mut form) = app.action_state else {
        return;
    };
    let Some(picker) = form.picker.take() else {
        return;
    };

    match picker {
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
fn fields_status_line(form: &CreateForm) -> Option<Line<'static>> {
    form.issuetype.as_ref()?;
    match &form.fields_state {
        CacheState::Loading | CacheState::Idle => Some(Line::from(Span::styled(
            "  (loading fields…)",
            Style::default()
                .fg(theme::MUTED)
                .add_modifier(Modifier::ITALIC),
        ))),
        CacheState::Failed(e) => Some(Line::from(Span::styled(
            format!("  (fields failed: {e} — reselect Type to retry)"),
            Style::default().fg(Color::Red),
        ))),
        CacheState::Loaded(()) => None,
    }
}

pub fn render_create_issue_overlay(f: &mut Frame, app: &AppState) {
    let ActionState::CreatingIssue(ref form) = app.action_state else {
        return;
    };

    let area = crate::tui::render::centered_rect(70, 80, f.area());
    f.render_widget(Clear, area);

    let dim = form.picker.is_some();
    let title_style = if dim {
        Style::default()
            .fg(theme::MUTED)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if dim { theme::MUTED } else { Color::Reset }))
        .title(Span::styled(" New Issue ", title_style));
    if !dim {
        block = block.title_bottom(hints_line(form.mode));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(field_row(
        "Project",
        false,
        &format!("{}  {}  ▾", form.project.key, form.project.name),
        form.focus == 0 && !dim,
        false,
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
    ));

    if let Some(status) = fields_status_line(form) {
        lines.push(status);
    }

    for (i, field) in form.fields.iter().enumerate() {
        let focused = form.focus == 2 + i && !dim;
        lines.push(field_row(
            &field.label,
            field.required,
            &field_value_display(field),
            focused,
            field.widget == WidgetKind::Unsupported,
        ));
    }

    // Spacer + error + Create button.
    lines.push(Line::from(""));
    if let Some(err) = &form.error {
        lines.push(Line::from(Span::styled(
            format!("  {err}"),
            Style::default().fg(Color::Red),
        )));
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

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);

    if let Some(picker) = &form.picker {
        render_picker(f, form, picker, app.project_cache.is_pending());
    }

    if let Some(selected) = form.discard_confirm {
        crate::tui::overlays::delete_confirm::render_delete_confirm_overlay(
            f,
            " Discard new issue? ",
            selected,
            ("Discard", "Keep editing"),
            "You have unsaved fields. Discard them?",
        );
    }
}

fn field_row(
    label: &str,
    required: bool,
    value: &str,
    focused: bool,
    unsupported: bool,
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
        Span::styled(format!("{label_owned:<16}"), label_style),
        Span::styled(value.to_string(), value_style),
    ])
}

fn field_value_display(field: &FormField) -> String {
    match (&field.widget, &field.value) {
        (WidgetKind::Text | WidgetKind::RichText, FieldValue::Text { input, .. })
        | (WidgetKind::Number, FieldValue::Number { input, .. }) => {
            if input.is_empty() {
                "—".to_string()
            } else {
                input.replace('\n', "⏎")
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
        (WidgetKind::Unsupported, _) => "(set in browser)".to_string(),
        _ => String::new(),
    }
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
                projects_loading,
                Some(project_picker_hints_line(*searching)),
            );
        }
        CreatePicker::IssueType { cursor } => {
            let items: Vec<String> = form
                .issuetypes
                .loaded()
                .map(|v| v.iter().map(|it| it.name.clone()).collect())
                .unwrap_or_default();
            render_list(f, " Issue Type ", &items, *cursor, None, false, None);
        }
        CreatePicker::Select { field_idx, cursor } => {
            let (title, items) = field_option_items(form, *field_idx);
            render_list(f, &title, &items, *cursor, None, false, None);
        }
        CreatePicker::MultiSelect { field_idx, cursor } => {
            let (title, items) = field_option_items(form, *field_idx);
            let marks = match form.fields.get(*field_idx).map(|f| &f.value) {
                Some(FieldValue::MultiOption(set)) => Some(set.clone()),
                _ => None,
            };
            render_list(f, &title, &items, *cursor, marks.as_ref(), false, None);
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

fn render_list(
    f: &mut Frame,
    title: &str,
    items: &[String],
    cursor: usize,
    marks: Option<&HashSet<usize>>,
    loading_more: bool,
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

    let (list_area, hint_area) = if loading_more {
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

    if let Some(area) = hint_area {
        f.render_widget(
            Paragraph::new(Span::styled(
                "  (loading more…)",
                Style::default()
                    .fg(theme::MUTED)
                    .add_modifier(Modifier::ITALIC),
            )),
            area,
        );
    }
}

fn hints_line(mode: FormMode) -> Line<'static> {
    let spans = match mode {
        FormMode::Nav => vec![
            Span::raw("┤ "),
            Span::styled("j/k", Style::default().fg(Color::Blue)),
            Span::raw(" move  "),
            Span::styled("↵", Style::default().fg(Color::Blue)),
            Span::raw(" edit/pick  "),
            Span::styled("Alt+↵", Style::default().fg(Color::Green)),
            Span::raw(" create  "),
            Span::styled("q", Style::default().fg(Color::Magenta)),
            Span::raw(" cancel ├──"),
        ],
        FormMode::Edit => vec![
            Span::raw("┤ "),
            Span::styled("↵", Style::default().fg(Color::Blue)),
            Span::raw(" next  "),
            Span::styled("Tab", Style::default().fg(Color::Blue)),
            Span::raw(" move  "),
            Span::styled("Alt+↵", Style::default().fg(Color::Green)),
            Span::raw(" create  "),
            Span::styled("Esc", Style::default().fg(Color::Magenta)),
            Span::raw(" done ├──"),
        ],
    };
    Line::from(spans).alignment(Alignment::Right)
}

fn picker_hints_line(multi: bool) -> Line<'static> {
    let mut spans = vec![
        Span::raw("┤ "),
        Span::styled("j/k", Style::default().fg(Color::Blue)),
        Span::raw(" nav  "),
    ];
    if multi {
        spans.push(Span::styled("Space", Style::default().fg(Color::Blue)));
        spans.push(Span::raw(" toggle  "));
    }
    spans.push(Span::styled("↵", Style::default().fg(Color::Green)));
    spans.push(Span::raw(if multi { " done  " } else { " select  " }));
    spans.push(Span::styled("q", Style::default().fg(Color::Magenta)));
    spans.push(Span::raw(" back ├──"));
    Line::from(spans).alignment(Alignment::Right)
}

fn project_picker_hints_line(searching: bool) -> Line<'static> {
    let spans = if searching {
        vec![
            Span::raw("┤ "),
            Span::styled("type", Style::default().fg(Color::Blue)),
            Span::raw(" filter  "),
            Span::styled("Esc", Style::default().fg(Color::Magenta)),
            Span::raw(" to list ├──"),
        ]
    } else {
        vec![
            Span::raw("┤ "),
            Span::styled("j/k", Style::default().fg(Color::Blue)),
            Span::raw(" nav  "),
            Span::styled("/", Style::default().fg(Color::Blue)),
            Span::raw(" search  "),
            Span::styled("↵", Style::default().fg(Color::Green)),
            Span::raw(" select  "),
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
        let fields = parse_create_fields(&values);
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
        let fields = parse_create_fields(&values);
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].field_id, "summary");
    }

    #[test]
    fn schema_mapping_table() {
        assert_eq!(schema_to_widget(&schema("string"), false), WidgetKind::Text);
        assert_eq!(
            schema_to_widget(&json!({ "type": "string", "system": "description" }), false),
            WidgetKind::RichText
        );
        assert_eq!(
            schema_to_widget(&schema("number"), false),
            WidgetKind::Number
        );
        assert_eq!(schema_to_widget(&schema("date"), false), WidgetKind::Date);
        assert_eq!(
            schema_to_widget(&schema("datetime"), false),
            WidgetKind::DateTime
        );
        assert_eq!(
            schema_to_widget(&schema("option"), true),
            WidgetKind::Select
        );
        assert_eq!(
            schema_to_widget(&json!({ "type": "array", "items": "option" }), true),
            WidgetKind::MultiSelect
        );
        assert_eq!(schema_to_widget(&schema("user"), true), WidgetKind::Select);
        assert_eq!(schema_to_widget(&schema("user"), false), WidgetKind::Text);
        assert_eq!(
            schema_to_widget(&schema("timetracking"), false),
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
        let fields = parse_create_fields(&values);
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
        let mk = |key: &str| Issue {
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
        };
        let issues = vec![mk("AAA"), mk("BBB"), mk("AAA")];
        let projects = distinct_projects(&issues);
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].key, "AAA");
        assert_eq!(projects[1].key, "BBB");
    }
}
