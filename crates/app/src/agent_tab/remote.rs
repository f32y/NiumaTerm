//! Agent sessions across computers: offering this computer's agent tabs to
//! paired devices, and following a paired host's agent tab here.
//!
//! The host keeps running the session. A view on another computer holds a
//! replica controller that follows the host's published view and sends
//! commands back, so the pane renders and reacts the same way for both.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use gpui::prelude::*;
use gpui::{AnyElement, App, Context, Entity, Task, WeakEntity, Window};
use gpui_component::{ActiveTheme as _, h_flex};
use nmt_agent::chat::Item;
use nmt_agent::session::AgentKind;
use nmt_agent::session::branch::{BRANCH_METHOD, BranchStep, BranchUpdate, RewindAction};
use nmt_agent::session::capabilities::AgentCapabilities as _;
use nmt_agent::session::command::{AgentCommand, NEW_CONVERSATION_METHOD, PromptImage, run_remote};
use nmt_agent::session::controller::{SessionController, SessionEffect};
use nmt_agent::session::history::{HISTORY_METHOD, HistoryStep, list_scoped_sessions};
use nmt_agent::session::input::{QuestionCompletion, QuestionKey};
use nmt_agent::session::lifecycle::Status as SessionStatus;
use nmt_agent::session::restore::{ResumeStart, SettingsSeed};
use nmt_agent::session::view::{
    AgentView, IMAGE_METHOD, ImageData, ImageRef, ViewCursor, ViewOp, ViewPublisher, ViewSlot,
};
use nmt_agent::transcript::conversation::ConversationImage;
use nmt_config::profile::AgentProfile;
use nmt_platform::runtime;
use nmt_remote::connection::{AgentLink, AgentUpdate, RemoteHost, Status};
use nmt_remote::sessions::AgentRequest;
use nmt_remote_core::rpc::EndReason;
use rust_i18n::t;
use serde_json::Value;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::sync::mpsc::{Sender, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tracing::warn;
use uuid::Uuid;

use crate::agent_tab::composer::attachments::scratch_dir;
use crate::agent_tab::composer::{branch_error_message, branch_failure_message};
use crate::agent_tab::execution::{AgentSession, SessionOwner};
use crate::agent_tab::view::recent_sessions::RecentSessionsMode;
use crate::agent_tab::{AgentPane, AgentPaneEvent};

/// How often attached views hear about changes. Streamed text changes far
/// more often; batching keeps a busy turn to a few messages a second, and a
/// command's own result goes out at once.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(100);

struct PagedView {
    id: String,
    publisher: ViewPublisher,
    updates: Sender<Value>,
}

/// Answer paired devices' requests for one host agent tab until the tab
/// closes or the registry lets go of it.
pub fn serve(
    session: WeakEntity<AgentSession>,
    mut requests: UnboundedReceiver<AgentRequest>,
    cx: &mut App,
) -> Task<()> {
    cx.spawn(async move |cx| {
        let mut publisher = ViewPublisher::default();
        let mut views: Vec<UnboundedSender<Value>> = Vec::new();
        let mut paged_views: Vec<PagedView> = Vec::new();

        loop {
            // With nobody watching there is nothing to publish, so the task
            // sleeps until a device asks for something.
            let first = if views.is_empty() && paged_views.is_empty() {
                match requests.recv().await {
                    Some(request) => Some(request),
                    None => break,
                }
            } else {
                cx.background_executor().timer(PUBLISH_INTERVAL).await;

                match requests.try_recv() {
                    Ok(request) => Some(request),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => break,
                }
            };

            let served = session.update(cx, |session, cx| {
                if let Some(request) = first {
                    answer(
                        session,
                        request,
                        &mut publisher,
                        &mut views,
                        &mut paged_views,
                        cx,
                    );
                }

                while let Ok(request) = requests.try_recv() {
                    answer(
                        session,
                        request,
                        &mut publisher,
                        &mut views,
                        &mut paged_views,
                        cx,
                    );
                }

                publish(session, &mut publisher, &mut views);
                publish_paged(session, &mut paged_views);
            });

            if served.is_err() {
                break;
            }
        }
    })
}

fn answer(
    session: &mut AgentSession,
    request: AgentRequest,
    publisher: &mut ViewPublisher,
    views: &mut Vec<UnboundedSender<Value>>,
    paged_views: &mut Vec<PagedView>,
    cx: &mut Context<AgentSession>,
) {
    match request {
        AgentRequest::AttachPaged {
            view_id,
            updates,
            reply,
        } => {
            let mut publisher = ViewPublisher::default();

            let snapshot = publisher.snapshot_paged(&session.controller.borrow());

            let event = serde_json::json!({
                "view_id": view_id,
                "type": "snapshot",
                "value": snapshot,
            });

            if reply.send(event).is_ok() {
                paged_views.retain(|view| !view.updates.is_closed() && view.id != view_id);

                paged_views.push(PagedView {
                    id: view_id,
                    publisher,
                    updates,
                });
            }
        }
        AgentRequest::Page {
            view_id,
            cursor,
            reply,
        } => {
            let outcome = publish_earlier_page(session, paged_views, &view_id, cursor);

            let _ = reply.send(outcome);
        }
        AgentRequest::Attach { updates, reply } => {
            // Views already attached catch up first, so the snapshot and
            // the changes after it share one baseline.
            publish(session, publisher, views);

            let view = publisher.snapshot(&session.controller.borrow());

            match serde_json::to_value(view) {
                Ok(view) => {
                    if reply.send(view).is_ok() {
                        views.push(updates);
                    }
                }
                Err(error) => warn!(%error, "cannot encode an agent view"),
            }
        }
        AgentRequest::Call {
            method,
            params,
            reply,
        } if method == IMAGE_METHOD => {
            let _ = reply.send(image(session, params));
        }
        AgentRequest::Call {
            method,
            params,
            reply,
        } if method == HISTORY_METHOD => {
            let outcome = serde_json::from_value::<HistoryStep>(params)
                .map_err(|error| error.to_string())
                .and_then(|step| take_history_step(session, step, cx));

            session.publish(SessionEffect::Changed, cx);

            publish(session, publisher, views);

            let _ = reply.send(serde_json::to_value(outcome).map_err(|error| error.to_string()));
        }
        AgentRequest::Call {
            method,
            params,
            reply,
        } if method == BRANCH_METHOD => {
            let outcome = serde_json::from_value::<BranchStep>(params)
                .map_err(|error| error.to_string())
                .and_then(|step| take_branch_step(session, step, cx));

            // Every view shows the picker the step moved to, the host's own
            // pane included.
            session.publish(SessionEffect::Changed, cx);

            publish(session, publisher, views);

            // The refusal is sent as a value, so the view shows the host's
            // message instead of a transport error around it.
            let _ = reply.send(serde_json::to_value(outcome).map_err(|error| error.to_string()));
        }
        AgentRequest::Call { method, reply, .. } if method == NEW_CONVERSATION_METHOD => {
            let outcome = start_new_conversation(session, cx);

            publish(session, publisher, views);

            let _ = reply.send(serde_json::to_value(outcome).map_err(|error| error.to_string()));
        }
        AgentRequest::Call {
            method,
            params,
            reply,
        } => {
            let route = session.route.as_str().to_owned();

            let outcome = run_remote(
                &method,
                params,
                &mut session.controller.borrow_mut(),
                |images| stage_images(&route, images),
            )
            .map_err(|error| error.to_string());

            // The host's own pane shows what the command changed.
            session.publish(SessionEffect::Changed, cx);

            publish(session, publisher, views);

            let _ = reply.send(outcome);
        }
    }
}

/// Replace the host's conversation for a view on another computer, as `/new`
/// does in the host's own pane. Returns the message a refusal shows.
fn start_new_conversation(
    session: &mut AgentSession,
    cx: &mut Context<AgentSession>,
) -> Result<(), String> {
    // A running turn or command, or a branch operation, still writes into the
    // conversation a restart would drop.
    let busy = {
        let controller = session.controller.borrow();

        controller.runtime().status() == SessionStatus::Running
            || controller.commands().awaiting_turn
            || controller.branch().holds_composer()
    };

    if busy {
        return Err(t!("agent-composer-command-idle-only", name = "new").into_owned());
    }

    session.reset(cx);

    Ok(())
}

/// Take one branch step for a view on another computer, the way the host's
/// own pane takes it: the controller moves the operation on, and the session
/// does the reading and restarting that follows. Returns the message a
/// refused step shows.
fn take_branch_step(
    session: &mut AgentSession,
    step: BranchStep,
    cx: &mut Context<AgentSession>,
) -> Result<(), String> {
    let kind = session.kind;

    let update = match step {
        BranchStep::BeginRewind(target) => {
            let cwd = session.workspace.primary().map(str::to_owned);

            let request = session
                .controller
                .borrow_mut()
                .begin_rewind(cwd, target)
                .map_err(|error| branch_error_message(error, kind))?;

            session.read_checkpoints(request, cx);

            return Ok(());
        }
        BranchStep::SelectCheckpoint(checkpoint) => {
            session
                .controller
                .borrow_mut()
                .select_checkpoint(checkpoint);

            return Ok(());
        }
        BranchStep::Rewind(RewindAction::Cancel) | BranchStep::Cancel => {
            if session.controller.borrow_mut().cancel_branch_picker() {
                session.publish(SessionEffect::BranchClosed, cx);
            }

            return Ok(());
        }
        BranchStep::Rewind(action) => session.controller.borrow_mut().rewind(action),
        BranchStep::BeginFork(target) => {
            return session
                .controller
                .borrow_mut()
                .begin_fork(target)
                .map_err(|error| branch_error_message(error, kind));
        }
        BranchStep::Fork(checkpoint) => session.controller.borrow_mut().fork(checkpoint),
    };

    match update {
        BranchUpdate::Failed(failure) => Err(branch_failure_message(failure, kind)),
        // Published, this step would have the host's pane keep the cut
        // prompt for its own composer, for a branch its user did not start.
        BranchUpdate::Branching => {
            session
                .controller
                .borrow_mut()
                .begin_branched_conversation();

            Ok(())
        }
        update => {
            session.on_branch_update(update, cx);

            Ok(())
        }
    }
}

/// List or resume the host's conversations for a view on another computer.
/// Listed rows reach the view through the published view; a resume replays
/// the conversation into the host session, which every view follows.
fn take_history_step(
    session: &mut AgentSession,
    step: HistoryStep,
    cx: &mut Context<AgentSession>,
) -> Result<(), String> {
    let kind = session.kind;
    let cwd = session.workspace.primary().map(str::to_owned);

    match step {
        HistoryStep::List(scope) if kind.caps().filesystem_session_history => {
            session.controller.borrow_mut().clear_listed_history();

            cx.spawn(async move |this, cx| {
                let rows = cx
                    .background_executor()
                    .spawn(async move { list_scoped_sessions(scope, cwd.as_deref()) })
                    .await;

                let _ = this.update(cx, |session, cx| {
                    session.controller.borrow_mut().list_history(rows);

                    session.publish(SessionEffect::Changed, cx);
                });
            })
            .detach();

            Ok(())
        }
        // Codex pages its history over the protocol; the pages arrive as
        // events. DeepSeek sends every row once at start, which the
        // controller has kept.
        HistoryStep::List(scope) => {
            if kind == AgentKind::Codex {
                let mut controller = session.controller.borrow_mut();

                controller.clear_listed_history();

                controller.request_history(scope);
            }

            Ok(())
        }
        HistoryStep::Resume(summary) => {
            let outcome = session
                .controller
                .borrow_mut()
                .begin_resume(&summary, cwd.as_deref());

            match outcome {
                ResumeStart::Requested => {
                    session
                        .controller
                        .borrow_mut()
                        .controls
                        .seed_settings(SettingsSeed::resumed(kind));

                    Ok(())
                }
                ResumeStart::ReadReplay(request) => {
                    session.read_resume(request, cx);

                    Ok(())
                }
                ResumeStart::Busy => Err(t!("agent-composer-resume-idle-only").into_owned()),
                ResumeStart::Rejected => {
                    Err(t!("agent-session-codex-recent-not-ready").into_owned())
                }
                // The host opens such a conversation in a tab of its own
                // directory, which is the host user's decision to make.
                ResumeStart::Elsewhere { cwd, .. } => {
                    Err(t!("agent-remote-resume-elsewhere", cwd = cwd).into_owned())
                }
            }
        }
    }
}

fn publish_earlier_page(
    session: &AgentSession,
    views: &mut [PagedView],
    view_id: &str,
    cursor: Value,
) -> Result<Value, String> {
    let view = views
        .iter_mut()
        .find(|view| view.id == view_id)
        .ok_or_else(|| "the transcript view is no longer attached".to_owned())?;

    let permit = view
        .updates
        .try_reserve()
        .map_err(|_| "the transcript view is catching up; retry loading history".to_owned())?;

    let cursor = serde_json::from_value::<ViewCursor>(cursor).map_err(|error| error.to_string())?;

    let controller = session.controller.borrow();
    let catch_up = view.publisher.changes_paged(&controller);

    let page = match view.publisher.earlier_paged(&controller, cursor) {
        Ok(page) => page,
        Err(error) => {
            if let Some(update) = catch_up {
                permit.send(serde_json::json!({
                    "view_id": view_id,
                    "type": "update",
                    "value": update,
                }));
            }

            return Err(error.to_owned());
        }
    };

    permit.send(serde_json::json!({
        "view_id": view_id,
        "type": "page",
        "catch_up": catch_up,
        "value": page,
    }));

    Ok(serde_json::json!({}))
}

fn publish_paged(session: &AgentSession, views: &mut Vec<PagedView>) {
    views.retain(|view| !view.updates.is_closed());

    for view in views {
        // A slow transfer keeps one pending update. The publisher version only
        // advances after capacity is reserved, so later changes include it.
        let Ok(permit) = view.updates.try_reserve() else {
            continue;
        };

        if let Some(update) = view.publisher.changes_paged(&session.controller.borrow()) {
            permit.send(serde_json::json!({
                "view_id": view.id,
                "type": "update",
                "value": update,
            }));
        }
    }
}

fn publish(
    session: &AgentSession,
    publisher: &mut ViewPublisher,
    views: &mut Vec<UnboundedSender<Value>>,
) {
    if views.is_empty() {
        return;
    }

    let ops = publisher.changes(&session.controller.borrow());

    if ops.is_empty() {
        return;
    }

    match serde_json::to_value(ops) {
        Ok(ops) => views.retain(|view| view.send(ops.clone()).is_ok()),
        Err(error) => warn!(%error, "cannot encode agent view changes"),
    }
}

fn image(session: &AgentSession, params: Value) -> Result<Value, String> {
    let ImageRef { id, .. } = serde_json::from_value(params).map_err(|error| error.to_string())?;

    let image = session
        .controller
        .borrow()
        .conversation_image(id)
        .ok_or_else(|| format!("no image {id}"))?;

    serde_json::to_value(ImageData {
        bytes: image.bytes.clone(),
    })
    .map_err(|error| error.to_string())
}

/// Write a remote prompt's images where this session's composer keeps its
/// own, for a harness that reads images by path. A failed write leaves the
/// prompt short of paths, which such a harness refuses instead of sending
/// a message without its images.
fn stage_images(route: &str, images: &[PromptImage]) -> Vec<PathBuf> {
    let dir = scratch_dir(route);

    if let Err(error) = fs::create_dir_all(&dir) {
        warn!(%error, "cannot stage remote images");

        return Vec::new();
    }

    images
        .iter()
        .map_while(|image| {
            let extension = image
                .media_type
                .rsplit('/')
                .next()
                .filter(|extension| extension.chars().all(char::is_alphanumeric))
                .unwrap_or("png");

            let path = dir.join(format!("remote-{}.{extension}", Uuid::new_v4()));

            fs::write(&path, &image.bytes)
                .inspect_err(|error| warn!(%error, "cannot stage a remote image"))
                .ok()
                .map(|()| path)
        })
        .collect()
}

/// A pane's link to the host session it follows.
pub struct RemoteAgent {
    host: Arc<RemoteHost>,
    link: Arc<AgentLink>,
    _replica: Task<()>,

    /// Repaints the pane when the link to the host changes, so the banner
    /// follows it. The changes come from the network runtime, not a frame.
    _status: Task<()>,
}

impl RemoteAgent {
    /// The host and session, as a saved tab records them.
    pub fn address(&self) -> (String, String) {
        (
            self.host.id().as_str().to_owned(),
            self.link.session().to_owned(),
        )
    }

    /// Why the host ended this view, while it stays ended.
    pub(crate) fn ended(&self) -> Option<EndReason> {
        self.host.ended(self.link.session())
    }

    /// Take up again a view the host ended.
    pub(crate) fn reconnect(&self) {
        self.host.reconnect(self.link.session());
    }

    pub(crate) fn host_name(&self) -> String {
        self.host.name()
    }

    /// A strip saying the host is out of reach, or `None` while connected.
    /// The transcript stays as last received, so without it a dropped link
    /// looks like an agent that went quiet.
    pub(crate) fn banner(&self, cx: &App) -> Option<AnyElement> {
        let name = self.host.name();

        let (text, failed) = match *self.host.status().borrow() {
            Status::Connected => return None,
            Status::Idle | Status::Connecting => {
                (t!("remote-banner-connecting", name = name), false)
            }
            Status::Reconnecting => (t!("remote-banner-reconnecting", name = name), false),
            Status::Refused => (t!("remote-banner-refused", name = name), true),
            Status::Unreachable => (t!("remote-banner-unreachable", name = name), true),
        };

        let banner = h_flex()
            .w_full()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(if failed {
                cx.theme().danger.opacity(0.12)
            } else {
                cx.theme().primary.opacity(0.10)
            })
            .text_xs()
            .text_color(cx.theme().foreground)
            .child(text.into_owned());

        Some(banner.into_any_element())
    }

    /// Ask the host to list or resume its conversations, returning the
    /// message of a refused request.
    pub(crate) fn history(&self, step: &HistoryStep) -> JoinHandle<Result<Result<(), String>>> {
        let link = Arc::clone(&self.link);
        let params = serde_json::to_value(step);

        runtime().spawn(async move {
            let value = link.call(HISTORY_METHOD, params?).await?;

            Ok(serde_json::from_value(value)?)
        })
    }

    /// Ask the host to take a branch step, returning the message of a
    /// refused one.
    pub(crate) fn branch(&self, step: &BranchStep) -> JoinHandle<Result<Result<(), String>>> {
        let link = Arc::clone(&self.link);
        let params = serde_json::to_value(step);

        runtime().spawn(async move {
            let value = link.call(BRANCH_METHOD, params?).await?;

            Ok(serde_json::from_value(value)?)
        })
    }

    /// Ask the host to replace its conversation with a new one, returning
    /// the message of a refusal.
    pub(crate) fn new_conversation(&self) -> JoinHandle<Result<Result<(), String>>> {
        let link = Arc::clone(&self.link);

        runtime().spawn(async move {
            let value = link.call(NEW_CONVERSATION_METHOD, Value::Null).await?;

            Ok(serde_json::from_value(value)?)
        })
    }

    /// Send `command` to the host and return its outcome.
    pub(crate) fn send<C: AgentCommand>(&self, command: &C) -> JoinHandle<Result<C::Outcome>> {
        let link = Arc::clone(&self.link);
        let params = serde_json::to_value(command);

        runtime().spawn(async move {
            let value = link.call(C::METHOD, params?).await?;

            Ok(serde_json::from_value(value)?)
        })
    }
}

/// Open a view of `session`, an agent tab on `host`, as the owner and pane
/// of a tab.
pub fn open(
    host: Arc<RemoteHost>,
    session: String,
    kind: AgentKind,
    window: &mut Window,
    cx: &mut App,
) -> (SessionOwner, Entity<AgentPane>) {
    let profile = AgentProfile {
        name: host.name(),
        kind,
        ..AgentProfile::default()
    };

    let owner = AgentSession::create(profile, Default::default(), None, cx);

    owner
        .session()
        .update(cx, |session, _| session.follow_remote());

    let (link, updates) = host.agent_view(session);
    let link = Arc::new(link);

    let replica = follow(owner.session().downgrade(), Arc::clone(&link), updates, cx);

    let pane = cx.new(|cx| {
        let mut status = host.status();
        let mut ended = host.ended_changes();

        // The link's state and the host ending this view both change what
        // covers the pane.
        let status = cx.spawn(async move |this, cx| {
            loop {
                tokio::select! {
                    changed = status.changed() => if changed.is_err() { break },
                    changed = ended.changed() => if changed.is_err() { break },
                }

                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });

        let mut pane = AgentPane::attach(&owner, window, cx);

        pane.remote = Some(RemoteAgent {
            host,
            link,
            _replica: replica,
            _status: status,
        });

        // The host's conversations are listed on request with `/resume`; a
        // view of a running conversation has no use for them unasked.
        pane.history_ui.mode = RecentSessionsMode::Hidden;

        pane.refresh_git_branch(cx);

        pane
    });

    (owner, pane)
}

/// Keep `session`'s replica controller in step with the host.
fn follow(
    session: WeakEntity<AgentSession>,
    link: Arc<AgentLink>,
    mut updates: UnboundedReceiver<AgentUpdate>,
    cx: &mut App,
) -> Task<()> {
    cx.spawn(async move |cx| {
        let mut images: HashMap<Uuid, Arc<ConversationImage>> = HashMap::new();

        while let Some(update) = updates.recv().await {
            let ops = match update {
                AgentUpdate::Snapshot(view) => {
                    serde_json::from_value::<AgentView>(view).map(AgentView::into_ops)
                }
                AgentUpdate::Ops(ops) => serde_json::from_value::<Vec<ViewOp>>(ops),
                AgentUpdate::Paged(_) => {
                    warn!("ignoring a paged update on a legacy agent view");

                    continue;
                }
                AgentUpdate::Error(message) => {
                    let _ = session.update(cx, |session, cx| {
                        session
                            .controller
                            .borrow_mut()
                            .push_item(Item::Error { text: message });

                        session.publish(SessionEffect::Changed, cx);
                    });

                    break;
                }
                AgentUpdate::Ended => {
                    let _ = session.update(cx, |session, cx| {
                        session.controller.borrow_mut().push_item(Item::Error {
                            text: t!("agent-remote-ended").into_owned(),
                        });

                        session.publish(SessionEffect::Changed, cx);
                    });

                    break;
                }
            };

            let ops = match ops {
                Ok(ops) => ops,
                Err(error) => {
                    warn!(%error, "ignoring an undecodable agent view update");

                    continue;
                }
            };

            fetch_images(&link, &ops, &mut images).await;

            let applied = session.update(cx, |session, cx| {
                let applied = apply(session, ops, &images);

                for effect in applied.effects {
                    session.publish(effect, cx);
                }

                if let Some(title) = applied.title {
                    cx.emit(AgentPaneEvent::TitleSuggested(title));
                }
            });

            if applied.is_err() {
                break;
            }
        }
    })
}

/// What the pane has to hear after the replica changed: the effects a
/// session running here would have raised for the same change.
struct Applied {
    effects: Vec<SessionEffect>,
    title: Option<String>,
}

/// The parts of the replica whose transitions the pane reacts to.
struct Presented {
    working: bool,
    pending: Vec<(usize, QuestionKey)>,
    title: Option<String>,

    /// Whether the host's branch picker is open, and whether a branch
    /// operation holds the composer at all.
    picker: bool,

    branching: bool,

    /// How many conversations the host has listed for this view.
    listed: usize,
}

impl Presented {
    fn of(controller: &SessionController) -> Self {
        Self {
            picker: controller.branch().picker_is_open(),
            listed: controller.listed_history().len(),
            branching: controller.branch().holds_composer(),
            working: controller.conversation().borrow().live.is_working(),
            pending: controller
                .input()
                .batches()
                .iter()
                .enumerate()
                .filter(|(_, draft)| draft.pending())
                .map(|(index, draft)| (index, draft.key()))
                .collect(),
            title: controller.conversation_title().map(str::to_owned),
        }
    }
}

/// Apply operations to the replica and work out what the pane has to hear.
fn apply(
    session: &AgentSession,
    ops: Vec<ViewOp>,
    images: &HashMap<Uuid, Arc<ConversationImage>>,
) -> Applied {
    let mut controller = session.controller.borrow_mut();

    let before = Presented::of(&controller);

    let mut catalogs = false;

    for op in ops {
        match op {
            ViewOp::Splice { from, entries } => {
                let entries = entries
                    .into_iter()
                    .map(|entry| {
                        let attached = entry
                            .images
                            .iter()
                            .filter_map(|image| images.get(&image.id).cloned())
                            .collect();

                        entry.into_entry(attached)
                    })
                    .collect();

                controller.splice_transcript(from, entries);
            }
            ViewOp::Slot { slot } => {
                catalogs |= matches!(*slot, ViewSlot::Catalogs(_));

                controller.apply_slot(*slot);
            }
        }
    }

    let after = Presented::of(&controller);

    let mut effects = vec![SessionEffect::Changed];

    // The composer caches the catalogs it offers.
    if catalogs {
        effects.push(SessionEffect::Commands);
        effects.push(SessionEffect::Skills);
    }

    match (before.working, after.working) {
        (false, true) => effects.push(SessionEffect::TurnStarted { opened: true }),
        (true, false) => effects.push(SessionEffect::TurnCompleted { error: None }),
        _ => {}
    }

    // An opening picker takes the transcript to its selection, as a local
    // one does; an operation that ends gives the transcript back.
    if !before.picker && after.picker {
        effects.push(SessionEffect::Branch(BranchUpdate::Picker {
            unresolved: false,
        }));
    }

    if before.branching && !after.branching {
        effects.push(SessionEffect::BranchClosed);
    }

    // The list keeps the rows it already has and adds new ones, so the whole
    // listing goes each time it grows.
    if after.listed > before.listed {
        effects.push(SessionEffect::History(controller.listed_history().to_vec()));
    }

    effects.extend(
        after
            .pending
            .iter()
            .filter(|asked| !before.pending.iter().any(|seen| seen.1 == asked.1))
            .map(|(index, _)| SessionEffect::InputRequested { index: *index }),
    );

    if before
        .pending
        .iter()
        .any(|seen| !after.pending.iter().any(|asked| asked.1 == seen.1))
    {
        effects.push(SessionEffect::InputResolved(QuestionCompletion {
            message: None,
            started_turn: false,
            waiting_finished: false,
        }));
    }

    Applied {
        effects,
        title: after
            .title
            .filter(|title| before.title.as_ref() != Some(title)),
    }
}

/// Fetch the images `ops` show that this view does not hold yet. An image
/// that cannot be fetched is left out of its entry.
async fn fetch_images(
    link: &Arc<AgentLink>,
    ops: &[ViewOp],
    images: &mut HashMap<Uuid, Arc<ConversationImage>>,
) {
    let wanted: Vec<ImageRef> = ops
        .iter()
        .filter_map(|op| match op {
            ViewOp::Splice { entries, .. } => Some(entries),
            ViewOp::Slot { .. } => None,
        })
        .flatten()
        .flat_map(|entry| entry.images.iter().copied())
        .filter(|image| !images.contains_key(&image.id))
        .collect();

    for image in wanted {
        let link = Arc::clone(link);

        let fetched = runtime()
            .spawn(async move {
                let value = link
                    .call(IMAGE_METHOD, serde_json::to_value(image)?)
                    .await?;

                anyhow::Ok(serde_json::from_value::<ImageData>(value)?)
            })
            .await
            .context("fetching stopped")
            .and_then(|fetched| fetched);

        match fetched {
            Ok(ImageData { bytes }) => {
                images.insert(
                    image.id,
                    Arc::new(ConversationImage {
                        id: image.id,
                        bytes,
                    }),
                );
            }
            Err(error) => warn!(%error, "cannot fetch a transcript image"),
        }
    }
}
