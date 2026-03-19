pub mod app;
pub mod detail;
pub mod hint_bar;
pub mod list;
pub mod onboarding;
pub mod overlays;
pub mod render;
pub mod views;

use anyhow::Result;
use crossterm::{
    event::EventStream,
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{Terminal, backend::CrosstermBackend, widgets::ListState};
use std::io;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::time::{Duration, interval};

use crate::config::hidden::{HiddenState, hidden_path};
use crate::config::types::Config;
use crate::events::{ActionResult, AppEvent};
use crate::jira::JiraClient;
use crate::sources::spawn_fetches;
use crate::tui::app::{ActionState, AppState, update_state};
use crate::tui::render::{RenderOut, render};

/// Entry point for the interactive TUI.
pub async fn run(config: Config, client: JiraClient, project_override: bool) -> Result<()> {
    // Terminal setup
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_inner(&mut terminal, config, client, project_override).await;

    // Cleanup
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn run_inner(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: Config,
    client: JiraClient,
    project_override: bool,
) -> Result<()> {
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    let mut app = AppState::new(config);

    // Fetch current user (best-effort; subsource sorting depends on it)
    {
        let client2 = client.clone();
        let tx2 = tx.clone();
        tokio::spawn(async move {
            if let Ok(user) = client2.current_user().await {
                let _ = tx2.send(AppEvent::CurrentUserResolved(user));
            }
        });
    }

    // Spawn fetch tasks for all sources
    spawn_fetches(&client, &app.config, &tx);

    // Mark all sources as Loading (they were Pending)
    for state in app.sources.values_mut() {
        *state = crate::tui::app::SourceState::Loading;
    }

    // Spawn input task
    spawn_input_task(tx.clone());

    // Spawn tick task (active while loading)
    let tick_tx = tx.clone();
    let tick_handle = tokio::spawn(async move {
        let mut ticker = interval(Duration::from_millis(100));
        loop {
            ticker.tick().await;
            if tick_tx.send(AppEvent::Tick).is_err() {
                break;
            }
        }
    });

    let hidden_file = hidden_path(project_override)?;
    let mut hidden = HiddenState::load(&hidden_file)?;

    let mut list_state = ListState::default();

    // Main event loop
    loop {
        let Some(event) = rx.recv().await else { break };

        update_state(&mut app, event);

        maybe_spawn_field_names_fetch(&mut app, &client, &tx);

        handle_pending_comment(terminal, &mut app, &client, &tx);
        handle_pending_field_edit(terminal, &mut app, &client, &tx);

        // Dispatch any pending action signals (transition fetch, hide, assign, move)
        dispatch_action(
            &mut app,
            &client,
            &tx,
            &mut hidden,
            &hidden_file,
            project_override,
        )?;

        if app.should_quit {
            break;
        }

        // Stop tick task once all sources are done
        if app.all_sources_terminal() {
            tick_handle.abort();
        }

        let mut render_out = RenderOut::default();
        terminal.draw(|f| render(f, &app, &mut list_state, &mut render_out))?;
        app.postmortem_field_offsets = std::mem::take(&mut render_out.postmortem_field_offsets);
        app.last_detail_viewport_h = render_out.detail_viewport_h;
        app.last_detail_content_h = render_out.detail_content_h;
    }

    tick_handle.abort();
    Ok(())
}

fn handle_pending_comment(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut AppState,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    let ActionState::PendingComment { ref issue_key } = app.action_state else {
        return;
    };
    let key = issue_key.clone();
    app.action_state = ActionState::None;
    match open_editor_for_comment(terminal) {
        Ok(Some(body)) => {
            app.action_state = ActionState::AwaitingAction {
                description: "Posting comment…".into(),
            };
            let client2 = client.clone();
            let tx2 = tx.clone();
            tokio::spawn(async move {
                match client2.post_comment(&key, &body).await {
                    Ok(_) => {
                        let _ = tx2.send(AppEvent::ActionDone(ActionResult::CommentPosted {
                            issue_key: key,
                        }));
                    }
                    Err(e) => {
                        let _ = tx2.send(AppEvent::ActionDone(ActionResult::Error(e)));
                    }
                }
            });
        }
        Ok(None) => {} // empty/cancelled — state already cleared
        Err(e) => {
            app.action_state = ActionState::Error(std::sync::Arc::new(e));
        }
    }
}

fn handle_pending_field_edit(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut AppState,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    let ActionState::PendingFieldEdit {
        ref issue_key,
        ref field_id,
        ref current_value,
        ref original_json,
    } = app.action_state
    else {
        return;
    };
    let (key, field_id, current_value, original_json) = (
        issue_key.clone(),
        field_id.clone(),
        current_value.clone(),
        original_json.clone(),
    );
    app.action_state = ActionState::None;
    match open_editor_with_content(terminal, &current_value) {
        Ok(Some(new_text)) => {
            let new_value = shape_field_value(&new_text, &original_json);
            app.action_state = ActionState::AwaitingAction {
                description: "Updating field…".into(),
            };
            let client2 = client.clone();
            let tx2 = tx.clone();
            tokio::spawn(async move {
                match client2
                    .update_field(&key, &field_id, new_value.clone())
                    .await
                {
                    Ok(()) => {
                        let _ = tx2.send(AppEvent::ActionDone(ActionResult::FieldUpdated {
                            issue_key: key,
                            field_id,
                            new_value,
                        }));
                    }
                    Err(e) => {
                        let _ = tx2.send(AppEvent::ActionDone(ActionResult::Error(e)));
                    }
                }
            });
        }
        Ok(None) => {} // cancelled
        Err(e) => {
            app.action_state = ActionState::Error(std::sync::Arc::new(e));
        }
    }
}

fn spawn_input_task(tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        let mut stream = EventStream::new();
        while let Some(Ok(event)) = stream.next().await {
            if tx.send(AppEvent::Input(event)).is_err() {
                break;
            }
        }
    });
}

