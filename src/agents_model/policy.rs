//! The one permission check of the agents model: who may do what to which
//! tab. Pure: the App computes the actor, the relation and the turn, and
//! this decides. No I/O.
//!
//! The matrix (decisions file, design §2.2), "UT" = the caller's current
//! turn is an effective user turn:
//!
//! | Action | self | teammate | other | coordinator | protected |
//! |---|---|---|---|---|---|
//! | Read | ✓ | ✓ | ✓ | ✓ | ✓ |
//! | ReadShellScreen | ✓ | ✓ | shell_private | ✓ | ✓ |
//! | Message | invalid_target | ✓ | ✓ | UT or reply | ✓ |
//! | SoftEdit | ✓ | ✓ | outside_team | UT | protected_tab |
//! | SuspendRestart, Activate (the coordinator too) | ✓ | ✓ | outside_team | UT | protected_tab |
//! | Reopen/Activate of a user's entry (D4) | UT | UT | outside_team | UT | - |
//! | MoveTab (not into another team) | ✓ | ✓ | outside_team | UT | protected_tab |
//! | MoveTab into another team | outside_team | outside_team | outside_team | UT | protected_tab |
//! | OpenTab into own team | ✓ | | | UT | |
//! | OpenTab elsewhere (D1) | UT | | | UT | |
//! | OpenTab into another team | outside_team | | | UT | |
//! | OwnOnly | ✓ | own_only | own_only | UT | own_only |
//! | Close | UT | UT | outside_team | UT | protected_tab |
//! | TeamStructure (D2) | UT | UT | outside_team | UT | - |
//! | ReorderTab (in a team group; no team: outside_team) | ✓ | ✓ | outside_team | UT | protected_tab |
//! | ReorderGroup (the user's whole sidebar) | coordinator_only | coordinator_only | coordinator_only | UT | - |
//!
//! The user may do everything.

use crate::api::schema::agents_model::error_code;

/// Who acts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    User,
    /// The server-identified coordinator pane (a member of every team).
    Coordinator,
    /// A live agent. `team` is whether it is a member of its group's team
    /// (an excluded pane has no team).
    Agent {
        team: bool,
    },
}

/// The caller's relation to the target tab or pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// The caller's own tab (or pane).
    SelfPane,
    /// A tab in the caller's team group, with no excluded agent pane.
    Teammate,
    /// Another team, a plain group (also the caller's own plain group), the
    /// top space, or a split with an excluded pane.
    Other,
    /// The coordinator's tab.
    Protected,
}

/// Where a tab goes (move, open, reopen).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dest {
    /// The caller's own team group.
    OwnTeam,
    /// Another team's group.
    OtherTeam,
    /// A plain group, a new group, or the top space.
    PlainOrNewOrTop,
}

/// A free edit inside the team (logged).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftEdit {
    RenameTab,
    SetMeta,
    NotesAppend,
    Checkpoint,
    /// Tab color, important, reminders.
    Cosmetic,
    SuspendRestart,
    /// Typing into a shell pane.
    ShellInput,
    /// Reopening a closed tab. `by_agent`: the entry was closed by an agent
    /// (`false` also when the entry does not say, fail closed).
    Reopen {
        by_agent: bool,
        dest: Dest,
    },
    /// Activating a suspended agent. `by_agent` as for `Reopen`.
    Activate {
        by_agent: bool,
    },
}

/// Writes only the owner may make.
// The owner's own-pane writes run through the existing `notes.*` and
// `checkpoints.*` methods; the variants keep the matrix complete and tested.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnOnly {
    /// Replacing the notes.
    NotesWrite,
    CheckpointUpdate,
    CheckpointRemove,
}

/// Team structure (D2). Disband, join and leave stay the user's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamOp {
    /// Make the caller's own group a team.
    Make,
    /// Set the caller's team's purpose (also a group rename or move).
    Purpose,
}

/// What the caller wants to do.
// `Read` and `OwnOnly` are decided where they are used (every read is
// free); they keep the matrix complete and tested.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// List, get, status, notes and checkpoints, message and action logs,
    /// an agent pane's screen.
    Read,
    /// A shell pane's screen (U6).
    ReadShellScreen,
    Message,
    SoftEdit(SoftEdit),
    MoveTab {
        dest: Dest,
    },
    OpenTab {
        dest: Dest,
    },
    OwnOnly(OwnOnly),
    Close,
    TeamStructure(TeamOp),
    /// A tab's place among its group's tabs.
    ReorderTab,
    /// A group's place in the sidebar (every group moves with it).
    ReorderGroup,
}

