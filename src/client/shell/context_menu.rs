use super::*;

impl ClientContextMenuOverlay {
    pub(super) fn items(&self) -> Vec<ClientContextMenuItem> {
        use ClientContextMenuAction as Action;

        let item = |label, action| ClientContextMenuItem { label, action };
        match &self.target {
            ClientContextMenuTarget::Workspace { is_git: false, .. } => {
                vec![item("Rename", Action::Rename), item("Close", Action::Close)]
            }
            ClientContextMenuTarget::Workspace {
                is_linked_worktree: false,
                has_worktree_children: false,
                ..
            } => vec![
                item("Rename", Action::Rename),
                item("Close", Action::Close),
                item("New worktree", Action::NewWorktree),
                item("Open worktree...", Action::OpenWorktree),
            ],
            ClientContextMenuTarget::Workspace {
                is_linked_worktree: true,
                ..
            } => vec![
                item("Rename", Action::Rename),
                item("Close", Action::Close),
                item("Delete worktree checkout...", Action::RemoveWorktree),
            ],
            ClientContextMenuTarget::Workspace {
                has_worktree_children: true,
                collapsed,
                ..
            } => vec![
                item("Rename", Action::Rename),
                item("Close group", Action::Close),
                item("New worktree", Action::NewWorktree),
                item("Open worktree...", Action::OpenWorktree),
                item(
                    if *collapsed { "Expand" } else { "Collapse" },
                    Action::ToggleGroup,
                ),
            ],
            ClientContextMenuTarget::Tab {
                agent,
                important,
                team,
                session_id,
                ..
            } => {
                let mut items = vec![
                    item("New tab", Action::NewTab),
                    item("Rename", Action::Rename),
                ];
                match agent {
                    Some(ClientTabMenuAgent {
                        suspended: true, ..
                    }) => items.push(item("Activate agent", Action::ActivateAgent)),
                    Some(ClientTabMenuAgent {
                        suspended: false, ..
                    }) => {
                        items.push(item("Suspend agent", Action::SuspendAgent));
                        items.push(item("Restart agent", Action::RestartAgent));
                    }
                    None => {}
                }
                // Fork: after the agent items, only once the agent's session
                // id arrived (context_menu_session.rs).
                if session_id.is_some() {
                    items.push(item("Copy session ID", Action::CopySessionId));
                }
                items.push(item("Close", Action::Close));
                // Fork: the role and team items, after Close so upstream's
                // positions hold. "Set role…" is on any agent tab of a server
                // with `agents.set_meta`; without it a member keeps "Set team
                // role…" and a non-member gets no role item.
                let set_role = agent.as_ref().is_some_and(|agent| agent.set_role);
                if set_role {
                    items.push(item("Set role…", Action::SetRole));
                }
                match team {
                    Some(team) if team.member => {
                        if !set_role {
                            items.push(item("Set team role…", Action::SetTeamRole));
                        }
                        items.push(item("Leave team", Action::LeaveTeam));
                    }
                    Some(_) => items.push(item("Join team", Action::JoinTeam)),
                    None => {}
                }
                // Fork: the tab's info dock, as on the pane menu.
                items.push(item("Info pane", Action::ToggleInfoPane));
                // After Close, so upstream's item positions hold: the
                // important toggle, then the two selector rows, whose labels
                // only name them (render_context_menu draws the options).
                items.push(item(
                    if *important {
                        "Important \u{2713}"
                    } else {
                        "Important"
                    },
                    Action::Important,
                ));
                items.push(item("remind", Action::RemindTop));
                items.push(item("", Action::RemindBottom));
                // The swatch row, last so upstream's item positions (Close at
                // 2 without an agent) stay put. The label only names the row;
                // render_context_menu draws the swatches in its place.
                items.push(item("Color", Action::Color));
                items
            }
            ClientContextMenuTarget::Group { team, .. } => {
                use super::teams::ClientGroupTeamMenu;
                let team_group = *team == Some(ClientGroupTeamMenu::Team);
                let mut items = vec![
                    item("Rename group", Action::Rename),
                    item(
                        if team_group {
                            "Ungroup (disbands team)"
                        } else {
                            "Ungroup"
                        },
                        Action::Ungroup,
                    ),
                    item("Close group", Action::CloseGroup),
                ];
                // Fork: the team items, after upstream's.
                match team {
                    Some(ClientGroupTeamMenu::NotTeam) => {
                        items.push(item("Make team…", Action::MakeTeam));
                    }
                    Some(ClientGroupTeamMenu::Team) => items.extend([
                        item("Team info", Action::TeamInfo),
                        item("Edit purpose…", Action::EditPurpose),
                        item("Disband team", Action::DisbandTeam),
                    ]),
                    None => {}
                }
                items
            }
            ClientContextMenuTarget::News { enabled, .. } => vec![
                item("Run now", Action::NewsRun),
                item("Open", Action::NewsOpen),
                item(
                    if *enabled {
                        "Pause schedule"
                    } else {
                        "Resume schedule"
                    },
                    Action::NewsToggleSchedule,
                ),
            ],
            ClientContextMenuTarget::Coordinator { items } => {
                super::coordinator_shell::coordinator_menu_items(items)
            }
            ClientContextMenuTarget::Browser { running, local, .. } => {
                let mut items = Vec::new();
                if *local {
                    if *running {
                        items.push(item("Focus window", Action::BrowserFocusWindow));
                        items.push(item("Stop profile", Action::BrowserToggleProfile));
                    } else {
                        items.push(item("Start profile", Action::BrowserToggleProfile));
                    }
                }
                items.push(item("Open overlay", Action::BrowserOpenOverlay));
                items
            }
            ClientContextMenuTarget::Pane {
                source_pane_id,
                has_manual_label,
                right_click_passthrough,
                session_id,
                ..
            } => {
                let mut items = vec![item("Rename pane", Action::RenamePane)];
                if *has_manual_label {
                    items.push(item("Clear pane name", Action::ClearPaneName));
                }
                if source_pane_id.is_some() {
                    items.push(item("Swap with focused pane", Action::SwapWithFocusedPane));
                }
                items.extend([
                    item("Split right", Action::SplitRight),
                    item("Split down", Action::SplitDown),
                    item("Zoom", Action::Zoom),
                    item(
                        if *right_click_passthrough {
                            "Use Herdr right-click menu"
                        } else {
                            "Send right-clicks to pane"
                        },
                        Action::ToggleRightClickPassthrough,
                    ),
                    item("Close pane", Action::ClosePane),
                ]);
                // Fork: the pane's tab's info dock.
                items.push(item("Info pane", Action::ToggleInfoPane));
                // Fork: once the pane agent's session id arrived.
                if session_id.is_some() {
                    items.push(item("Copy session ID", Action::CopySessionId));
                }
                items
            }
        }
    }
}

