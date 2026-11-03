use super::*;

impl ClientContextMenuOverlay {
    /// Fork (menu design): the items grouped into sections with a glyph and
    /// a kind each (`context_menu_look.rs` draws them); positions are free.
    pub(super) fn items(&self) -> Vec<ClientContextMenuItem> {
        use super::context_menu_look::{MenuKind as Kind, MenuLook, MenuSection as Section};
        use ClientContextMenuAction as Action;

        let item = |label: &'static str, action, glyph, section, kind| ClientContextMenuItem {
            label: std::borrow::Cow::Borrowed(label),
            action,
            look: MenuLook::new(glyph, section, kind),
        };
        let lead = |label, action, glyph| item(label, action, glyph, Section::Lead, Kind::Action);
        let worktree = Section::Titled("Worktree");
        match &self.target {
            ClientContextMenuTarget::Workspace {
                is_git,
                is_linked_worktree,
                has_worktree_children,
                close_group,
                collapsed,
                ..
            } => {
                let mut items = vec![lead("Rename", Action::Rename, GLYPH_RENAME)];
                if *is_git && !*is_linked_worktree {
                    items.push(item(
                        "New worktree",
                        Action::NewWorktree,
                        GLYPH_NEW,
                        worktree,
                        Kind::Action,
                    ));
                    items.push(item(
                        "Open worktree...",
                        Action::OpenWorktree,
                        GLYPH_WORKTREE,
                        worktree,
                        Kind::Action,
                    ));
                    if *has_worktree_children {
                        items.push(item(
                            if *collapsed { "Expand" } else { "Collapse" },
                            Action::ToggleGroup,
                            if *collapsed {
                                GLYPH_EXPAND
                            } else {
                                GLYPH_COLLAPSE
                            },
                            worktree,
                            Kind::Action,
                        ));
                    }
                }
                items.push(item(
                    if *has_worktree_children && *close_group {
                        "Close group"
                    } else {
                        "Close"
                    },
                    Action::Close,
                    GLYPH_CLOSE,
                    Section::Last,
                    Kind::Danger,
                ));
                if *is_git && *is_linked_worktree {
                    items.push(item(
                        "Delete worktree checkout...",
                        Action::RemoveWorktree,
                        GLYPH_DELETE,
                        Section::Last,
                        Kind::Danger,
                    ));
                }
                items
            }
            ClientContextMenuTarget::Tab {
                agent,
                important,
                team,
                session_id,
                pinned,
                pin_supported,
                muted,
                mute_supported,
                dnd,
                dnd_supported,
                browser_page,
                browser_dock,
                ..
            } => {
                let agent_section = Section::Titled("Agent");
                let team_section = Section::Titled("Team");
                let alerts = Section::Titled("Alerts");
                let agent_item =
                    |label, action, glyph| item(label, action, glyph, agent_section, Kind::Action);
                // The everyday four lead; Close is red but away from the
                // cursor's start.
                let mut items = vec![
                    lead("New tab", Action::NewTab, GLYPH_NEW),
                    lead("Rename", Action::Rename, GLYPH_RENAME),
                    // Fork: the tab's info dock, as on the pane menu.
                    lead("Info pane", Action::ToggleInfoPane, GLYPH_INFO),
                    item(
                        "Close",
                        Action::Close,
                        GLYPH_CLOSE,
                        Section::Lead,
                        Kind::Danger,
                    ),
                ];
                // Fork (companion browser): the session's page title; a
                // click opens the Browser overlay.
                if let Some((title, needs_you)) = browser_page {
                    items.push(ClientContextMenuItem {
                        label: std::borrow::Cow::Owned(title.clone()),
                        action: Action::BrowserOpenOverlay,
                        look: MenuLook::new(
                            if *needs_you { "\u{25C9}" } else { "\u{25CE}" },
                            Section::Titled("Browser"),
                            Kind::Action,
                        ),
                    });
                }
                // Fork (companion launcher): the tab's browser dock and
                // Browser view, where the browser docks.
                if let Some((docked, viewing)) = browser_dock {
                    let browser = Section::Titled("Browser");
                    items.push(item(
                        "Dock browser",
                        Action::BrowserToggleDock,
                        if *docked { GLYPH_ON } else { GLYPH_OFF },
                        browser,
                        Kind::Toggle(*docked),
                    ));
                    items.push(item(
                        "Browser view",
                        Action::BrowserToggleView,
                        if *viewing { GLYPH_ON } else { GLYPH_OFF },
                        browser,
                        Kind::Toggle(*viewing),
                    ));
                }
                match agent {
                    Some(ClientTabMenuAgent {
                        suspended: true, ..
                    }) => items.push(agent_item(
                        "Activate agent",
                        Action::ActivateAgent,
                        GLYPH_ACTIVATE,
                    )),
                    Some(ClientTabMenuAgent {
                        suspended: false,
                        handoff,
                        ..
                    }) => {
                        items.push(agent_item(
                            "Suspend agent",
                            Action::SuspendAgent,
                            GLYPH_SUSPEND,
                        ));
                        items.push(agent_item(
                            "Restart agent",
                            Action::RestartAgent,
                            GLYPH_RESTART,
                        ));
                        // Fork (agent handoff): a live Claude agent of a
                        // server with `agents.handoff`.
                        if *handoff {
                            items.push(agent_item(
                                "Handoff & compact",
                                Action::HandoffCompact,
                                GLYPH_HANDOFF,
                            ));
                        }
                    }
                    None => {}
                }
                // Fork: once the agent's session id arrived
                // (context_menu_session.rs).
                if session_id.is_some() {
                    items.push(agent_item(
                        "Copy session ID",
                        Action::CopySessionId,
                        GLYPH_COPY,
                    ));
                }
                // Fork: the role and team items. "Set role…" is on any
                // agent tab of a server with `agents.set_meta`; without it a
                // member keeps "Set team role…" and a non-member gets no
                // role item.
                let set_role = agent.as_ref().is_some_and(|agent| agent.set_role);
                if set_role {
                    items.push(item(
                        "Set role…",
                        Action::SetRole,
                        GLYPH_ROLE,
                        team_section,
                        Kind::Action,
                    ));
                }
                match team {
                    Some(team) if team.member => {
                        if !set_role {
                            items.push(item(
                                "Set team role…",
                                Action::SetTeamRole,
                                GLYPH_ROLE,
                                team_section,
                                Kind::Action,
                            ));
                        }
                        items.push(item(
                            "Leave team",
                            Action::LeaveTeam,
                            GLYPH_LEAVE,
                            team_section,
                            Kind::Danger,
                        ));
                    }
                    Some(_) => items.push(item(
                        "Join team",
                        Action::JoinTeam,
                        GLYPH_JOIN,
                        team_section,
                        Kind::Action,
                    )),
                    None => {}
                }
                // The toggles keep one label; the dot (the star) shows the
                // state when the menu opened.
                items.push(item(
                    "Important",
                    Action::Important,
                    if *important {
                        GLYPH_STAR_ON
                    } else {
                        GLYPH_STAR_OFF
                    },
                    alerts,
                    Kind::Star(*important),
                ));
                let toggle = |label, action, on: bool| {
                    item(
                        label,
                        action,
                        if on { GLYPH_ON } else { GLYPH_OFF },
                        alerts,
                        Kind::Toggle(on),
                    )
                };
                // Fork (sidebar v3): when the server has `tab.set_pinned`.
                if *pin_supported {
                    items.push(toggle("Pin", Action::Pin, *pinned));
                }
                // Fork: when the server has `tab.set_muted`.
                if *mute_supported {
                    items.push(toggle("Mute notifications", Action::Mute, *muted));
                }
                // Fork: when the server has `tab.set_dnd`.
                if *dnd_supported {
                    items.push(toggle("Do not disturb", Action::Dnd, *dnd));
                }
                // The two selector rows and the swatch row: their labels
                // only name them (context_menu_look.rs draws the options).
                items.push(item(
                    "remind",
                    Action::RemindTop,
                    GLYPH_REMIND,
                    alerts,
                    Kind::Selector,
                ));
                items.push(item("", Action::RemindBottom, " ", alerts, Kind::Selector));
                items.push(item(
                    "Color",
                    Action::Color,
                    GLYPH_COLOR,
                    alerts,
                    Kind::Selector,
                ));
                items
            }
            ClientContextMenuTarget::Group { team, .. } => {
                use super::teams::ClientGroupTeamMenu;
                let team_group = *team == Some(ClientGroupTeamMenu::Team);
                let team_section = Section::Titled("Team");
                let mut items = vec![lead("Rename group", Action::Rename, GLYPH_RENAME)];
                if !team_group {
                    items.push(lead("Ungroup", Action::Ungroup, GLYPH_UNGROUP));
                }
                // Fork: the team items.
                match team {
                    Some(ClientGroupTeamMenu::NotTeam) => items.push(item(
                        "Make team…",
                        Action::MakeTeam,
                        GLYPH_TEAM,
                        team_section,
                        Kind::Action,
                    )),
                    Some(ClientGroupTeamMenu::Team) => items.extend([
                        item(
                            "Team info",
                            Action::TeamInfo,
                            GLYPH_INFO,
                            team_section,
                            Kind::Action,
                        ),
                        item(
                            "Edit purpose…",
                            Action::EditPurpose,
                            GLYPH_RENAME,
                            team_section,
                            Kind::Action,
                        ),
                    ]),
                    None => {}
                }
                if team_group {
                    items.push(item(
                        "Ungroup (disbands team)",
                        Action::Ungroup,
                        GLYPH_UNGROUP,
                        Section::Danger,
                        Kind::Danger,
                    ));
                }
                items.push(item(
                    "Close group",
                    Action::CloseGroup,
                    GLYPH_CLOSE,
                    Section::Danger,
                    Kind::Danger,
                ));
                if team_group {
                    items.push(item(
                        "Disband team",
                        Action::DisbandTeam,
                        GLYPH_DISBAND,
                        Section::Danger,
                        Kind::Danger,
                    ));
                }
                items
            }
            ClientContextMenuTarget::News { enabled, .. } => vec![
                lead("Run now", Action::NewsRun, GLYPH_ACTIVATE),
                lead("Open", Action::NewsOpen, GLYPH_OPEN),
                item(
                    "Run on schedule",
                    Action::NewsToggleSchedule,
                    if *enabled { GLYPH_ON } else { GLYPH_OFF },
                    Section::Lead,
                    Kind::Toggle(*enabled),
                ),
            ],
            ClientContextMenuTarget::Coordinator { items } => {
                super::coordinator_shell::coordinator_menu_items(items)
            }
            ClientContextMenuTarget::Browser {
                running,
                local,
                companion,
                window,
                dock,
                ..
            } => {
                let mut items = Vec::new();
                if *local {
                    // Fork (companion launcher): the companion's items lead;
                    // the herdr+ window's follow in their own section while
                    // it is on.
                    if let Some(state) = companion.as_deref() {
                        for (label, action, glyph) in super::browser::companion_menu_items(state) {
                            items.push(item(
                                label,
                                Action::BrowserCompanion(action),
                                glyph,
                                Section::Lead,
                                if action == crate::api::schema::BrowserCompanionAction::Stop {
                                    Kind::Danger
                                } else {
                                    Kind::Action
                                },
                            ));
                        }
                    }
                    // Fork (companion launcher): dock the browser on the
                    // focused tab (the row's `◨` does the same).
                    if let Some(docked) = dock {
                        items.push(item(
                            "Dock browser here",
                            Action::BrowserToggleDock,
                            if *docked { GLYPH_ON } else { GLYPH_OFF },
                            Section::Lead,
                            Kind::Toggle(*docked),
                        ));
                    }
                    if companion.is_none() || *window {
                        let section = if companion.is_some() {
                            Section::Titled("Window")
                        } else {
                            Section::Lead
                        };
                        if *running {
                            items.push(item(
                                "Focus window",
                                Action::BrowserFocusWindow,
                                GLYPH_FOCUS,
                                section,
                                Kind::Action,
                            ));
                            items.push(item(
                                "Stop profile",
                                Action::BrowserToggleProfile,
                                GLYPH_STOP,
                                section,
                                Kind::Action,
                            ));
                        } else {
                            items.push(item(
                                "Start profile",
                                Action::BrowserToggleProfile,
                                GLYPH_ACTIVATE,
                                section,
                                Kind::Action,
                            ));
                        }
                    }
                }
                items.push(lead("Open overlay", Action::BrowserOpenOverlay, GLYPH_OPEN));
                items
            }
            ClientContextMenuTarget::Pane {
                source_pane_id,
                has_manual_label,
                right_click_passthrough,
                session_id,
                ..
            } => {
                let layout = Section::Titled("Layout");
                let name = Section::Titled("Name");
                let pane = Section::Titled("Pane");
                let mut items = vec![
                    item(
                        "Split right",
                        Action::SplitRight,
                        GLYPH_SPLIT_RIGHT,
                        layout,
                        Kind::Action,
                    ),
                    item(
                        "Split down",
                        Action::SplitDown,
                        GLYPH_SPLIT_DOWN,
                        layout,
                        Kind::Action,
                    ),
                    item("Zoom", Action::Zoom, GLYPH_ZOOM, layout, Kind::Action),
                ];
                if source_pane_id.is_some() {
                    items.push(item(
                        "Swap with focused pane",
                        Action::SwapWithFocusedPane,
                        GLYPH_HANDOFF,
                        layout,
                        Kind::Action,
                    ));
                }
                items.push(item(
                    "Rename pane",
                    Action::RenamePane,
                    GLYPH_RENAME,
                    name,
                    Kind::Action,
                ));
                if *has_manual_label {
                    items.push(item(
                        "Clear pane name",
                        Action::ClearPaneName,
                        GLYPH_CLEAR,
                        name,
                        Kind::Action,
                    ));
                }
                // Fork: the pane's tab's info dock.
                items.push(item(
                    "Info pane",
                    Action::ToggleInfoPane,
                    GLYPH_INFO,
                    pane,
                    Kind::Action,
                ));
                // Fork: once the pane agent's session id arrived.
                if session_id.is_some() {
                    items.push(item(
                        "Copy session ID",
                        Action::CopySessionId,
                        GLYPH_COPY,
                        pane,
                        Kind::Action,
                    ));
                }
                items.push(item(
                    "Send right-clicks to pane",
                    Action::ToggleRightClickPassthrough,
                    if *right_click_passthrough {
                        GLYPH_ON
                    } else {
                        GLYPH_OFF
                    },
                    pane,
                    Kind::Toggle(*right_click_passthrough),
                ));
                items.push(item(
                    "Close pane",
                    Action::ClosePane,
                    GLYPH_CLOSE,
                    Section::Last,
                    Kind::Danger,
                ));
                items
            }
        }
    }
}

