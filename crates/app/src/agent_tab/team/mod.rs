pub use crate::agent_tab::team::controls::TeamCommand;
pub use crate::agent_tab::team::view::TeamPane;

mod controls;
mod dispatch;
mod events;
mod recovery;

mod view;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gpui::{App, AppContext as _, Context, Entity, Subscription};
use nmt_agent::AgentWorkspace;
use nmt_agent::chat::ThreadSettings;
use nmt_agent::session::team_capabilities::TeamLaunch;
use nmt_agent::session::{AgentKind, RecoveryIdentity};
use nmt_agent::team::identity::{AttemptId, InteractionId, MemberId, RoomId};
use nmt_agent::team::member::MemberConfig;
use nmt_agent::team::room::Room;
use nmt_agent::team::session::{TeamError, TeamSession};
use nmt_config::profile::AgentProfile;

use crate::agent_tab::execution::{AgentSession, SessionOwner};
use crate::agent_tab::settings::AgentSettings;

struct MemberHost {
    owner: SessionOwner,
    active: Option<AttemptId>,
    interaction: Option<InteractionId>,
    ready_epoch: Option<u64>,
    _subscriptions: Vec<Subscription>,
}

/// Session owners stay alive when the Team view is hidden. Provider events
/// update the durable room before another arrangement becomes eligible.
pub struct TeamRuntime {
    session: TeamSession,
    hosts: BTreeMap<MemberId, MemberHost>,
    data_directory: PathBuf,
    error: Option<String>,
    scheduled: bool,
    closed: bool,
}

impl Drop for TeamRuntime {
    fn drop(&mut self) {
        if !self.closed
            && let Err(error) = self.session.close()
        {
            tracing::warn!("could not save closed Team: {error}");
        }
    }
}

impl TeamRuntime {
    pub fn create(
        data_directory: &Path,
        workspace: AgentWorkspace,
        cx: &mut App,
    ) -> Result<Entity<Self>, TeamError> {
        let session = TeamSession::create(data_directory, Room::new(workspace))?;

        Ok(cx.new(|_| Self::new(session, data_directory)))
    }

    pub fn open(
        data_directory: &Path,
        id: RoomId,
        cx: &mut App,
    ) -> Result<Entity<Self>, TeamError> {
        let (session, notices) = TeamSession::open(data_directory, id)?;
        let entity = cx.new(|_| Self::new(session, data_directory));

        entity.update(cx, |this, cx| {
            if !notices.is_empty() { this.error = Some(format!("Recovery notices: {notices:?}")); }

            let members: Vec<_> = this.room().members().iter().filter(|member| !member.excluded()).cloned().collect();

            for member in members {
                let profile = cx.global::<AgentSettings>().profiles.iter().find(|profile| profile.kind == member.profile().kind && profile.name == member.profile().name).cloned();

                match profile {
                    Some(profile) => this.attach_member(member.id(), profile, cx),
                    None => { this.error = Some(format!("The profile for {} is unavailable. Restore that profile before continuing.", member.name())); }
                }
            }

            this.schedule(cx);
        });

        Ok(entity)
    }

    fn new(session: TeamSession, data_directory: &Path) -> Self {
        Self {
            session,
            hosts: BTreeMap::new(),
            data_directory: data_directory.to_owned(),
            error: None,
            scheduled: false,
            closed: false,
        }
    }

    pub fn room(&self) -> &Room {
        self.session.room()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn data_directory(&self) -> &Path {
        &self.data_directory
    }

    pub fn add_member(
        &mut self,
        profile: AgentProfile,
        config: MemberConfig,
        cx: &mut Context<Self>,
    ) -> Result<MemberId, TeamError> {
        if config.profile.kind != profile.kind || config.profile.name != profile.name {
            return Err(TeamError::Unavailable);
        }

        let id = self.session.add_member(config)?;

        self.attach_member(id, profile, cx);
        self.schedule(cx);

        Ok(id)
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

        let owner = AgentSession::create_team(profile, member.roots().clone(), options, cx);

        owner.session().update(cx, |session, _| {
            session
                .controller
                .borrow_mut()
                .set_settings(member.settings().clone());
        });

        self.attach_member_owner(id, owner, cx);

        if let Some(host) = self.hosts.get(&id) {
            host.owner.session().update(cx, |session, cx| {
                session.start(recovery, true, |_, _| {}, cx);
            });
        }
    }

    fn attach_member_owner(&mut self, id: MemberId, owner: SessionOwner, cx: &mut Context<Self>) {
        let session = owner.session().clone();

        let events = cx.subscribe(&session, move |this, _, event, cx| {
            this.on_execution(id, event, cx)
        });

        let changed = cx.observe(&session, move |this, _, cx| this.schedule(cx));

        self.hosts.insert(
            id,
            MemberHost {
                owner,
                active: None,
                interaction: None,
                ready_epoch: None,
                _subscriptions: vec![events, changed],
            },
        );
    }

    pub fn member_session(&self, member: MemberId) -> Option<&Entity<AgentSession>> {
        self.hosts.get(&member).map(|host| host.owner.session())
    }

    pub fn member_settings(&self, member: MemberId, cx: &App) -> Option<ThreadSettings> {
        Some(
            self.member_session(member)?
                .read(cx)
                .controller
                .borrow()
                .controls()
                .settings
                .clone(),
        )
    }

    fn schedule(&mut self, cx: &mut Context<Self>) {
        cx.notify();

        if self.scheduled || self.closed {
            return;
        }

        self.scheduled = true;

        cx.spawn(async move |this, cx| {
            let _ = this.update(cx, |this, cx| {
                this.scheduled = false;

                if let Err(error) = this.pump(cx) {
                    this.error = Some(error.to_string());
                }

                cx.notify();
            });
        })
        .detach();
    }

    pub fn close(&mut self, cx: &mut Context<Self>) -> Result<(), TeamError> {
        self.closed = true;
        self.session.close()?;

        for host in self.hosts.values() {
            host.owner.session().update(cx, |session, cx| {
                session.controller.borrow_mut().interrupt_from_user();

                cx.notify();
            });

            host.owner.close();
        }

        Ok(())
    }
}
