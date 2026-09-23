pub use crate::agent_tab::team::controls::TeamCommand;
pub use crate::agent_tab::team::view::TeamPane;

mod controls;
mod dispatch;
mod events;
mod member_host;
mod operations;
mod view;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};

use futures::channel::oneshot;
use gpui::{App, AppContext as _, BackgroundExecutor, Context, Entity, Task};
use nmt_agent::AgentWorkspace;
use nmt_agent::chat::SendOutcome;
use nmt_agent::session::team_capabilities::TeamLaunch;
use nmt_agent::session::{AgentKind, RecoveryIdentity};
use nmt_agent::team::attempt::{Attempt, AttemptState, Invocation};
use nmt_agent::team::member::MemberConfig;
use nmt_agent::team::model::{AttemptId, MemberId, RoomId};
use nmt_agent::team::room::Room;
use nmt_agent::team::session::{AttemptEventKey, TeamError, TeamSession};
use nmt_config::profile::AgentProfile;

use crate::agent_tab::execution::{AgentSession, ExecutionSignal, SessionOwner};
use crate::agent_tab::settings::AgentSettings;
use crate::agent_tab::team::member_host::MemberHost;
use crate::agent_tab::team::operations::{ExecutionOutcome, MemberSnapshot};

type Operation = Box<dyn FnOnce(&mut TeamRuntime, &mut Context<TeamRuntime>)>;

/// The UI observes the last saved room. One operation owns the session on a
/// background thread until its complete storage update has finished.
pub struct TeamRuntime {
    directory: PathBuf,
    session: Option<TeamSession>,
    room: Room,
    id: RoomId,
    recovery: Vec<Attempt>,
    revision: u64,
    pending: VecDeque<Operation>,
    executor: BackgroundExecutor,
    hosts: BTreeMap<MemberId, MemberHost>,
    error: Option<String>,
    scheduled: bool,
    refresh_again: bool,
    loading: bool,
    load_failed: bool,
    closed: bool,
}

impl Drop for TeamRuntime {
    fn drop(&mut self) {
        if let Some(mut session) = self.session.take() {
            self.executor
                .spawn(async move {
                    if let Err(error) = session.close() {
                        tracing::warn!("could not save closed Team: {error}");
                    }
                })
                .detach();
        }
    }
}

impl TeamRuntime {
    pub fn create(directory: &Path, workspace: AgentWorkspace, cx: &mut App) -> Entity<Self> {
        let room = Room::new(workspace);
        let directory = directory.to_owned();
        let initial = room.clone();

        Self::load(
            directory.clone(),
            room.id(),
            room,
            move || TeamSession::create(&directory, initial),
            cx,
        )
    }

    pub fn open(directory: &Path, id: RoomId, cx: &mut App) -> Entity<Self> {
        let directory = directory.to_owned();

        Self::load(
            directory.clone(),
            id,
            Room::new(AgentWorkspace::default()),
            move || TeamSession::open(&directory, id),
            cx,
        )
    }

    fn load(
        directory: PathBuf,
        id: RoomId,
        room: Room,
        load: impl FnOnce() -> Result<TeamSession, TeamError> + Send + 'static,
        cx: &mut App,
    ) -> Entity<Self> {
        let task = cx.background_executor().spawn(async move { load() });

        let entity = cx.new(|cx| Self {
            directory,
            session: None,
            room,
            id,
            recovery: Vec::new(),
            revision: 0,
            pending: VecDeque::new(),
            executor: cx.background_executor().clone(),
            hosts: BTreeMap::new(),
            error: None,
            scheduled: false,
            refresh_again: false,
            loading: true,
            load_failed: false,
            closed: false,
        });

        entity.update(cx, |_, cx| {
            cx.on_app_quit(|this, cx| {
                let closed = this.close(cx);

                async move {
                    let _ = closed.await;
                }
            })
            .detach();
        });

        let weak = entity.downgrade();
        let keep_alive = entity.clone();
        let executor = cx.background_executor().clone();

        cx.spawn(async move |cx| {
            let mut loaded = Some(task.await);

            let _ = weak.update(cx, |this, cx| {
                this.loading = false;

                match loaded.take().unwrap() {
                    Ok(session) => {
                        this.install(session);

                        if !this.closed {
                            let members: Vec<_> = this
                                .room
                                .members()
                                .iter()
                                .filter(|member| !member.excluded())
                                .cloned()
                                .collect();

                            for member in members {
                                let profile = cx
                                    .global::<AgentSettings>()
                                    .profiles
                                    .iter()
                                    .find(|profile| {
                                        profile.kind == member.profile().kind
                                            && profile.name == member.profile().name
                                    })
                                    .cloned();

                                if let Some(profile) = profile {
                                    this.attach_member(member.id(), profile, cx);
                                } else {
                                    this.error = Some(format!(
                                        "The profile for {} is unavailable. Restore that profile before continuing.",
                                        member.name()
                                    ));
                                }
                            }

                            this.schedule(cx);
                        }

                        this.start_next(cx);
                    }
                    Err(error) => {
                        this.load_failed = true;
                        this.error = Some(error.to_string());

                        this.pending.clear();
                    }
                }

                cx.notify();
            });

            if let Some(Ok(mut session)) = loaded {
                executor
                    .spawn(async move {
                        let _ = session.close();
                    })
                    .detach();
            }

            drop(keep_alive);
        })
        .detach();

        entity
    }