/// Dispatch any pending typed action states.
/// Transitions the state to `AwaitingAction` immediately to prevent re-dispatch.
fn dispatch_action(
    app: &mut AppState,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
    hidden: &mut HiddenState,
    hidden_file: &std::path::PathBuf,
    _project_override: bool,
) -> Result<()> {
    match app.action_state.clone() {
        ActionState::LoadingTransitions { issue_key } => {
            app.action_state = ActionState::AwaitingAction {
                description: "Fetching transitions…".into(),
            };
            spawn_load_transitions(issue_key, client.clone(), tx.clone());
        }
        ActionState::PendingTransition {
            issue_key,
            transition_id,
        } => {
            app.action_state = ActionState::AwaitingAction {
                description: "Applying transition…".into(),
            };
            spawn_transition(issue_key, transition_id, client.clone(), tx.clone());
        }
        ActionState::PendingHide { issue_key } => {
            app.action_state = ActionState::AwaitingAction {
                description: "Hiding…".into(),
            };
            let duration = app.config.hide_for_a_day.duration_hours();
            hidden.hide_for(&issue_key, duration);
            hidden.save(hidden_file)?;
            let _ = tx.send(AppEvent::ActionDone(ActionResult::Hidden { issue_key }));
        }
        ActionState::PendingAssign { issue_key } => {
            app.action_state = ActionState::AwaitingAction {
                description: "Assigning…".into(),
            };
            let username = app
                .current_user
                .clone()
                .unwrap_or_else(|| "currentUser()".into());
            spawn_assign(issue_key, username, client.clone(), tx.clone());
        }
        ActionState::PendingMove { issue_key } => {
            app.action_state = ActionState::AwaitingAction {
                description: "Moving…".into(),
            };
            let target = app.config.jira.default_project.clone();
            spawn_move(issue_key, target, client.clone(), tx.clone());
        }
        ActionState::LoadingFieldOptions {
            issue_key,
            field_id,
            label,
            original_json,
            description,
            multi,
        } => {
            app.action_state = ActionState::AwaitingAction {
                description: "Fetching options…".into(),
            };
            spawn_load_field_options(
                FieldOptionsRequest {
                    issue_key,
                    field_id,
                    label,
                    original_json,
                    description,
                    multi,
                },
                client.clone(),
                tx.clone(),
            );
        }
        ActionState::CommittingFieldEdit {
            issue_key,
            field_id,
            new_value,
        } => {
            app.action_state = ActionState::AwaitingAction {
                description: "Updating field…".into(),
            };
            spawn_commit_field_edit(issue_key, field_id, new_value, client.clone(), tx.clone());
        }
        _ => {}
    }
    Ok(())
}

struct FieldOptionsRequest {
    issue_key: String,
    field_id: String,
    label: String,
    original_json: serde_json::Value,
    description: Option<String>,
    multi: bool,
}

fn spawn_load_transitions(issue_key: String, client: JiraClient, tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        match client.get_transitions(&issue_key).await {
            Ok(transitions) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::TransitionsLoaded {
                    issue_key,
                    transitions,
                }));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::Error(e)));
            }
        }
    });
}

fn spawn_assign(
    issue_key: String,
    username: String,
    client: JiraClient,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        match client.set_assignee(&issue_key, &username).await {
            Ok(()) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::AssignedToMe {
                    issue_key,
                }));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::Error(e)));
            }
        }
    });
}

fn spawn_move(
    issue_key: String,
    target: String,
    client: JiraClient,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        match client.move_issue(&issue_key, &target).await {
            Ok(()) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::MovedToProject {
                    issue_key,
                    project: target,
                }));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::Error(e)));
            }
        }
    });
}

