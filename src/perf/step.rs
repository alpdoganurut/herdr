//! Fork (stall attribution): which step a watched loop is in.
//!
//! The loop marks each step as it enters it; the [`super::watchdog`] thread
//! reads the mark while a stall is open, so a stall names its cause (the
//! loop phase, and for a dispatched item the API method, app event kind,
//! server event kind or scheduled task).
//!
//! One [`StepMarker`] is one `AtomicU64` that only its loop's thread writes
//! (relaxed load + store, no clock read, no allocation, no lock):
//!
//! | bits   | field                                            |
//! |--------|--------------------------------------------------|
//! | 0..8   | current [`Step`]                                 |
//! | 8..20  | current detail (index + 1 into the step's table) |
//! | 20..28 | outer step (the step a nested guard interrupted) |
//! | 28..40 | outer detail                                     |
//! | 40..64 | change counter (wraps)                           |
//!
//! Sequential loop phases use [`StepMarker::set`]; handlers that can run
//! inside another step (an API request draining app events, a client-shell
//! endpoint request dispatching an API method) use [`StepMarker::enter`],
//! whose guard restores the interrupted step when it drops.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::events::AppEvent;
use crate::server::client_transport::ServerEvent;

/// Defines a fieldless enum (variants numbered from 0 in order), the
/// slice of all its variants, the slice of their names and `name()`, from
/// one list, so the three never drift apart.
macro_rules! named_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident: $repr:ident, $all:ident, $names:ident {
            $($(#[$vmeta:meta])* $variant:ident => $text:literal,)*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr($repr)]
        $vis enum $name {
            $($(#[$vmeta])* $variant,)*
        }

        /// Every variant, in discriminant order.
        $vis const $all: &[$name] = &[$($name::$variant,)*];
        /// Each variant's name, in discriminant order.
        $vis const $names: &[&str] = &[$($text,)*];

        impl $name {
            pub(crate) fn name(self) -> &'static str {
                $names[self as usize]
            }
        }
    };
}

named_enum! {
    /// A step of a watched loop.
    pub(crate) enum Step: u8, STEPS, STEP_NAMES {
        /// Never marked (a loop without step marks, or the start). Must stay first (0).
        Unmarked => "unmarked",
        /// The top of an iteration: reaping detached processes, quit checks.
        LoopTop => "loop_top",
        /// Beginning shutdown (`initiate_shutdown`).
        Shutdown => "shutdown",
        /// The bounded internal (app) event drain.
        InternalEvents => "internal_events",
        /// Expiring due pane metadata.
        MetadataExpiry => "metadata_expiry",
        /// The bounded API request drain.
        ApiRequests => "api_requests",
        /// Focus events and the session-save schedule.
        FocusSync => "focus_sync",
        /// Accepting client connections.
        ClientAccept => "client_accept",
        /// The bounded server (client transport) event drain.
        ServerEvents => "server_events",
        /// One scheduled task (detail: [`ScheduledTask`]).
        Scheduled => "scheduled",
        /// Pending and deferred alternate-screen reads.
        AltScreenReads => "alt_screen_reads",
        /// The default workspace for a newly attached shell client.
        DefaultWorkspace => "default_workspace",
        /// Expiring native graphics.
        NativeGraphics => "native_graphics",
        /// Config reload, immediate PTY sources, mouse and keyboard modes.
        ClientSync => "client_sync",
        /// The render decision, title sync and visibility check.
        RenderPrepare => "render_prepare",
        /// A full render pass and its frame fanout.
        FullRender => "full_render",
        /// A retained pane-surface pass and its fanout.
        RetainedRender => "retained_render",
        /// Computing the next wake deadline.
        NextDeadline => "next_deadline",
        /// Right after the wait, before the event's handler marks itself.
        Dispatch => "dispatch",
        /// One app event (detail: its kind).
        AppEvent => "app_event",
        /// One API request (detail: its method).
        ApiRequest => "api_request",
        /// One server event (detail: its kind; client attach is `client_connected`).
        ServerEvent => "server_event",
    }
}

impl Step {
    fn from_bits(bits: u64) -> Step {
        STEPS
            .get(usize::try_from(bits).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or(Step::Unmarked)
    }

    /// The name of detail `index` of this step, if the step has a table.
    fn detail_name(self, index: usize) -> Option<&'static str> {
        match self {
            Step::ApiRequest => crate::api::schema::Method::OPERATIONS
                .get(index)
                .map(|operation| operation.name),
            Step::AppEvent => APP_EVENT_KINDS.get(index).copied(),
            Step::ServerEvent => SERVER_EVENT_KINDS.get(index).copied(),
            Step::Scheduled => ALL_SCHEDULED_TASKS.get(index).map(|task| task.name()),
            _ => None,
        }
    }
}

/// Defines a kind table and the index of a value's kind. Each variant is
/// matched with `{ .. }` (fits unit, tuple and struct variants), so an
/// added upstream variant is a compile error here, never an unnamed kind.
macro_rules! kinds {
    ($table:ident, $index:ident, $ty:ident { $($variant:ident => $name:literal,)* }) => {
        pub(crate) const $table: &[&str] = &[$($name,)*];

        #[inline]
        pub(crate) fn $index(value: &$ty) -> u16 {
            enum Index { $($variant,)* }
            match value {
                $($ty::$variant { .. } => Index::$variant as u16,)*
            }
        }
    };
}

kinds!(APP_EVENT_KINDS, app_event_kind, AppEvent {
    PaneDied => "pane_died",
    WorktreeRuntimeRestoreFailed => "worktree_runtime_restore_failed",
    AgentProcessDetected => "agent_process_detected",
    CodexPromptObserved => "codex_prompt_observed",
    StateChanged => "state_changed",
    HookStateReported => "hook_state_reported",
    AgentSessionReported => "agent_session_reported",
    AgentResumeReported => "agent_resume_reported",
    ReportedAgentShellReturned => "reported_agent_shell_returned",
    HookMetadataReported => "hook_metadata_reported",
    HookAuthorityCleared => "hook_authority_cleared",
    HookAgentReleased => "hook_agent_released",
    UpdateReady => "update_ready",
    AgentDetectionManifestsUpdated => "agent_detection_manifests_updated",
    TerminalBell => "terminal_bell",
    ClipboardWrite => "clipboard_write",
    TerminalCwdReported => "terminal_cwd_reported",
    GitStatusRefreshed => "git_status_refreshed",
    RestoredWorktreeSpaceChecked => "restored_worktree_space_checked",
    TabBarCommandFinished => "tab_bar_command_finished",
    PluginCommandFinished => "plugin_command_finished",
    WorktreeAddFinished => "worktree_add_finished",
    WorktreeRemoveFinished => "worktree_remove_finished",
    WorktreeReadFinished => "worktree_read_finished",
    CoordinatorPassFinished => "coordinator_pass_finished",
    AgentRemoteObserved => "agent_remote_observed",
});

kinds!(SERVER_EVENT_KINDS, server_event_kind, ServerEvent {
    ClientConnected => "client_connected",
    ClientShellConnected => "client_shell_connected",
    ClientInput => "client_input",
    GraphicsTransmissionResult => "graphics_transmission_result",
    GraphicsTransmissionStarted => "graphics_transmission_started",
    ClientPasteRejected => "client_paste_rejected",
    ClientClipboardImage => "client_clipboard_image",
    ClientAttachTerminal => "client_attach_terminal",
    ClientObserveTerminal => "client_observe_terminal",
    ClientControlTerminal => "client_control_terminal",
    ClientAttachScroll => "client_attach_scroll",
    ClientAttachMouse => "client_attach_mouse",
    ClientResize => "client_resize",
    ClientShellResize => "client_shell_resize",
    ClientShellPaneInput => "client_shell_pane_input",
    ClientShellPopupInput => "client_shell_popup_input",
    ClientShellHostTheme => "client_shell_host_theme",
    ClientShellFocus => "client_shell_focus",
    ClientShellMouseCapture => "client_shell_mouse_capture",
    ClientShellPresentationSync => "client_shell_presentation_sync",
    ClientShellEndpointRequest => "client_shell_endpoint_request",
    ClientShellEndpointRequestError => "client_shell_endpoint_request_error",
    ClientShellEndpointResponseChunkReady => "client_shell_endpoint_response_chunk_ready",
    ClientDetach => "client_detach",
    ClientDisconnected => "client_disconnected",
    ClientWriterDrained => "client_writer_drained",
    QuitSignal => "quit_signal",
});

named_enum! {
    /// The scheduled tasks of the headless server's tick (`Step::Scheduled`).
    pub(crate) enum ScheduledTask: u16, ALL_SCHEDULED_TASKS, SCHEDULED_TASKS {
        Deadlines => "deadlines",
        AgentNotifications => "agent_notifications",
        WorktreeValidation => "worktree_validation",
        GitRefresh => "git_refresh",
        UpdateChecks => "update_checks",
        SessionSave => "session_save",
        TranscriptBackup => "transcript_backup",
        /// Codex session and agent context probes.
        AgentProbes => "agent_probes",
        MetadataExpiry => "metadata_expiry",
        /// Coordinator and news notifications flushed to clients.
        Notices => "notices",
        AgentLifecycle => "agent_lifecycle",
        Autosuspend => "autosuspend",
        AgentResumes => "agent_resumes",
        /// Agent handoff, then compact, runs.
        AgentHandoff => "agent_handoff",
        HostAppearance => "host_appearance",
        TabBarStatus => "tab_bar_status",
        News => "news",
        Coordinator => "coordinator",
        MessageQueue => "message_queue",
        /// Companion browser page errors to their agents.
        PageErrors => "page_errors",
    }
}

/// The index of an API method in `Method::OPERATIONS` (one scan of short
/// static names per request; never per byte or per pane).
pub(crate) fn api_method_index(method: &crate::api::schema::Method) -> u16 {
    let name = method.operation().name;
    crate::api::schema::Method::OPERATIONS
        .iter()
        .position(|operation| operation.name == name)
        .and_then(|index| u16::try_from(index).ok())
        .unwrap_or(NO_DETAIL)
}

const STEP_BITS: u32 = 8;
const DETAIL_BITS: u32 = 12;
const MARK_BITS: u32 = STEP_BITS + DETAIL_BITS;
const MARK_MASK: u64 = (1 << MARK_BITS) - 1;
const DETAIL_MASK: u64 = (1 << DETAIL_BITS) - 1;
const COUNTER_SHIFT: u32 = 2 * MARK_BITS;
/// The change counter's width; deltas are taken modulo this.
pub(crate) const COUNTER_MASK: u64 = (1 << (64 - COUNTER_SHIFT)) - 1;
/// A step without detail.
pub(crate) const NO_DETAIL: u16 = u16::MAX;

#[inline]
fn pack_mark(step: Step, detail: u16) -> u64 {
    let detail = if detail == NO_DETAIL || u64::from(detail) + 1 > DETAIL_MASK {
        0
    } else {
        u64::from(detail) + 1
    };
    step as u64 | (detail << STEP_BITS)
}

/// The step a loop is in; one writer (the loop), any reader.
pub(crate) struct StepMarker(AtomicU64);

impl StepMarker {
    pub(crate) const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    #[inline]
    fn write(&self, current: u64, outer: u64) {
        let counter = (self.0.load(Ordering::Relaxed) >> COUNTER_SHIFT).wrapping_add(1);
        self.0.store(
            current | (outer << MARK_BITS) | ((counter & COUNTER_MASK) << COUNTER_SHIFT),
            Ordering::Relaxed,
        );
    }

    /// The loop enters a sequential phase (clears any outer step).
    #[inline]
    pub(crate) fn set(&self, step: Step) {
        self.write(pack_mark(step, NO_DETAIL), 0);
    }

    /// [`StepMarker::set`] with a detail index into the step's table.
    #[inline]
    pub(crate) fn set_detail(&self, step: Step, detail: u16) {
        self.write(pack_mark(step, detail), 0);
    }

    /// The loop enters a step that may interrupt another; the guard puts
    /// the interrupted step back. Re-entering the same step keeps the outer.
    #[inline]
    pub(crate) fn enter(&'static self, step: Step, detail: u16) -> StepGuard {
        let saved = self.0.load(Ordering::Relaxed);
        let current = pack_mark(step, detail);
        let previous = saved & MARK_MASK;
        let outer = if previous == current || previous == 0 {
            (saved >> MARK_BITS) & MARK_MASK
        } else {
            previous
        };
        self.write(current, outer);
        StepGuard {
            marker: self,
            saved: saved & ((1 << COUNTER_SHIFT) - 1),
        }
    }

    /// The current mark.
    pub(crate) fn read(&self) -> StepRecord {
        StepRecord(self.0.load(Ordering::Relaxed))
    }
}

#[must_use = "the step is restored when the guard drops"]
pub(crate) struct StepGuard {
    marker: &'static StepMarker,
    saved: u64,
}

impl Drop for StepGuard {
    #[inline]
    fn drop(&mut self) {
        self.marker.write(
            self.saved & MARK_MASK,
            (self.saved >> MARK_BITS) & MARK_MASK,
        );
    }
}

/// One read of a [`StepMarker`] (a copy of its word).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct StepRecord(pub(crate) u64);

impl StepRecord {
    pub(crate) fn step(self) -> Step {
        Step::from_bits(self.0 & ((1 << STEP_BITS) - 1))
    }

    pub(crate) fn detail(self) -> Option<&'static str> {
        mark_detail(self.0 & MARK_MASK)
    }

    pub(crate) fn outer_step(self) -> Option<Step> {
        let outer = (self.0 >> MARK_BITS) & MARK_MASK;
        (outer != 0).then(|| Step::from_bits(outer & ((1 << STEP_BITS) - 1)))
    }

    pub(crate) fn outer_detail(self) -> Option<&'static str> {
        mark_detail((self.0 >> MARK_BITS) & MARK_MASK)
    }

    /// The current step alone (no outer step, no counter).
    pub(crate) fn current_mark(self) -> StepRecord {
        StepRecord(self.0 & MARK_MASK)
    }

    /// The interrupted (outer) step alone, if nested.
    pub(crate) fn outer_mark(self) -> Option<StepRecord> {
        let outer = (self.0 >> MARK_BITS) & MARK_MASK;
        (outer != 0).then_some(StepRecord(outer))
    }

    /// The change counter (modulo [`COUNTER_MASK`] + 1).
    pub(crate) fn counter(self) -> u64 {
        self.0 >> COUNTER_SHIFT
    }

    /// Whether the loop ever marked a step.
    pub(crate) fn is_marked(self) -> bool {
        self.0 & MARK_MASK != 0
    }

    /// `step` or `step:detail`, with ` in outer[:detail]` when nested.
    pub(crate) fn describe(self) -> String {
        let mut out = describe_mark(self.step(), self.detail());
        if let Some(outer) = self.outer_step() {
            out.push_str(" in ");
            out.push_str(&describe_mark(outer, self.outer_detail()));
        }
        out
    }
}

fn mark_detail(mark: u64) -> Option<&'static str> {
    let step = Step::from_bits(mark & ((1 << STEP_BITS) - 1));
    let detail = (mark >> STEP_BITS) & DETAIL_MASK;
    let index = usize::try_from(detail.checked_sub(1)?).ok()?;
    step.detail_name(index)
}

fn describe_mark(step: Step, detail: Option<&'static str>) -> String {
    match detail {
        Some(detail) => format!("{}:{detail}", step.name()),
        None => step.name().to_owned(),
    }
}

/// The number of step changes between two reads (counter modulo).
pub(crate) fn changes_between(earlier: StepRecord, later: StepRecord) -> u64 {
    later.counter().wrapping_sub(earlier.counter()) & COUNTER_MASK
}