impl ClientShellState {
    pub(super) fn open_workspace_context_menu(&mut self, workspace_id: String, x: u16, y: u16) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(workspace) = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == workspace_id)
        else {
            return;
        };
        let worktree = workspace.worktree.as_ref();
        let has_worktree_children = worktree.is_some_and(|worktree| {
            !worktree.is_linked_worktree
                && snapshot
                    .workspaces
                    .iter()
                    .filter(|candidate| {
                        candidate
                            .worktree
                            .as_ref()
                            .is_some_and(|candidate| candidate.key == worktree.key)
                    })
                    .count()
                    >= 2
        });
        let collapsed = worktree.is_some_and(|worktree| {
            self.group_is_collapsed(&self.active_endpoint_id, &worktree.key)
        });
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Workspace {
                workspace_id,
                is_git: worktree.is_some() || workspace.branch.is_some(),
                is_linked_worktree: worktree.is_some_and(|worktree| worktree.is_linked_worktree),
                has_worktree_children,
                collapsed,
            },
            x,
            y,
            highlighted: 0,
        }));
    }

    pub(super) fn open_tab_context_menu(&mut self, tab_id: String, x: u16, y: u16) {
        let Some(tab) = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id))
        else {
            return;
        };
        // The coordinator's role is herdr's, never set from the menu.
        let set_role =
            !self.coordinator.is_coordinator_tab(&tab_id) && self.agents_set_meta_supported();
        let agent = self.snapshot.as_deref().and_then(|snapshot| {
            snapshot
                .agents
                .iter()
                .find(|agent| agent.tab_id == tab_id)
                .map(|agent| ClientTabMenuAgent {
                    pane_id: agent.pane_id.clone(),
                    suspended: agent.agent_status == crate::api::schema::AgentStatus::Suspended,
                    set_role,
                })
        });
        let color = super::tab_color::tab_menu_color(tab.color);
        let team = self.tab_team_menu(&tab.workspace_id, agent.as_ref());
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Tab {
                tab_id,
                workspace_id: tab.workspace_id.clone(),
                agent,
                color,
                important: tab.important,
                remind: super::tab_remind_menu::tab_menu_remind(tab.remind_every),
                team,
                session_id: None,
            },
            x,
            y,
            highlighted: 0,
        }));
    }

    pub(super) fn open_pane_context_menu(&mut self, pane_id: String, x: u16, y: u16) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(pane) = snapshot.panes.iter().find(|pane| pane.pane_id == pane_id) else {
            return;
        };
        let source_pane_id = snapshot
            .focused_pane_id
            .clone()
            .filter(|focused| focused != &pane_id);
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Pane {
                pane_id,
                workspace_id: pane.workspace_id.clone(),
                source_pane_id,
                has_manual_label: pane.label.is_some(),
                right_click_passthrough: pane.right_click_passthrough,
                session_id: None,
            },
            x,
            y,
            highlighted: 0,
        }));
    }

    pub(super) fn move_context_menu_selection(&mut self, delta: isize) {
        let Some(ClientShellOverlay::ContextMenu(menu)) = self.overlay.as_mut() else {
            return;
        };
        let item_count = menu.items().len();
        if item_count == 0 {
            return;
        }
        menu.highlighted = (menu.highlighted as isize + delta)
            .clamp(0, item_count.saturating_sub(1) as isize) as usize;
    }

    pub(super) fn activate_context_menu_item(
        &mut self,
        index: usize,
        outcome: &mut ClientShellInput,
    ) {
        let Some(ClientShellOverlay::ContextMenu(menu)) = self.overlay.take() else {
            return;
        };
        let Some(action) = menu.items().get(index).map(|item| item.action) else {
            outcome.repaint = true;
            return;
        };
        if let (
            ClientContextMenuAction::Color,
            ClientContextMenuTarget::Tab { tab_id, color, .. },
        ) = (action, &menu.target)
        {
            // Picking a swatch sets the color without focusing the tab.
            self.pick_tab_color_swatch(tab_id.clone(), color.cursor, outcome);
            outcome.repaint = true;
            return;
        }
        match (action, &menu.target) {
            // Like the swatch row, these do not focus the tab: focusing would
            // mark its agent seen and cancel the reminder being armed.
            (
                ClientContextMenuAction::Important,
                ClientContextMenuTarget::Tab {
                    tab_id, important, ..
                },
            ) => {
                self.push_endpoint_method(
                    crate::api::schema::Method::TabSetReminder(
                        crate::api::schema::TabSetReminderParams {
                            tab_id: tab_id.clone(),
                            important: Some(!*important),
                            every: None,
                        },
                    ),
                    outcome,
                );
                outcome.repaint = true;
                return;
            }
            (
                ClientContextMenuAction::RemindTop | ClientContextMenuAction::RemindBottom,
                ClientContextMenuTarget::Tab { tab_id, remind, .. },
            ) => {
                self.pick_tab_remind_option(tab_id.clone(), *remind, outcome);
                outcome.repaint = true;
                return;
            }
            // Fork: copying the agent's session id focuses nothing.
            (
                ClientContextMenuAction::CopySessionId,
                ClientContextMenuTarget::Tab { session_id, .. }
                | ClientContextMenuTarget::Pane { session_id, .. },
            ) => {
                if let Some(session_id) = session_id.clone() {
                    self.copy_context_menu_session_id(session_id, outcome);
                }
                outcome.repaint = true;
                return;
            }
            // Fork: the tab menu's info dock item, like the pane menu's,
            // does not focus the tab (a background tab opens in state only).
            (
                ClientContextMenuAction::ToggleInfoPane,
                ClientContextMenuTarget::Tab { tab_id, .. },
            ) => {
                self.toggle_info_pane(tab_id.clone(), outcome);
                outcome.repaint = true;
                return;
            }
            // Fork: "Set role…" opens the role prompt without focusing the
            // tab, like the team items.
            (
                ClientContextMenuAction::SetRole,
                ClientContextMenuTarget::Tab {
                    agent: Some(agent),
                    team,
                    ..
                },
            ) => {
                self.open_agent_role_rename(agent.pane_id.clone(), team.as_ref());
                outcome.repaint = true;
                return;
            }
            // Fork: the pane menu's info dock item acts on the pane's tab
            // without focusing it (a background tab is opened in state only).
            (
                ClientContextMenuAction::ToggleInfoPane,
                ClientContextMenuTarget::Pane { pane_id, .. },
            ) => {
                let tab_id = self.snapshot.as_deref().and_then(|snapshot| {
                    snapshot
                        .panes
                        .iter()
                        .find(|pane| &pane.pane_id == pane_id)
                        .map(|pane| pane.tab_id.clone())
                });
                if let Some(tab_id) = tab_id {
                    self.toggle_info_pane(tab_id, outcome);
                }
                outcome.repaint = true;
                return;
            }
            _ => {}
        }
        match menu.target {
            ClientContextMenuTarget::Workspace { workspace_id, .. } => {
                self.activate_workspace_context_action(workspace_id, action, outcome)
            }
            ClientContextMenuTarget::Group { workspace_id, team } => {
                self.activate_group_context_action(workspace_id, team, action, outcome)
            }
            ClientContextMenuTarget::News { enabled, .. } => {
                self.activate_news_context_action(enabled, action, outcome)
            }
            ClientContextMenuTarget::Browser {
                profile, running, ..
            } => self.activate_browser_context_action(profile, running, action, outcome),
            ClientContextMenuTarget::Coordinator { .. } => {
                self.activate_coordinator_context_action(action, outcome)
            }
            ClientContextMenuTarget::Tab {
                team: Some(team), ..
            } if matches!(
                action,
                ClientContextMenuAction::SetTeamRole
                    | ClientContextMenuAction::LeaveTeam
                    | ClientContextMenuAction::JoinTeam
            ) =>
            {
                // Like the swatch row, these do not focus the tab.
                self.activate_tab_team_action(team, action, outcome)
            }
            ClientContextMenuTarget::Tab {
                tab_id,
                workspace_id,
                agent,
                ..
            } => self.activate_tab_context_action(tab_id, workspace_id, agent, action, outcome),
            ClientContextMenuTarget::Pane {
                pane_id,
                workspace_id,
                source_pane_id,
                right_click_passthrough,
                ..
            } => self.activate_pane_context_action(
                pane_id,
                workspace_id,
                source_pane_id,
                right_click_passthrough,
                action,
                outcome,
            ),
        }
        outcome.repaint = true;
    }

    fn activate_workspace_context_action(
        &mut self,
        workspace_id: String,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::input::KeybindAction;

        match action {
            ClientContextMenuAction::Rename => {
                let label = self
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| {
                        snapshot
                            .workspaces
                            .iter()
                            .find(|workspace| workspace.workspace_id == workspace_id)
                    })
                    .map(|workspace| workspace.label.clone());
                if let Some(label) = label {
                    self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                        title: "rename workspace",
                        input: TextEditor::new(&label, false),
                        target: ClientRenameTarget::Workspace { workspace_id },
                    }));
                }
            }
            ClientContextMenuAction::Close => {
                if self.config.confirm_close {
                    self.open_confirm_close_overlay(workspace_id);
                } else {
                    self.push_endpoint_method(
                        crate::api::schema::Method::WorkspaceClose(
                            crate::api::schema::WorkspaceCloseParams {
                                workspace_id,
                                close_group: true,
                            },
                        ),
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::NewWorktree => {
                self.begin_worktree_action_for(KeybindAction::NewWorktree, workspace_id, outcome)
            }
            ClientContextMenuAction::OpenWorktree => {
                self.begin_worktree_action_for(KeybindAction::OpenWorktree, workspace_id, outcome)
            }
            ClientContextMenuAction::RemoveWorktree => {
                self.begin_worktree_action_for(KeybindAction::RemoveWorktree, workspace_id, outcome)
            }
            ClientContextMenuAction::ToggleGroup => {
                let key = self.snapshot.as_deref().and_then(|snapshot| {
                    snapshot
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.workspace_id == workspace_id)
                        .and_then(|workspace| workspace.worktree.as_ref())
                        .map(|worktree| worktree.key.clone())
                });
                if let Some(key) = key {
                    let endpoint_id = self.active_endpoint_id.clone();
                    self.toggle_collapsed_group(&endpoint_id, key);
                    self.persist_chrome_preferences(outcome);
                }
            }
            _ => {}
        }
    }

    fn activate_tab_context_action(
        &mut self,
        tab_id: String,
        workspace_id: String,
        agent: Option<ClientTabMenuAgent>,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::api::schema::{Method, TabTarget};

        self.push_endpoint_method(
            Method::TabFocus(TabTarget {
                tab_id: tab_id.clone(),
            }),
            outcome,
        );
        match action {
            ClientContextMenuAction::NewTab => {
                if self.config.prompt_new_tab_name {
                    let default_name = (self
                        .snapshot
                        .as_deref()
                        .map(|snapshot| {
                            snapshot
                                .tabs
                                .iter()
                                .filter(|tab| tab.workspace_id == workspace_id)
                                .count()
                        })
                        .unwrap_or(0)
                        + 1)
                    .to_string();
                    self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                        title: "new tab",
                        input: TextEditor::new(&default_name, true),
                        target: ClientRenameTarget::NewTab {
                            workspace_id,
                            default_name,
                        },
                    }));
                } else {
                    self.push_endpoint_method(
                        Method::TabCreate(crate::api::schema::TabCreateParams {
                            workspace_id: Some(workspace_id),
                            cwd: None,
                            focus: true,
                            label: None,
                            env: Default::default(),
                        }),
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::Rename => {
                let tab = self
                    .snapshot
                    .as_deref()
                    .and_then(|snapshot| snapshot.tabs.iter().find(|tab| tab.tab_id == tab_id));
                if let Some(tab) = tab {
                    self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                        title: "rename tab",
                        input: TextEditor::new(&tab.label, false),
                        target: ClientRenameTarget::Tab {
                            tab_id,
                            auto_name: !tab.custom_label,
                            original_name: tab.label.clone(),
                        },
                    }));
                }
            }
            ClientContextMenuAction::Close => {
                self.request_tab_close(tab_id, outcome);
            }
            ClientContextMenuAction::SuspendAgent | ClientContextMenuAction::ActivateAgent => {
                // Act on the pane the menu described when it opened, not whatever
                // agent the tab holds now.
                if let Some(pane_id) = agent.map(|agent| agent.pane_id) {
                    let method = if action == ClientContextMenuAction::SuspendAgent {
                        Method::AgentSuspend(crate::api::schema::AgentSuspendParams {
                            target: pane_id,
                        })
                    } else {
                        Method::AgentActivate(crate::api::schema::AgentActivateParams {
                            target: pane_id,
                        })
                    };
                    self.push_endpoint_method(method, outcome);
                }
            }
            ClientContextMenuAction::RestartAgent => {
                if let Some(pane_id) = agent.map(|agent| agent.pane_id) {
                    self.push_endpoint_method(
                        Method::AgentRestart(crate::api::schema::AgentRestartParams {
                            target: pane_id,
                        }),
                        outcome,
                    );
                }
            }
            _ => {}
        }
    }

    fn activate_pane_context_action(
        &mut self,
        pane_id: String,
        workspace_id: String,
        source_pane_id: Option<String>,
        right_click_passthrough: bool,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        use crate::api::schema::{
            Method, PaneInputSetParams, PaneRenameParams, PaneRightClickTarget, PaneSplitParams,
            PaneSwapParams, PaneTarget, PaneZoomMode, PaneZoomParams, SplitDirection,
        };

        match action {
            ClientContextMenuAction::RenamePane => {
                let label = self.snapshot.as_deref().and_then(|snapshot| {
                    snapshot
                        .panes
                        .iter()
                        .find(|pane| pane.pane_id == pane_id)
                        .and_then(|pane| pane.label.clone())
                });
                self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                    title: "rename pane",
                    input: TextEditor::new(label.as_deref().unwrap_or_default(), label.is_none()),
                    target: ClientRenameTarget::Pane { pane_id },
                }));
            }
            ClientContextMenuAction::ClearPaneName => self.push_endpoint_method(
                Method::PaneRename(PaneRenameParams {
                    pane_id,
                    label: None,
                }),
                outcome,
            ),
            ClientContextMenuAction::SwapWithFocusedPane => {
                if let Some(source_pane_id) = source_pane_id {
                    self.push_endpoint_method(
                        Method::PaneSwap(PaneSwapParams {
                            pane_id: None,
                            direction: None,
                            source_pane_id: Some(source_pane_id.clone()),
                            target_pane_id: Some(pane_id),
                        }),
                        outcome,
                    );
                    self.push_endpoint_method(
                        Method::PaneFocus(PaneTarget {
                            pane_id: source_pane_id,
                        }),
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::SplitRight | ClientContextMenuAction::SplitDown => {
                self.push_endpoint_method(
                    Method::PaneSplit(PaneSplitParams {
                        workspace_id: Some(workspace_id),
                        target_pane_id: Some(pane_id),
                        direction: if action == ClientContextMenuAction::SplitRight {
                            SplitDirection::Right
                        } else {
                            SplitDirection::Down
                        },
                        ratio: None,
                        cwd: None,
                        focus: true,
                        right_click: Default::default(),
                        env: Default::default(),
                    }),
                    outcome,
                );
            }
            ClientContextMenuAction::Zoom => self.push_endpoint_method(
                Method::PaneZoom(PaneZoomParams {
                    pane_id: Some(pane_id),
                    mode: PaneZoomMode::Toggle,
                }),
                outcome,
            ),
            ClientContextMenuAction::ToggleRightClickPassthrough => self.push_endpoint_method(
                Method::PaneInputSet(PaneInputSetParams {
                    pane_id,
                    right_click: if right_click_passthrough {
                        PaneRightClickTarget::Herdr
                    } else {
                        PaneRightClickTarget::Pane
                    },
                }),
                outcome,
            ),
            ClientContextMenuAction::ClosePane => {
                self.push_endpoint_method(Method::PaneClose(PaneTarget { pane_id }), outcome)
            }
            _ => {}
        }
    }
}