/// The caller's turn and the facts the hints name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// The effective, poison-aware user turn (§3.6).
    pub user_turn: bool,
    /// For the coordinator's messages: the message answers the one that
    /// started its turn.
    pub reply_to_turn_starter: bool,
    /// What started the caller's turn, for the `non_user_turn` hint:
    /// "a teammate's message", "a herdr wake-up", "a scripted prompt",
    /// "no input".
    pub turn_desc: &'static str,
    /// The target's name as shown (a tab label, an agent name).
    pub target: String,
    /// The target sits in the caller's own plain group (the hint offers
    /// Make team).
    pub own_plain_group: Option<String>,
}

/// The decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { code: &'static str, hint: String },
}

impl Decision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

/// Decide `action` by `actor` on a target with `relation`.
pub fn authorize(actor: Actor, relation: Relation, action: Action, facts: &Facts) -> Decision {
    match actor {
        Actor::User => Decision::Allow,
        Actor::Coordinator => coordinator(relation, action, facts),
        Actor::Agent { team } => agent(team, relation, action, facts),
    }
}

/// The coordinator: a member of every team; every write needs its user's
/// turn. It may message on its own only to answer the message that started
/// its turn. Its own tab is protected from it too (D3).
fn coordinator(relation: Relation, action: Action, facts: &Facts) -> Decision {
    // Its agent's lifecycle too: suspending or restarting itself would end
    // the turn that asked.
    let tab_change = matches!(
        action,
        Action::Close
            | Action::MoveTab { .. }
            | Action::ReorderTab
            | Action::SoftEdit(
                SoftEdit::RenameTab | SoftEdit::SuspendRestart | SoftEdit::Activate { .. }
            )
    );
    if relation == Relation::Protected && tab_change {
        return protected(facts);
    }
    match action {
        Action::Read | Action::ReadShellScreen => Decision::Allow,
        Action::Message if facts.user_turn || facts.reply_to_turn_starter => Decision::Allow,
        _ if facts.user_turn => Decision::Allow,
        _ => non_user_turn(facts),
    }
}

fn agent(team: bool, relation: Relation, action: Action, facts: &Facts) -> Decision {
    use Relation::{Other, Protected, SelfPane, Teammate};
    // A caller without a team has no teammates.
    let relation = match relation {
        Teammate if !team => Other,
        relation => relation,
    };
    match action {
        Action::Read => Decision::Allow,
        Action::ReadShellScreen => match relation {
            SelfPane | Teammate | Protected => Decision::Allow,
            Other => deny(
                error_code::SHELL_PRIVATE,
                format!(
                    "the screen of shell {} is visible to its team and the coordinator; ask your user",
                    facts.target
                ),
            ),
        },
        Action::Message => match relation {
            SelfPane => deny(error_code::INVALID_TARGET, "that is you".into()),
            Teammate | Other | Protected => Decision::Allow,
        },
        Action::SoftEdit(SoftEdit::Reopen { by_agent, dest }) => match dest {
            Dest::OtherTeam => outside_team(facts),
            Dest::OwnTeam if team && by_agent => Decision::Allow,
            Dest::OwnTeam | Dest::PlainOrNewOrTop => turn_gate(facts),
        },
        Action::SoftEdit(SoftEdit::Activate { by_agent }) => match relation {
            Protected => protected(facts),
            Other => outside_team(facts),
            SelfPane | Teammate if by_agent => Decision::Allow,
            SelfPane | Teammate => turn_gate(facts),
        },
        Action::SoftEdit(_) => match relation {
            SelfPane | Teammate => Decision::Allow,
            Other => outside_team(facts),
            Protected => protected(facts),
        },
        Action::MoveTab { dest } => match relation {
            Protected => protected(facts),
            Other => outside_team(facts),
            SelfPane | Teammate if dest == Dest::OtherTeam => outside_team(facts),
            SelfPane | Teammate => Decision::Allow,
        },
        Action::OpenTab { dest } => match dest {
            Dest::OwnTeam if team => Decision::Allow,
            Dest::OtherTeam => outside_team(facts),
            Dest::OwnTeam | Dest::PlainOrNewOrTop => turn_gate(facts),
        },
        Action::OwnOnly(_) => match relation {
            SelfPane => Decision::Allow,
            Teammate | Other | Protected => deny(
                error_code::OWN_ONLY,
                format!(
                    "only {}'s own agent may do that; append instead, or ask your user",
                    facts.target
                ),
            ),
        },
        Action::Close => match relation {
            Protected => protected(facts),
            Other => outside_team(facts),
            SelfPane | Teammate => turn_gate(facts),
        },
        Action::TeamStructure(_) => match relation {
            Protected | Other => outside_team(facts),
            SelfPane | Teammate => turn_gate(facts),
        },
        // Reordering moves the other tabs of the group too: free inside the
        // caller's own team only (its own tab in a plain group included).
        Action::ReorderTab => match relation {
            Protected => protected(facts),
            SelfPane | Teammate if team => Decision::Allow,
            SelfPane | Teammate | Other => outside_team(facts),
        },
        Action::ReorderGroup => deny(
            error_code::COORDINATOR_ONLY,
            "the group order is your user's whole sidebar: only your user, or the coordinator at their request, reorders groups".into(),
        ),
    }
}

