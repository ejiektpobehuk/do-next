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
use crate::tui::app::{
    ActionState, AppState, AttachmentFetchRequest, cache_path_for, compute_completions_for,
    update_state,
};
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
    let mut input_task = spawn_input_task(tx.clone());

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

    // Initialize image picker (best-effort; terminal must be in raw mode for query)
    app.image_picker = ratatui_image::picker::Picker::from_query_stdio().ok();

    let hidden_file = hidden_path(project_override)?;
    let mut hidden = HiddenState::load(&hidden_file)?;

    let mut list_state = ListState::default();

    // Main event loop
    loop {
        let Some(event) = rx.recv().await else { break };

        update_state(&mut app, event);

        maybe_spawn_field_names_fetch(&mut app, &client, &tx);

        handle_pending_comment(terminal, &mut app, &client, &tx, &mut rx, &mut input_task);
        handle_pending_field_edit(terminal, &mut app, &mut rx, &mut input_task, &tx);
        handle_pending_comment_edit(terminal, &mut app, &mut rx, &mut input_task, &tx);

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
        app.detail_focus_offsets = std::mem::take(&mut render_out.detail_focus_offsets);
        app.last_detail_viewport_h = render_out.detail_viewport_h;
        app.last_detail_content_h = render_out.detail_content_h;
        app.overlay_content_h = render_out.overlay_content_h;
        app.overlay_viewport_h = render_out.overlay_viewport_h;
        app.overlay_comment_offsets = std::mem::take(&mut render_out.overlay_comment_offsets);
    }

    tick_handle.abort();
    Ok(())
}