    #[cfg(test)]
    fn new(session: TeamSession, executor: BackgroundExecutor) -> Self {
        let room = session.store().room().clone();

        Self {
            directory: session.store().data_directory().to_owned(),
            id: room.id(),
            room,
            recovery: session.pending_recovery().cloned().collect(),
            revision: session.store().revision(),
            session: Some(session),
            pending: VecDeque::new(),
            executor,
            hosts: BTreeMap::new(),
            error: None,
            scheduled: false,
            refresh_again: false,
            loading: false,
            load_failed: false,
            closed: false,
        }
    }

    fn install(&mut self, session: TeamSession) {
        self.room = session.store().room().clone();
        self.recovery = session.pending_recovery().cloned().collect();
        self.revision = session.store().revision();
        self.session = Some(session);
    }

    pub fn id(&self) -> RoomId {
        self.id
    }

    pub fn room(&self) -> &Room {
        &self.room
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    fn pending_recovery(&self) -> impl Iterator<Item = &Attempt> {
        self.recovery.iter()
    }

    fn enqueue(&mut self, operation: Operation, cx: &mut Context<Self>) {
        self.pending.push_back(operation);
        self.start_next(cx);
    }

    fn start_next(&mut self, cx: &mut Context<Self>) {
        if self.session.is_some()
            && let Some(operation) = self.pending.pop_front()
        {
            operation(self, cx);
        }
    }

    /// Queue `work` behind the operations already waiting and resolve with
    /// its result once `apply` has folded it into this runtime.
    fn run<R: Send + 'static>(
        &mut self,
        work: impl FnOnce(&mut TeamSession) -> Result<R, TeamError> + Send + 'static,
        apply: impl FnOnce(&mut Self, &Result<R, TeamError>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Task<Result<R, TeamError>> {
        self.run_prepared(move |_, _| work, apply, cx)
    }

    /// Queue an operation whose work is decided when it starts, so it sees
    /// the hosts as they are then instead of as they were when queued.
    fn run_prepared<R, W>(
        &mut self,
        prepare: impl FnOnce(&mut Self, &mut Context<Self>) -> W + 'static,
        apply: impl FnOnce(&mut Self, &Result<R, TeamError>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Task<Result<R, TeamError>>
    where
        R: Send + 'static,
        W: FnOnce(&mut TeamSession) -> Result<R, TeamError> + Send + 'static,
    {
        if self.load_failed {
            return Task::ready(Err(TeamError::Unavailable));
        }

        let (sender, receiver) = oneshot::channel();

        self.enqueue(
            Box::new(move |this, cx| {
                let work = prepare(this, cx);

                this.run_now(
                    work,
                    move |this, result, cx| {
                        apply(this, &result, cx);

                        let _ = sender.send(result);
                    },
                    cx,
                );
            }),
            cx,
        );

        cx.spawn(async move |_, _| receiver.await.unwrap_or(Err(TeamError::Unavailable)))
    }

    /// Run `work` on the session right away. Only an operation that already
    /// holds its turn may call this: the session is taken for the duration.
    fn run_now<R: Send + 'static>(
        &mut self,
        work: impl FnOnce(&mut TeamSession) -> Result<R, TeamError> + Send + 'static,
        apply: impl FnOnce(&mut Self, Result<R, TeamError>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let mut session = self.session.take().expect("one Team operation at a time");

        let task = cx.background_executor().spawn(async move {
            let result = work(&mut session);

            (session, result)
        });

        let executor = self.executor.clone();
        let keep_alive = cx.entity();

        cx.spawn(async move |this, cx| {
            let (session, result) = task.await;

            let mut finished = Some(session);

            let _ = this.update(cx, |this, cx| {
                this.install(finished.take().unwrap());

                apply(this, result, cx);

                this.start_next(cx);

                cx.notify();
            });

            if let Some(mut session) = finished {
                executor
                    .spawn(async move {
                        let _ = session.close();
                    })
                    .detach();
            }

            drop(keep_alive);
        })
        .detach();
    }

    pub fn add_member(
        &mut self,
        profile: AgentProfile,
        config: MemberConfig,
        cx: &mut Context<Self>,
    ) -> Task<Result<MemberId, TeamError>> {
        let valid = config.profile.kind == profile.kind
            && config.profile.name == profile.name
            && !self.closed;

        self.run(
            move |session| {
                if !valid {
                    return Err(TeamError::Unavailable);
                }

                session.add_member(config)
            },
            move |this, result, cx| {
                if let Ok(id) = result
                    && !this.closed
                {
                    this.attach_member(*id, profile, cx);
                    this.schedule(cx);
                }
            },
            cx,
        )
    }

    fn attach_member(&mut self, id: MemberId, profile: AgentProfile, cx: &mut Context<Self>) {
        let Some(member) = self.room().member(id) else {
            return;
        };

        let recovery = member
            .provider_id()
            .map(|provider| RecoveryIdentity::new(member.profile().kind, provider));

        let options = TeamLaunch {
            moderator: member.profile().kind == AgentKind::Codex
                && (member.provider_id().is_none() || member.moderator_registered()),
            restore_transcript: true,
        };

        let settings = member.settings().clone();

        let owner = AgentSession::create_team(profile, member.roots().clone(), options, cx);

        self.attach_member_owner(id, owner, cx);

        if let Some(host) = self.hosts.get(&id) {
            host.apply_settings(settings, cx);

            host.start(recovery, cx);
        }
    }

    fn attach_member_owner(&mut self, id: MemberId, owner: SessionOwner, cx: &mut Context<Self>) {
        let session = owner.session().clone();

        let events = cx.subscribe(&session, move |this, _, event, cx| {
            this.on_execution(id, event, cx)
        });

        let changed = cx.observe(&session, move |this, _, cx| this.schedule(cx));

        self.hosts
            .insert(id, MemberHost::new(owner, vec![events, changed]));
    }

    #[cfg(test)]
    fn member_session(&self, member: MemberId) -> Option<&Entity<AgentSession>> {
        self.hosts.get(&member).map(|host| host.owner.session())
    }

    fn schedule(&mut self, cx: &mut Context<Self>) {
        cx.notify();

        if self.closed {
            return;
        }

        if self.scheduled {
            self.refresh_again = true;

            return;
        }

        self.scheduled = true;

        self.enqueue(Box::new(|this, cx| this.pump(cx)), cx);
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            self.scheduled = false;

            self.start_next(cx);

            return;
        }

        let mut members: Vec<_> = self
            .hosts
            .iter()
            .map(|(id, host)| MemberSnapshot::capture(*id, host, cx))
            .collect();

        self.run_now(
            move |session| {
                let pending = operations::refresh(session, &mut members)?;

                Ok((members, pending))
            },
            |this, result, cx| {
                this.scheduled = false;

                if this.closed {
                    return;
                }

                match result {
                    Err(error) => this.error = Some(error.to_string()),
                    Ok((members, pending)) => {
                        for member in members {
                            if let Some(message) = &member.start_failure {
                                this.error = Some(message.clone());
                            }

                            if let Some(host) = this.hosts.get_mut(&member.id) {
                                host.active = member.active;
                                host.ready_epoch = member.ready_epoch;
                            }
                        }

                        for id in pending {
                            this.dispatch(id, cx);
                        }
                    }
                }

                if this.refresh_again {
                    this.refresh_again = false;

                    this.schedule(cx);
                }
            },
            cx,
        );
    }

    pub fn close(&mut self, cx: &mut Context<Self>) -> Task<Result<(), TeamError>> {
        self.closed = true;

        self.run(
            |session| session.close(),
            |this, result, cx| {
                if let Err(error) = result {
                    tracing::warn!("could not save closed Team: {error}");
                    this.error = Some(error.to_string());
                }

                for host in this.hosts.values() {
                    host.interrupt(cx);
                    host.owner.close();
                }
            },
            cx,
        )
    }

    pub fn command(
        &mut self,
        command: TeamCommand,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), TeamError>> {
        let closed = self.closed;

        let settings_member = match &command {
            TeamCommand::MemberSettings { member, .. } => Some(*member),
            _ => None,
        };

        let stopped = match &command {
            TeamCommand::Stop(member) => Some(*member),
            _ => None,
        };

        self.run_prepared(
            move |this, cx| {
                // Which members are busy is read when the command starts, so
                // a turn that finished while it waited in the queue counts.
                let busy: Vec<_> = this
                    .hosts
                    .iter()
                    .filter(|(_, host)| host.is_busy(cx))
                    .map(|(id, _)| *id)
                    .collect();

                move |session| {
                    if closed {
                        return Err(TeamError::Unavailable);
                    }

                    operations::command(session, command, &busy)
                }
            },
            move |this, result, cx| {
                if result.is_ok() && !this.closed {
                    this.error = None;

                    if let Some(id) = settings_member
                        && let Some(member) = this.room.member(id)
                        && let Some(host) = this.hosts.get(&id)
                    {
                        host.apply_settings(member.settings().clone(), cx);
                    }

                    if let Some(id) = stopped
                        && let Some(host) = this.hosts.get(&id)
                    {
                        host.interrupt(cx);
                    }

                    this.schedule(cx);
                }
            },
            cx,
        )
    }

    fn dispatch(&mut self, id: AttemptId, cx: &mut Context<Self>) {
        self.enqueue(
            Box::new(move |this, cx| {
                let eligible = this
                    .room
                    .attempts()
                    .iter()
                    .find(|attempt| attempt.id == id)
                    .filter(|attempt| {
                        matches!(attempt.intent.invocation, Invocation::MemberConversation)
                    })
                    .and_then(|attempt| this.hosts.get(&attempt.intent.recipient))
                    .is_some_and(|host| !host.is_busy(cx));

                if !eligible || this.closed {
                    this.start_next(cx);

                    return;
                }

                this.run_now(
                    move |session| match session.prepare_dispatch(id) {
                        Ok(intent) => Ok(Some(intent)),
                        Err(TeamError::Busy | TeamError::Paused | TeamError::Unavailable) => {
                            Ok(None)
                        }
                        Err(error) => Err(error),
                    },
                    move |this, result, cx| {
                        let intent = match result {
                            Ok(Some(intent)) => intent,
                            Ok(None) => return,
                            Err(error) => {
                                this.error = Some(error.to_string());

                                return;
                            }
                        };

                        let recipient = intent.recipient;

                        let outcome = if this.closed {
                            SendOutcome::NotReady
                        } else {
                            match (
                                this.hosts.get_mut(&intent.recipient),
                                this.room.member(intent.recipient),
                            ) {
                                (Some(host), Some(member)) => {
                                    host.active = Some(id);

                                    host.submit(&intent, member.settings(), cx)
                                }
                                _ => SendOutcome::NotReady,
                            }
                        };

                        this.run_now(
                            move |session| session.finish_dispatch(id, &outcome),
                            move |this, result, cx| {
                                if let Err(error) = result {
                                    this.error = Some(error.to_string());
                                }

                                if this.room.attempts().iter().any(|attempt| {
                                    attempt.id == id && attempt.state == AttemptState::Rejected
                                }) && let Some(host) = this.hosts.get_mut(&recipient)
                                {
                                    host.active = None;
                                }

                                this.schedule(cx);
                            },
                            cx,
                        );
                    },
                    cx,
                );
            }),
            cx,
        );
    }

    fn on_execution(&mut self, member: MemberId, signal: &ExecutionSignal, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }

        let Some(attempt) = self.hosts.get(&member).and_then(|host| host.active) else {
            return;
        };

        let (epoch, decision, failure) = match signal {
            ExecutionSignal::Accepted { epoch, .. } => (*epoch, None, None),
            ExecutionSignal::Finished { epoch, error, .. } => (*epoch, None, error.clone()),
            ExecutionSignal::Decision { epoch, request } => (*epoch, Some(request.clone()), None),
        };

        let signal = signal.clone();

        self.run(
            move |session| {
                operations::execution(
                    session,
                    AttemptEventKey {
                        attempt,
                        member,
                        backend_generation: epoch,
                    },
                    signal,
                )
            },
            move |this, result, cx| {
                let accepted = match result {
                    Ok(ExecutionOutcome::DecisionAccepted) => true,
                    Ok(
                        ExecutionOutcome::Ignored
                        | ExecutionOutcome::Applied
                        | ExecutionOutcome::DecisionRejected,
                    ) => false,
                    Err(error) => {
                        this.error = Some(error.to_string());

                        false
                    }
                };

                // A decision request needs an answer whatever became of it;
                // an unanswered one would leave the backend waiting.
                if let Some(request) = decision
                    && let Some(host) = this.hosts.get(&member)
                {
                    host.respond_decision(&request, accepted, cx);
                }

                if let Some(failure) = failure {
                    this.error = Some(failure);
                }

                this.schedule(cx);
            },
            cx,
        )
        .detach();
    }
}