fn turn_gate(facts: &Facts) -> Decision {
    if facts.user_turn {
        Decision::Allow
    } else {
        non_user_turn(facts)
    }
}

fn deny(code: &'static str, hint: String) -> Decision {
    Decision::Deny { code, hint }
}

fn non_user_turn(facts: &Facts) -> Decision {
    let from = if facts.turn_desc.is_empty() {
        "no input"
    } else {
        facts.turn_desc
    };
    deny(
        error_code::NON_USER_TURN,
        format!(
            "this turn started from {from}, not your user; finish, then ask your user to repeat the request"
        ),
    )
}

fn outside_team(facts: &Facts) -> Decision {
    let mut hint = format!(
        "you can read and message {}; changing it needs its team, your user, or the coordinator",
        facts.target
    );
    if let Some(group) = &facts.own_plain_group {
        hint.push_str(&format!(", or ask your user to Make team on {group}"));
    }
    deny(error_code::OUTSIDE_TEAM, hint)
}

fn protected(_facts: &Facts) -> Decision {
    deny(
        error_code::PROTECTED_TAB,
        "the coordinator's tab is the user's; no agent renames, moves, closes, suspends or restarts it".into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTORS: [Actor; 4] = [
        Actor::User,
        Actor::Coordinator,
        Actor::Agent { team: true },
        Actor::Agent { team: false },
    ];
    const RELATIONS: [Relation; 4] = [
        Relation::SelfPane,
        Relation::Teammate,
        Relation::Other,
        Relation::Protected,
    ];

    fn facts(user_turn: bool) -> Facts {
        Facts {
            user_turn,
            turn_desc: "a teammate's message",
            target: "api".into(),
            ..Facts::default()
        }
    }

    fn code(decision: Decision) -> &'static str {
        match decision {
            Decision::Allow => "allow",
            Decision::Deny { code, .. } => code,
        }
    }

    fn agent(relation: Relation, action: Action, ut: bool) -> &'static str {
        code(authorize(
            Actor::Agent { team: true },
            relation,
            action,
            &facts(ut),
        ))
    }

    fn all_actions() -> Vec<Action> {
        let mut out = vec![Action::Read, Action::ReadShellScreen, Action::Message];
        for soft in [
            SoftEdit::RenameTab,
            SoftEdit::SetMeta,
            SoftEdit::NotesAppend,
            SoftEdit::Checkpoint,
            SoftEdit::Cosmetic,
            SoftEdit::SuspendRestart,
            SoftEdit::ShellInput,
            SoftEdit::Activate { by_agent: true },
            SoftEdit::Activate { by_agent: false },
        ] {
            out.push(Action::SoftEdit(soft));
        }
        for dest in [Dest::OwnTeam, Dest::OtherTeam, Dest::PlainOrNewOrTop] {
            out.push(Action::MoveTab { dest });
            out.push(Action::OpenTab { dest });
            for by_agent in [true, false] {
                out.push(Action::SoftEdit(SoftEdit::Reopen { by_agent, dest }));
            }
        }
        for own in [
            OwnOnly::NotesWrite,
            OwnOnly::CheckpointUpdate,
            OwnOnly::CheckpointRemove,
        ] {
            out.push(Action::OwnOnly(own));
        }
        out.push(Action::Close);
        out.push(Action::TeamStructure(TeamOp::Make));
        out.push(Action::TeamStructure(TeamOp::Purpose));
        out.push(Action::ReorderTab);
        out.push(Action::ReorderGroup);
        out
    }

    #[test]
    fn the_user_may_do_everything() {
        for relation in RELATIONS {
            for action in all_actions() {
                for ut in [true, false] {
                    assert!(authorize(Actor::User, relation, action, &facts(ut)).is_allowed());
                }
            }
        }
    }

    #[test]
    fn the_coordinator_reads_freely_and_writes_only_in_a_user_turn() {
        for relation in [Relation::SelfPane, Relation::Teammate, Relation::Other] {
            for action in all_actions() {
                assert!(
                    authorize(Actor::Coordinator, relation, action, &facts(true)).is_allowed(),
                    "{action:?}"
                );
                let without = code(authorize(
                    Actor::Coordinator,
                    relation,
                    action,
                    &facts(false),
                ));
                match action {
                    Action::Read | Action::ReadShellScreen => assert_eq!(without, "allow"),
                    _ => assert_eq!(without, "non_user_turn", "{action:?}"),
                }
            }
        }
        let reply = Facts {
            reply_to_turn_starter: true,
            ..facts(false)
        };
        assert!(
            authorize(Actor::Coordinator, Relation::Other, Action::Message, &reply).is_allowed()
        );
        // Its own tab is protected from it too (D3), its agent's lifecycle
        // included.
        for action in [
            Action::Close,
            Action::SoftEdit(SoftEdit::RenameTab),
            Action::MoveTab {
                dest: Dest::PlainOrNewOrTop,
            },
            Action::SoftEdit(SoftEdit::SuspendRestart),
            Action::SoftEdit(SoftEdit::Activate { by_agent: true }),
            Action::SoftEdit(SoftEdit::Activate { by_agent: false }),
            Action::ReorderTab,
        ] {
            assert_eq!(
                code(authorize(
                    Actor::Coordinator,
                    Relation::Protected,
                    action,
                    &facts(true)
                )),
                "protected_tab"
            );
        }
    }

    #[test]
    fn reads_are_free_and_shell_screens_stay_in_the_team() {
        for actor in ACTORS {
            for relation in RELATIONS {
                assert!(authorize(actor, relation, Action::Read, &facts(false)).is_allowed());
            }
        }
        assert_eq!(
            agent(Relation::SelfPane, Action::ReadShellScreen, false),
            "allow"
        );
        assert_eq!(
            agent(Relation::Teammate, Action::ReadShellScreen, false),
            "allow"
        );
        assert_eq!(
            agent(Relation::Protected, Action::ReadShellScreen, false),
            "allow"
        );
        assert_eq!(
            agent(Relation::Other, Action::ReadShellScreen, true),
            "shell_private"
        );
        // A caller without a team has no teammates.
        assert_eq!(
            code(authorize(
                Actor::Agent { team: false },
                Relation::Teammate,
                Action::ReadShellScreen,
                &facts(true)
            )),
            "shell_private"
        );
    }

    #[test]
    fn every_agent_messages_every_agent_but_itself() {
        assert_eq!(
            agent(Relation::SelfPane, Action::Message, true),
            "invalid_target"
        );
        for relation in [Relation::Teammate, Relation::Other, Relation::Protected] {
            assert_eq!(agent(relation, Action::Message, false), "allow");
        }
    }

    #[test]
    fn soft_edits_are_free_in_the_team_only() {
        let rename = Action::SoftEdit(SoftEdit::RenameTab);
        assert_eq!(agent(Relation::SelfPane, rename, false), "allow");
        assert_eq!(agent(Relation::Teammate, rename, false), "allow");
        assert_eq!(agent(Relation::Other, rename, true), "outside_team");
        // A rename attempt on the protected tab is refused before anything.
        assert_eq!(agent(Relation::Protected, rename, true), "protected_tab");
        // No team: a teammate relation degrades to other.
        assert_eq!(
            code(authorize(
                Actor::Agent { team: false },
                Relation::Teammate,
                rename,
                &facts(true)
            )),
            "outside_team"
        );
        // ... but its own tab stays its own.
        assert_eq!(
            code(authorize(
                Actor::Agent { team: false },
                Relation::SelfPane,
                rename,
                &facts(false)
            )),
            "allow"
        );
    }

    #[test]
    fn moves_stay_out_of_other_teams() {
        for dest in [Dest::OwnTeam, Dest::PlainOrNewOrTop] {
            assert_eq!(
                agent(Relation::SelfPane, Action::MoveTab { dest }, false),
                "allow"
            );
            // Moving out of the team is an ordinary, logged edit (decision 2).
            assert_eq!(
                agent(Relation::Teammate, Action::MoveTab { dest }, false),
                "allow"
            );
        }
        let into_other = Action::MoveTab {
            dest: Dest::OtherTeam,
        };
        for relation in [Relation::SelfPane, Relation::Teammate, Relation::Other] {
            assert_eq!(agent(relation, into_other, true), "outside_team");
        }
        assert_eq!(
            agent(Relation::Protected, into_other, true),
            "protected_tab"
        );
        assert_eq!(
            agent(
                Relation::Other,
                Action::MoveTab {
                    dest: Dest::OwnTeam
                },
                true
            ),
            "outside_team"
        );
    }

    #[test]
    fn opening_outside_the_team_needs_the_user() {
        let open = |dest| Action::OpenTab { dest };
        assert_eq!(
            agent(Relation::SelfPane, open(Dest::OwnTeam), false),
            "allow"
        );
        assert_eq!(
            agent(Relation::SelfPane, open(Dest::PlainOrNewOrTop), false),
            "non_user_turn"
        );
        assert_eq!(
            agent(Relation::SelfPane, open(Dest::PlainOrNewOrTop), true),
            "allow"
        );
        assert_eq!(
            agent(Relation::SelfPane, open(Dest::OtherTeam), true),
            "outside_team"
        );
        // Without a team, "own team" is not a team.
        assert_eq!(
            code(authorize(
                Actor::Agent { team: false },
                Relation::SelfPane,
                open(Dest::OwnTeam),
                &facts(false)
            )),
            "non_user_turn"
        );
    }

    #[test]
    fn reopen_and_activate_of_a_users_entry_need_the_user() {
        let reopen = |by_agent, dest| Action::SoftEdit(SoftEdit::Reopen { by_agent, dest });
        assert_eq!(
            agent(Relation::SelfPane, reopen(true, Dest::OwnTeam), false),
            "allow"
        );
        assert_eq!(
            agent(Relation::SelfPane, reopen(false, Dest::OwnTeam), false),
            "non_user_turn"
        );
        assert_eq!(
            agent(Relation::SelfPane, reopen(false, Dest::OwnTeam), true),
            "allow"
        );
        assert_eq!(
            agent(
                Relation::SelfPane,
                reopen(true, Dest::PlainOrNewOrTop),
                false
            ),
            "non_user_turn"
        );
        assert_eq!(
            agent(Relation::SelfPane, reopen(true, Dest::OtherTeam), true),
            "outside_team"
        );
        let activate = |by_agent| Action::SoftEdit(SoftEdit::Activate { by_agent });
        assert_eq!(agent(Relation::Teammate, activate(true), false), "allow");
        assert_eq!(
            agent(Relation::Teammate, activate(false), false),
            "non_user_turn"
        );
        assert_eq!(agent(Relation::Teammate, activate(false), true), "allow");
        assert_eq!(agent(Relation::Other, activate(true), true), "outside_team");
        assert_eq!(agent(Relation::SelfPane, activate(true), false), "allow");
        assert_eq!(
            agent(Relation::Protected, activate(true), true),
            "protected_tab"
        );
        // Suspend and restart: a plain soft edit (self and teammates free).
        let lifecycle = Action::SoftEdit(SoftEdit::SuspendRestart);
        assert_eq!(agent(Relation::SelfPane, lifecycle, false), "allow");
        assert_eq!(agent(Relation::Teammate, lifecycle, false), "allow");
        assert_eq!(agent(Relation::Other, lifecycle, true), "outside_team");
        assert_eq!(agent(Relation::Protected, lifecycle, true), "protected_tab");
    }

    #[test]
    fn own_only_writes_stay_with_the_owner() {
        let write = Action::OwnOnly(OwnOnly::NotesWrite);
        assert_eq!(agent(Relation::SelfPane, write, false), "allow");
        for relation in [Relation::Teammate, Relation::Other, Relation::Protected] {
            assert_eq!(agent(relation, write, true), "own_only");
        }
    }

    #[test]
    fn closing_needs_the_users_turn_and_the_team() {
        assert_eq!(
            agent(Relation::SelfPane, Action::Close, false),
            "non_user_turn"
        );
        assert_eq!(agent(Relation::SelfPane, Action::Close, true), "allow");
        assert_eq!(
            agent(Relation::Teammate, Action::Close, false),
            "non_user_turn"
        );
        assert_eq!(agent(Relation::Teammate, Action::Close, true), "allow");
        assert_eq!(agent(Relation::Other, Action::Close, true), "outside_team");
        assert_eq!(
            agent(Relation::Protected, Action::Close, true),
            "protected_tab"
        );
    }

    #[test]
    fn team_structure_needs_the_users_turn() {
        for op in [TeamOp::Make, TeamOp::Purpose] {
            let action = Action::TeamStructure(op);
            assert_eq!(agent(Relation::SelfPane, action, false), "non_user_turn");
            assert_eq!(agent(Relation::SelfPane, action, true), "allow");
            assert_eq!(agent(Relation::Other, action, true), "outside_team");
        }
    }

    #[test]
    fn tabs_reorder_inside_the_team_and_groups_only_for_the_user() {
        let reorder = Action::ReorderTab;
        assert_eq!(agent(Relation::SelfPane, reorder, false), "allow");
        assert_eq!(agent(Relation::Teammate, reorder, false), "allow");
        assert_eq!(agent(Relation::Other, reorder, true), "outside_team");
        assert_eq!(agent(Relation::Protected, reorder, true), "protected_tab");
        // Without a team even its own tab: it moves the group's other tabs.
        for relation in [Relation::SelfPane, Relation::Teammate] {
            assert_eq!(
                code(authorize(
                    Actor::Agent { team: false },
                    relation,
                    reorder,
                    &facts(true)
                )),
                "outside_team"
            );
        }
        for relation in RELATIONS {
            for actor in [Actor::Agent { team: true }, Actor::Agent { team: false }] {
                assert_eq!(
                    code(authorize(
                        actor,
                        relation,
                        Action::ReorderGroup,
                        &facts(true)
                    )),
                    "coordinator_only"
                );
            }
        }
        assert_eq!(
            code(authorize(
                Actor::Coordinator,
                Relation::Other,
                Action::ReorderGroup,
                &facts(false)
            )),
            "non_user_turn"
        );
        assert!(authorize(
            Actor::Coordinator,
            Relation::Other,
            Action::ReorderGroup,
            &facts(true)
        )
        .is_allowed());
        assert!(authorize(
            Actor::User,
            Relation::Other,
            Action::ReorderGroup,
            &facts(false)
        )
        .is_allowed());
    }

    #[test]
    fn denials_carry_one_actionable_hint() {
        let mut plain = facts(true);
        plain.own_plain_group = Some("search it".into());
        let Decision::Deny { code, hint } = authorize(
            Actor::Agent { team: false },
            Relation::Other,
            Action::Close,
            &plain,
        ) else {
            panic!("allowed");
        };
        assert_eq!(code, "outside_team");
        assert!(hint.starts_with("you can read and message api;"), "{hint}");
        assert!(hint.ends_with("Make team on search it"), "{hint}");
        let Decision::Deny { hint, .. } = authorize(
            Actor::Agent { team: true },
            Relation::Teammate,
            Action::Close,
            &facts(false),
        ) else {
            panic!("allowed");
        };
        assert!(
            hint.contains("from a teammate's message, not your user"),
            "{hint}"
        );
    }
}