fn handle_pending_comment(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut AppState,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    input_task: &mut tokio::task::JoinHandle<()>,
) {
    let ActionState::PendingComment { ref issue_key } = app.action_state else {
        return;
    };
    let key = issue_key.clone();
    app.action_state = ActionState::None;
    input_task.abort();
    let editor_result = open_editor_for_comment(terminal);
    *input_task = spawn_input_task(tx.clone());
    drain_input_events(rx);
    match editor_result {
        Ok(Some(body)) => {
            app.action_state = ActionState::AwaitingAction {
                description: "Posting comment…".into(),
            };
            let client2 = client.clone();
            let tx2 = tx.clone();
            tokio::spawn(async move {
                match client2.post_comment(&key, &body).await {
                    Ok(new_comment) => {
                        let _ = tx2.send(AppEvent::ActionDone(ActionResult::CommentPosted {
                            issue_key: key,
                            new_comment,
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
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    input_task: &mut tokio::task::JoinHandle<()>,
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
    input_task.abort();
    let editor_result = open_editor_with_content(terminal, &current_value);
    *input_task = spawn_input_task(tx.clone());
    drain_input_events(rx);
    match editor_result {
        Ok(Some(new_text)) => {
            if new_text == current_value.trim() {
                // No change — skip Jira update
            } else {
                let new_value = shape_field_value(&new_text, &original_json);
                app.action_state = ActionState::ConfirmingFieldEdit {
                    issue_key: key,
                    field_id,
                    old_text: current_value,
                    new_text,
                    new_value,
                    tab: 0,
                };
            }
        }
        Ok(None) => {} // cancelled
        Err(e) => {
            app.action_state = ActionState::Error(std::sync::Arc::new(e));
        }
    }
}

fn spawn_input_task(tx: UnboundedSender<AppEvent>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut stream = EventStream::new();
        while let Some(Ok(event)) = stream.next().await {
            if tx.send(AppEvent::Input(event)).is_err() {
                break;
            }
        }
    })
}

/// Discard any input events queued while the editor was blocking the event loop.
fn drain_input_events(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) {
    while let Ok(AppEvent::Input(_)) = rx.try_recv() {}
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
    dispatch_background_tasks(app, client, tx);

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
            dispatch_pending_hide(app, issue_key, tx, hidden, hidden_file)?;
        }
        ActionState::PendingAssign { issue_key } => {
            dispatch_pending_assign(app, issue_key, client, tx);
        }
        ActionState::PendingMove { issue_key } => {
            dispatch_pending_move(app, issue_key, client, tx);
        }
        ActionState::LoadingFieldOptions {
            issue_key,
            field_id,
            label,
            original_json,
            description,
            multi,
        } => {
            dispatch_load_field_options(
                app,
                FieldOptionsRequest {
                    issue_key,
                    field_id,
                    label,
                    original_json,
                    description,
                    multi,
                },
                client,
                tx,
            );
        }
        ActionState::CommittingFieldEdit {
            issue_key,
            field_id,
            new_value,
        } => {
            dispatch_committing_field_edit(app, issue_key, field_id, new_value, client, tx);
        }
        ActionState::CommittingCommentEdit {
            issue_key,
            comment_id,
            new_body,
        } => {
            dispatch_committing_comment_edit(app, issue_key, comment_id, new_body, client, tx);
        }
        ActionState::DeletingComment {
            issue_key,
            comment_id,
        } => {
            dispatch_deleting_comment(app, issue_key, comment_id, client, tx);
        }
        ActionState::DeletingAttachment {
            issue_key,
            attachment_id,
        } => {
            dispatch_deleting_attachment(app, issue_key, attachment_id, client, tx);
        }
        ActionState::OpeningAttachment {
            attachment_id,
            content_url,
            filename,
            issue_key,
        } => {
            dispatch_opening_attachment(
                app,
                attachment_id,
                content_url,
                filename,
                issue_key,
                client,
                tx,
            );
        }
        ActionState::PendingAttachmentUpload {
            issue_key,
            file_path,
        } => {
            dispatch_pending_attachment_upload(app, issue_key, file_path, client, tx);
        }
        _ => {}
    }
    Ok(())
}

fn dispatch_background_tasks(
    app: &mut AppState,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    // Silent background attachment fetch (not ActionState-driven)
    if let Some(req) = app.pending_attachment_fetch.take() {
        spawn_cache_attachment(req, false, client.clone(), tx.clone());
    }

    // Debounced path completion fetch
    spawn_debounced_completions(app, tx);
}

fn spawn_debounced_completions(app: &mut AppState, tx: &UnboundedSender<AppEvent>) {
    if let Some(g) = app.pending_completion_fetch.take()
        && let ActionState::TypingAttachmentPath { ref path, .. } = app.action_state
    {
        let path = path.clone();
        let tx2 = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let completions = compute_completions_for(&path);
            let _ = tx2.send(AppEvent::PathCompletions {
                generation: g,
                completions,
            });
        });
    }
}

fn dispatch_pending_hide(
    app: &mut AppState,
    issue_key: String,
    tx: &UnboundedSender<AppEvent>,
    hidden: &mut HiddenState,
    hidden_file: &std::path::PathBuf,
) -> Result<()> {
    app.action_state = ActionState::AwaitingAction {
        description: "Hiding…".into(),
    };
    let duration = app.config.hide_for_a_day.duration_hours();
    hidden.hide_for(&issue_key, duration);
    hidden.save(hidden_file)?;
    let _ = tx.send(AppEvent::ActionDone(ActionResult::Hidden { issue_key }));
    Ok(())
}

fn dispatch_pending_assign(
    app: &mut AppState,
    issue_key: String,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    app.action_state = ActionState::AwaitingAction {
        description: "Assigning…".into(),
    };
    let username = app
        .current_user
        .clone()
        .unwrap_or_else(|| "currentUser()".into());
    spawn_assign(issue_key, username, client.clone(), tx.clone());
}

fn dispatch_pending_move(
    app: &mut AppState,
    issue_key: String,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    app.action_state = ActionState::AwaitingAction {
        description: "Moving…".into(),
    };
    let target = app.config.jira.default_project.clone();
    spawn_move(issue_key, target, client.clone(), tx.clone());
}

fn dispatch_pending_attachment_upload(
    app: &mut AppState,
    issue_key: String,
    file_path: String,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    app.action_state = ActionState::AwaitingAction {
        description: "Uploading…".into(),
    };
    let client2 = client.clone();
    let tx2 = tx.clone();
    tokio::spawn(async move {
        let path = std::path::PathBuf::from(&file_path);
        match client2.upload_attachment(&issue_key, &path).await {
            Ok(mut attachments) if !attachments.is_empty() => {
                let _ = tx2.send(AppEvent::ActionDone(ActionResult::AttachmentUploaded {
                    issue_key,
                    new_attachment: attachments.remove(0),
                }));
            }
            Ok(_) => {
                let _ = tx2.send(AppEvent::ActionDone(ActionResult::Error(anyhow::anyhow!(
                    "Upload succeeded but Jira returned no attachment data"
                ))));
            }
            Err(e) => {
                let _ = tx2.send(AppEvent::ActionDone(ActionResult::Error(e)));
            }
        }
    });
}

fn dispatch_opening_attachment(
    app: &mut AppState,
    attachment_id: String,
    content_url: String,
    filename: String,
    issue_key: String,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    app.action_state = ActionState::AwaitingAction {
        description: "Fetching attachment…".into(),
    };
    let req = AttachmentFetchRequest {
        attachment_id,
        content_url,
        filename,
        issue_key,
    };
    spawn_cache_attachment(req, true, client.clone(), tx.clone());
}

fn dispatch_committing_field_edit(
    app: &mut AppState,
    issue_key: String,
    field_id: String,
    new_value: serde_json::Value,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    app.action_state = ActionState::AwaitingAction {
        description: "Updating field…".into(),
    };
    spawn_commit_field_edit(issue_key, field_id, new_value, client.clone(), tx.clone());
}

fn dispatch_committing_comment_edit(
    app: &mut AppState,
    issue_key: String,
    comment_id: String,
    new_body: String,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    app.action_state = ActionState::AwaitingAction {
        description: "Updating comment…".into(),
    };
    spawn_commit_comment_edit(issue_key, comment_id, new_body, client.clone(), tx.clone());
}

fn dispatch_deleting_comment(
    app: &mut AppState,
    issue_key: String,
    comment_id: String,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    app.action_state = ActionState::AwaitingAction {
        description: "Deleting comment…".into(),
    };
    spawn_delete_comment(issue_key, comment_id, client.clone(), tx.clone());
}

fn spawn_cache_attachment(
    req: AttachmentFetchRequest,
    open_after: bool,
    client: JiraClient,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let cache_path = cache_path_for(&req.issue_key, &req.attachment_id, &req.filename);
        if !cache_path.exists() {
            if let Some(parent) = cache_path.parent()
                && let Err(e) = tokio::fs::create_dir_all(parent).await
            {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::Error(e.into())));
                return;
            }
            match client.download_attachment(&req.content_url).await {
                Ok(bytes) => {
                    if let Err(e) = tokio::fs::write(&cache_path, &bytes).await {
                        let _ = tx.send(AppEvent::ActionDone(ActionResult::Error(e.into())));
                        return;
                    }
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::ActionDone(ActionResult::Error(e)));
                    return;
                }
            }
        }
        let _ = tx.send(AppEvent::ActionDone(ActionResult::AttachmentCached {
            attachment_id: req.attachment_id,
            cache_path,
            open_after,
        }));
    });
}