fn spawn_load_field_options(
    req: FieldOptionsRequest,
    client: JiraClient,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        match client
            .get_field_options(&req.issue_key, &req.field_id)
            .await
        {
            Ok(options) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::FieldOptionsLoaded {
                    issue_key: req.issue_key,
                    field_id: req.field_id,
                    label: req.label,
                    original_json: req.original_json,
                    options,
                    description: req.description,
                    multi: req.multi,
                }));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::Error(e)));
            }
        }
    });
}

fn spawn_commit_field_edit(
    issue_key: String,
    field_id: String,
    new_value: serde_json::Value,
    client: JiraClient,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        match client
            .update_field(&issue_key, &field_id, new_value.clone())
            .await
        {
            Ok(()) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::FieldUpdated {
                    issue_key,
                    field_id,
                    new_value,
                }));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::Error(e)));
            }
        }
    });
}

fn spawn_transition(key: String, tid: String, client: JiraClient, tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        // Fetch transitions to get the target status name for the result
        let name = client
            .get_transitions(&key)
            .await
            .ok()
            .and_then(|ts| ts.into_iter().find(|t| t.id == tid).map(|t| t.to.name))
            .unwrap_or_default();
        match client.post_transition(&key, &tid).await {
            Ok(()) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::TransitionApplied {
                    issue_key: key,
                    new_status: name,
                }));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::Error(e)));
            }
        }
    });
}

/// Open $EDITOR with optional initial content, then return the edited text.
/// This suspends the TUI.
fn open_editor_with_content(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    initial_content: &str,
) -> Result<Option<String>> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let tmp = tempfile_path();
    if !initial_content.is_empty() {
        std::fs::write(&tmp, initial_content)?;
    }

    let status = std::process::Command::new(&editor).arg(&tmp).status();

    enable_raw_mode()?;
    execute!(terminal.backend_mut(), EnterAlternateScreen)?;
    terminal.clear()?;

    match status {
        Ok(s) if s.success() => {
            let content = std::fs::read_to_string(&tmp).unwrap_or_default();
            let _ = std::fs::remove_file(&tmp);
            let trimmed = content.trim().to_string();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed))
            }
        }
        _ => Ok(None),
    }
}

/// Open $EDITOR for the user to write a comment, then return the text.
/// This suspends the TUI.
#[allow(dead_code)]
pub fn open_editor_for_comment(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<Option<String>> {
    open_editor_with_content(terminal, "")
}

/// Spawn a fetch for postmortem field display names if needed.
fn maybe_spawn_field_names_fetch(
    app: &mut AppState,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    if app.view_mode != crate::tui::app::ViewMode::Postmortem {
        return;
    }
    if app.postmortem_field_names_loading || !app.postmortem_field_names.is_empty() {
        return;
    }
    let Some(cfg) = app.config.view_modes.postmortem.as_ref() else {
        return;
    };
    // Only fetch names for fields that don't have a name override
    let field_ids: Vec<String> = cfg
        .sections
        .iter()
        .flat_map(|s| s.fields.iter())
        .filter(|f| f.name.is_none())
        .map(|f| f.field_id.clone())
        .collect();
    if field_ids.is_empty() {
        return;
    }
    let Some(issue) = app.selected_issue() else {
        return;
    };
    let issue_key = issue.key.clone();
    app.postmortem_field_names_loading = true;
    spawn_load_postmortem_field_names(issue_key, field_ids, client.clone(), tx.clone());
}

fn spawn_load_postmortem_field_names(
    issue_key: String,
    field_ids: Vec<String>,
    client: JiraClient,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let ids_ref: Vec<&str> = field_ids.iter().map(String::as_str).collect();
        match client.get_field_labels(&issue_key, &ids_ref).await {
            Ok((names, schemas)) => {
                let _ = tx.send(AppEvent::ActionDone(
                    ActionResult::PostmortemFieldNamesLoaded { names, schemas },
                ));
            }
            Err(_) => {
                // Best-effort: silently ignore failures, field IDs will be shown as fallback
                let _ = tx.send(AppEvent::ActionDone(
                    ActionResult::PostmortemFieldNamesLoaded {
                        names: std::collections::HashMap::new(),
                        schemas: std::collections::HashMap::new(),
                    },
                ));
            }
        }
    });
}

/// Shape the user's edited text into the correct JSON value for a Jira field update.
/// Object fields with a "value" key are Jira select fields; all others are plain strings.
fn shape_field_value(user_text: &str, original: &serde_json::Value) -> serde_json::Value {
    if let serde_json::Value::Object(map) = original
        && map.contains_key("value")
    {
        return serde_json::json!({ "value": user_text });
    }
    serde_json::Value::String(user_text.to_string())
}

#[allow(dead_code)]
fn tempfile_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("do-next-comment-{}.txt", std::process::id()))
}