// Fork (menu design): one glyph per item (the legend of the chosen mock).
pub(super) const GLYPH_NEW: &str = "+";
pub(super) const GLYPH_RENAME: &str = "\u{270E}"; // ✎
pub(super) const GLYPH_INFO: &str = "\u{25E7}"; // ◧
pub(super) const GLYPH_CLOSE: &str = "\u{2715}"; // ✕
pub(super) const GLYPH_SUSPEND: &str = "\u{2016}"; // ‖
pub(super) const GLYPH_ACTIVATE: &str = "\u{25B6}"; // ▶
pub(super) const GLYPH_RESTART: &str = "\u{21BB}"; // ↻
pub(super) const GLYPH_HANDOFF: &str = "\u{21C4}"; // ⇄
pub(super) const GLYPH_COPY: &str = "\u{2398}"; // ⎘
pub(super) const GLYPH_ROLE: &str = "\u{25C8}"; // ◈
pub(super) const GLYPH_LEAVE: &str = "\u{2190}"; // ←
pub(super) const GLYPH_JOIN: &str = "\u{2192}"; // →
pub(super) const GLYPH_UNGROUP: &str = "\u{21F1}"; // ⇱
pub(super) const GLYPH_DISBAND: &str = "\u{2298}"; // ⊘
pub(super) const GLYPH_DELETE: &str = "\u{2298}"; // ⊘
pub(super) const GLYPH_TEAM: &str = "\u{25C6}"; // ◆
pub(super) const GLYPH_SPLIT_RIGHT: &str = "\u{25AE}"; // ▮
pub(super) const GLYPH_SPLIT_DOWN: &str = "\u{25AC}"; // ▬
pub(super) const GLYPH_ZOOM: &str = "\u{2922}"; // ⤢
pub(super) const GLYPH_CLEAR: &str = "\u{2205}"; // ∅
pub(super) const GLYPH_REMIND: &str = "\u{25F7}"; // ◷
pub(super) const GLYPH_COLOR: &str = "\u{25D0}"; // ◐
pub(super) const GLYPH_OFF: &str = "\u{25CB}"; // ○
pub(super) const GLYPH_ON: &str = "\u{25CF}"; // ●
pub(super) const GLYPH_STAR_OFF: &str = "\u{2606}"; // ☆
pub(super) const GLYPH_STAR_ON: &str = "\u{2605}"; // ★
pub(super) const GLYPH_WORKTREE: &str = "\u{2387}"; // ⎇
pub(super) const GLYPH_COLLAPSE: &str = "\u{25BE}"; // ▾
pub(super) const GLYPH_EXPAND: &str = "\u{25B8}"; // ▸
pub(super) const GLYPH_OPEN: &str = "\u{25A4}"; // ▤
pub(super) const GLYPH_FOCUS: &str = "\u{25CE}"; // ◎
pub(super) const GLYPH_STOP: &str = "\u{25A0}"; // ■
pub(super) const GLYPH_WAKE: &str = "\u{21AF}"; // ↯
pub(super) const GLYPH_SETTINGS: &str = "\u{2261}"; // ≡

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
                && snapshot.workspaces.iter().any(|candidate| {
                    candidate.worktree.as_ref().is_some_and(|candidate| {
                        candidate.key == worktree.key && candidate.is_linked_worktree
                    })
                })
        });
        let close_group = super::sidebar::workspace_close_is_group(snapshot, workspace);
        let collapsed = worktree.is_some_and(|worktree| {
            self.group_is_collapsed(&self.active_endpoint_id, &worktree.key)
        });
        self.overlay = Some(ClientShellOverlay::ContextMenu(ClientContextMenuOverlay {
            target: ClientContextMenuTarget::Workspace {
                workspace_id,
                is_git: worktree.is_some() || workspace.branch.is_some(),
                is_linked_worktree: worktree.is_some_and(|worktree| worktree.is_linked_worktree),
                has_worktree_children,
                close_group,
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
                    handoff: super::actions::agent_handoff_method(snapshot, &agent.pane_id)
                        .is_some_and(|method| self.supports_endpoint_method(&method)),
                })
        });
        let color = super::tab_color::tab_menu_color(tab.color);
        let team = self.tab_team_menu(&tab.workspace_id, agent.as_ref());
        // Fork (sidebar v3): the Pin item's state and whether it shows.
        let pinned = self.active_tab_pinned(&tab_id);
        let pin_supported = self.supports_endpoint_method(
            &crate::api::schema::Method::TabSetPinned(crate::api::schema::TabSetPinnedParams {
                tab_id: String::new(),
                pinned: true,
            }),
        );
        // Fork: the Mute item's state and whether it shows.
        let muted = self.active_tab_muted(&tab_id);
        let mute_supported = self.supports_endpoint_method(
            &crate::api::schema::Method::TabSetMuted(crate::api::schema::TabSetMutedParams {
                tab_id: String::new(),
                muted: true,
            }),
        );
        // Fork: the Do not disturb item's state and whether it shows.
        let dnd = self.active_tab_dnd(&tab_id);
        let dnd_supported = self.supports_endpoint_method(&crate::api::schema::Method::TabSetDnd(
            crate::api::schema::TabSetDndParams {
                tab_id: String::new(),
                dnd: true,
            },
        ));
        // Fork (companion launcher): the tab's dock and Browser view.
        let browser_dock = self.browser_dock_offered().then(|| {
            (
                self.browser_dock_open_for(&tab_id),
                self.browser_view_open_for(&tab_id),
            )
        });
        // Fork (companion browser): the session's active page, if any.
        let browser_page = self.browser.info.as_ref().and_then(|info| {
            info.sessions
                .iter()
                .find(|session| session.tab_id == tab_id)
                .map(|session| {
                    let title = if session.title.trim().is_empty() {
                        session.url.clone()
                    } else {
                        session.title.clone()
                    };
                    (title, session.needs_you)
                })
                .filter(|(title, _)| !title.trim().is_empty())
        });
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
                pinned,
                pin_supported,
                muted,
                mute_supported,
                dnd,
                dnd_supported,
                browser_page,
                browser_dock,
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
            // Fork (companion browser): the page title row opens the
            // Browser overlay, without focusing the tab.
            (ClientContextMenuAction::BrowserOpenOverlay, ClientContextMenuTarget::Tab { .. }) => {
                self.open_browser_overlay(outcome);
                outcome.repaint = true;
                return;
            }
            // Fork (sidebar v3): Pin / Unpin, without focusing the tab. The
            // block follows when the `endpoint.tab-pins.v1` push arrives.
            (ClientContextMenuAction::Pin, ClientContextMenuTarget::Tab { tab_id, pinned, .. }) => {
                self.push_endpoint_method(
                    crate::api::schema::Method::TabSetPinned(
                        crate::api::schema::TabSetPinnedParams {
                            tab_id: tab_id.clone(),
                            pinned: !*pinned,
                        },
                    ),
                    outcome,
                );
                outcome.repaint = true;
                return;
            }
            // Fork: Mute / Unmute notifications, without focusing the tab.
            // The mark follows when the `endpoint.tab-mutes.v1` push arrives.
            (ClientContextMenuAction::Mute, ClientContextMenuTarget::Tab { tab_id, muted, .. }) => {
                self.push_endpoint_method(
                    crate::api::schema::Method::TabSetMuted(
                        crate::api::schema::TabSetMutedParams {
                            tab_id: tab_id.clone(),
                            muted: !*muted,
                        },
                    ),
                    outcome,
                );
                outcome.repaint = true;
                return;
            }
            // Fork: Do not disturb / End do not disturb, without focusing
            // the tab. The mark follows when the `endpoint.quiet.v1` push
            // arrives.
            (ClientContextMenuAction::Dnd, ClientContextMenuTarget::Tab { tab_id, dnd, .. }) => {
                self.push_endpoint_method(
                    crate::api::schema::Method::TabSetDnd(crate::api::schema::TabSetDndParams {
                        tab_id: tab_id.clone(),
                        dnd: !*dnd,
                    }),
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
            // Fork (companion launcher): the tab menu's dock and Browser
            // view items act on that tab without focusing it; the Browser
            // row's on the focused tab.
            (
                ClientContextMenuAction::BrowserToggleDock,
                ClientContextMenuTarget::Tab { tab_id, .. },
            ) => {
                self.toggle_browser_dock(tab_id.clone(), outcome);
                outcome.repaint = true;
                return;
            }
            (
                ClientContextMenuAction::BrowserToggleView,
                ClientContextMenuTarget::Tab { tab_id, .. },
            ) => {
                self.toggle_browser_view(tab_id.clone(), outcome);
                outcome.repaint = true;
                return;
            }
            (
                ClientContextMenuAction::BrowserToggleDock,
                ClientContextMenuTarget::Browser { .. },
            ) => {
                self.toggle_browser_dock_for_focused_tab(outcome);
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
            ClientContextMenuTarget::Workspace {
                workspace_id,
                close_group,
                ..
            } => self.activate_workspace_context_action(workspace_id, close_group, action, outcome),
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
        close_group: bool,
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
                self.request_workspace_close(workspace_id, Some(close_group), outcome);
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
            ClientContextMenuAction::HandoffCompact => {
                if let Some(pane_id) = agent.map(|agent| agent.pane_id) {
                    self.push_endpoint_method(
                        Method::AgentsHandoff(
                            crate::api::schema::agents_model::AgentsHandoffParams {
                                caller_pane: None,
                                target: pane_id,
                            },
                        ),
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
            ClientContextMenuAction::ClosePane => self.request_pane_close(pane_id, outcome),
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