fn dispatch_load_field_options(
    app: &mut AppState,
    req: FieldOptionsRequest,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    app.action_state = ActionState::AwaitingAction {
        description: "Fetching options…".into(),
    };
    spawn_load_field_options(req, client.clone(), tx.clone());
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

fn handle_pending_comment_edit(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut AppState,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    input_task: &mut tokio::task::JoinHandle<()>,
    tx: &UnboundedSender<AppEvent>,
) {
    let ActionState::PendingCommentEdit {
        ref issue_key,
        ref comment_id,
        ref original_body,
    } = app.action_state
    else {
        return;
    };
    let (key, cid, original) = (issue_key.clone(), comment_id.clone(), original_body.clone());
    app.action_state = ActionState::None;
    input_task.abort();
    let editor_result = open_editor_with_content(terminal, &original);
    *input_task = spawn_input_task(tx.clone());
    drain_input_events(rx);
    match editor_result {
        Ok(Some(new_text)) => {
            if new_text == original.trim() {
                // No change
            } else {
                app.action_state = ActionState::ConfirmingCommentEdit {
                    issue_key: key,
                    comment_id: cid,
                    old_text: original,
                    new_text,
                    tab: 0,
                };
            }
        }
        Ok(None) => {}
        Err(e) => {
            app.action_state = ActionState::Error(std::sync::Arc::new(e));
        }
    }
}

fn spawn_commit_comment_edit(
    issue_key: String,
    comment_id: String,
    new_body: String,
    client: JiraClient,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        match client
            .update_comment(&issue_key, &comment_id, &new_body)
            .await
        {
            Ok(updated) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::CommentEdited {
                    issue_key,
                    updated_comment: updated,
                }));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::Error(e)));
            }
        }
    });
}