impl ClientShellState {
    /// `tabs` layout: right-click on a group header.
    pub(super) fn open_group_context_menu(&mut self, workspace_id: String, x: u16, y: u16) {
        let team = self.group_team_menu(&workspace_id);
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Group { workspace_id, team },
            x,
            y,
            highlighted: 0,
        }));
    }

    fn activate_group_context_action(
        &mut self,
        workspace_id: String,
        team: Option<super::teams::ClientGroupTeamMenu>,
        action: ClientContextMenuAction,
        outcome: &mut ClientShellInput,
    ) {
        if team == Some(super::teams::ClientGroupTeamMenu::Team)
            && action == ClientContextMenuAction::Ungroup
            && self.config.confirm_close
        {
            // Ungrouping a team group disbands the team: confirmed like
            // Close group (the same `confirm_close` setting).
            self.open_ungroup_team_confirmation(workspace_id);
            return;
        }
        match action {
            ClientContextMenuAction::Rename => {
                let label = self.snapshot.as_deref().and_then(|snapshot| {
                    snapshot
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.workspace_id == workspace_id)
                        .map(|workspace| workspace.label.clone())
                });
                if let Some(label) = label {
                    self.overlay = Some(ClientShellOverlay::Rename(ClientRenameOverlay {
                        title: "rename group",
                        input: TextEditor::new(&label, false),
                        target: ClientRenameTarget::Workspace { workspace_id },
                    }));
                }
            }
            ClientContextMenuAction::Ungroup => self.ungroup(workspace_id, outcome),
            ClientContextMenuAction::CloseGroup => {
                if self.config.confirm_close {
                    self.open_close_group_confirmation(workspace_id);
                } else {
                    self.push_endpoint_method(
                        crate::api::schema::Method::WorkspaceClose(
                            crate::api::schema::WorkspaceCloseParams {
                                workspace_id,
                                close_group: false,
                            },
                        ),
                        outcome,
                    );
                }
            }
            ClientContextMenuAction::MakeTeam => {
                self.open_team_purpose_rename(workspace_id, "", true, false);
            }
            ClientContextMenuAction::TeamInfo => self.open_team_overlay(workspace_id),
            ClientContextMenuAction::EditPurpose => {
                let purpose = self
                    .active_team(&workspace_id)
                    .and_then(|team| team.purpose.clone())
                    .unwrap_or_default();
                self.open_team_purpose_rename(workspace_id, &purpose, false, false);
            }
            ClientContextMenuAction::DisbandTeam => {
                // No confirmation: nothing is closed, the agents keep running.
                self.push_team_request(
                    super::teams::TeamRequest::Disband { workspace_id },
                    outcome,
                );
            }
            _ => {}
        }
    }

    /// Ungroup: every member goes to the ungrouped bucket (the first space);
    /// the emptied space is removed by the server (and a team with it). All
    /// or nothing: a member that cannot move (several panes) refuses the
    /// whole ungroup.
    pub(super) fn ungroup(&mut self, workspace_id: String, outcome: &mut ClientShellInput) {
        let (first, member_ids) = match self.snapshot.as_deref() {
            Some(snapshot) => {
                let Some(first) = snapshot.workspaces.first() else {
                    return;
                };
                if first.workspace_id == workspace_id {
                    return;
                }
                (
                    first.workspace_id.clone(),
                    snapshot
                        .tabs
                        .iter()
                        .filter(|tab| tab.workspace_id == workspace_id)
                        .map(|tab| tab.tab_id.clone())
                        .collect::<Vec<_>>(),
                )
            }
            None => return,
        };
        if let Some(reason) = member_ids
            .iter()
            .find_map(|tab_id| self.tab_group_move_blocker(tab_id))
        {
            self.notify_group_move_refused(reason);
            outcome.repaint = true;
            return;
        }
        let moves = member_ids
            .iter()
            .filter_map(|tab_id| self.move_tab_to_workspace_method(tab_id, &first, false))
            .collect::<Vec<_>>();
        for method in moves {
            self.push_endpoint_method(method, outcome);
        }
    }
}