fn spawn_delete_comment(
    issue_key: String,
    comment_id: String,
    client: JiraClient,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        match client.delete_comment(&issue_key, &comment_id).await {
            Ok(()) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::CommentDeleted {
                    issue_key,
                    comment_id,
                }));
            }
            Err(e) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::Error(e)));
            }
        }
    });
}

fn dispatch_deleting_attachment(
    app: &mut AppState,
    issue_key: String,
    attachment_id: String,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    app.action_state = ActionState::AwaitingAction {
        description: "Deleting attachment…".into(),
    };
    spawn_delete_attachment(issue_key, attachment_id, client.clone(), tx.clone());
}

fn spawn_delete_attachment(
    issue_key: String,
    attachment_id: String,
    client: JiraClient,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        match client.delete_attachment(&attachment_id).await {
            Ok(()) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::AttachmentDeleted {
                    issue_key,
                    attachment_id,
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

/// Spawn a fetch for field display names if needed (for both default and custom views).
fn maybe_spawn_field_names_fetch(
    app: &mut AppState,
    client: &JiraClient,
    tx: &UnboundedSender<AppEvent>,
) {
    use crate::tui::app::ViewMode;
    if !matches!(app.view_mode, ViewMode::Default | ViewMode::Custom(_)) {
        return;
    }
    if app.field_names_loading || !app.field_names.is_empty() {
        return;
    }

    if let ViewMode::Custom(id) = &app.view_mode {
        let Some(issue) = app.selected_issue() else {
            return;
        };
        let Some(cfg) = app.config.views.get(id.as_str()) else {
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
        let issue_key = issue.key.clone();
        app.field_names_loading = true;
        spawn_load_field_names_editmeta(issue_key, field_ids, client.clone(), tx.clone());
    } else {
        // Default view: use the global field registry to get names for all fields.
        // This covers readonly fields that don't appear in editmeta.
        app.field_names_loading = true;
        spawn_load_all_field_names(client.clone(), tx.clone());
    }
}

/// Fetch field names via editmeta (issue-specific; returns names + schema types for editable fields).
/// Used by custom views to also obtain schema types for the datetime picker.
fn spawn_load_field_names_editmeta(
    issue_key: String,
    field_ids: Vec<String>,
    client: JiraClient,
    tx: UnboundedSender<AppEvent>,
) {
    tokio::spawn(async move {
        let ids_ref: Vec<&str> = field_ids.iter().map(String::as_str).collect();
        match client.get_field_labels(&issue_key, &ids_ref).await {
            Ok((names, schemas)) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::FieldNamesLoaded {
                    names,
                    schemas,
                }));
            }
            Err(_) => {
                // Best-effort: silently ignore failures, field IDs will be shown as fallback
                let _ = tx.send(AppEvent::ActionDone(ActionResult::FieldNamesLoaded {
                    names: std::collections::HashMap::new(),
                    schemas: std::collections::HashMap::new(),
                }));
            }
        }
    });
}

/// Fetch field names from the global Jira field registry (`GET /rest/api/2/field`).
/// Returns names for ALL fields including readonly ones. Used by the default view.
fn spawn_load_all_field_names(client: JiraClient, tx: UnboundedSender<AppEvent>) {
    tokio::spawn(async move {
        match client.get_all_fields().await {
            Ok(fields) => {
                let names = fields.into_iter().map(|f| (f.id, f.name)).collect();
                let _ = tx.send(AppEvent::ActionDone(ActionResult::FieldNamesLoaded {
                    names,
                    schemas: std::collections::HashMap::new(),
                }));
            }
            Err(_) => {
                let _ = tx.send(AppEvent::ActionDone(ActionResult::FieldNamesLoaded {
                    names: std::collections::HashMap::new(),
                    schemas: std::collections::HashMap::new(),
                }));
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
