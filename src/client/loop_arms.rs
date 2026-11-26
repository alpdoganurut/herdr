//! Fork (dock sync): the client loop's server-message, response and resize
//! arms as functions, so a deterministic test can drive the same code the
//! loop runs (`tests/dock_sync.rs`) with fake endpoints and a virtual clock.
//! Each takes the loop's state by reference and does exactly what its arm
//! in `run_client_loop` did.

use super::*;

/// How the loop treats a server message: whether its endpoint is the one
/// shown, and whether a pending activation takes it.
pub(super) struct MessageRoute {
    pub(super) endpoint_active: bool,
    pub(super) activation_message: bool,
}

/// The loop's gate before it handles a server message (`None`: dropped).
pub(super) fn route_server_message(
    state: &ClientState,
    endpoints: &endpoint::EndpointRegistry,
    pending: &Option<endpoint::PendingEndpointActivation>,
    endpoint_commands: &endpoint_commands::EndpointCommands,
    endpoint_id: &endpoint::ClientEndpointId,
    generation: u64,
    message: &ServerMessage,
) -> Option<MessageRoute> {
    let endpoint_active = endpoints.active_id() == endpoint_id
        && endpoints
            .connection(endpoint_id)
            .is_some_and(|connection| connection.surface_active);
    let activation_message = pending
        .as_ref()
        .is_some_and(|pending| pending.accepts_endpoint(endpoint_id, generation));
    let command_response = match message {
        ServerMessage::ClientShellEndpointResponseChunk {
            boot_id,
            request_id,
            ..
        } => endpoint_commands.accepts_response(endpoint_id, generation, boot_id, request_id),
        _ => false,
    };
    if !endpoint::accepts_endpoint_message(
        endpoint_active,
        activation_message,
        command_response,
        message,
    ) {
        return None;
    }
    // Target presentation effects may arrive as soon as surface.set(true) is
    // acknowledged. They cannot be applied while the source frame is frozen; the
    // target receives one explicit replay after the coherent commit instead.
    if state.presentation_frozen && activation_message && endpoint::is_presentation_effect(message)
    {
        return None;
    }
    Some(MessageRoute {
        endpoint_active,
        activation_message,
    })
}

/// `ServerMessage::PaneSurface`.
pub(super) fn receive_pane_surface(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    pending: &mut Option<endpoint::PendingEndpointActivation>,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
    prefix_input_source: &mut impl crate::platform::PrefixInputSource,
    endpoint_id: &endpoint::ClientEndpointId,
    generation: u64,
    surface: crate::protocol::PaneSurfaceFrame,
    route: &MessageRoute,
) -> Result<Option<ClientLoopEvent>, ClientError> {
    if route.activation_message {
        let progress = pending
            .as_mut()
            .map(|pending| pending.receive_surface(endpoint_id, generation, surface));
        if matches!(progress, Some(endpoint::SurfaceActivationProgress::Ready)) {
            return complete_endpoint_activation(state, endpoints, pending, endpoint_commands);
        }
        return Ok(None);
    }
    if !route.endpoint_active {
        return Ok(None);
    }
    let composed = if let Some(shell) = &mut state.shell {
        shell.set_pane_surface(surface);
        shell.compose(state.reported_size.0, state.reported_size.1)
    } else {
        None
    };
    apply_client_shell_input_source_changes(state, prefix_input_source);
    if let Some(frame) = composed {
        state.present_frame(frame);
    }
    Ok(None)
}

/// A decoded endpoint snapshot (`EndpointControl` snapshot): installed, the
/// activation that waited for it completed, the connection marked ready.
pub(super) fn receive_shell_snapshot(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    pending: &mut Option<endpoint::PendingEndpointActivation>,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
    prefix_input_source: &mut impl crate::platform::PrefixInputSource,
    endpoint_id: &endpoint::ClientEndpointId,
    generation: u64,
    snapshot: Box<crate::protocol::ClientShellSnapshot>,
    route: &MessageRoute,
    now: std::time::Instant,
) -> Result<Option<ClientLoopEvent>, ClientError> {
    let projection_pending = route.activation_message;
    let activation_progress = route
        .activation_message
        .then(|| {
            pending
                .as_mut()
                .map(|pending| pending.receive_snapshot(endpoint_id, generation, &snapshot))
        })
        .flatten();
    install_client_shell_snapshot(
        state,
        endpoint_id,
        snapshot,
        projection_pending,
        endpoints,
        prefix_input_source,
        now,
    )?;
    let event = if matches!(
        activation_progress,
        Some(endpoint::SurfaceActivationProgress::Ready)
    ) {
        complete_endpoint_activation(state, endpoints, pending, endpoint_commands)?
    } else {
        None
    };
    endpoints.mark_ready(endpoint_id, generation);
    Ok(event)
}

/// After a snapshot from `endpoint_id`: a Local selection that waited for
/// Local's metadata, else the selected endpoint when it has no surface (at
/// start, after a reconnect or a handoff that left none).
pub(super) fn activation_after_snapshot(
    state: &mut ClientState,
    endpoints: &endpoint::EndpointRegistry,
    pending: &Option<endpoint::PendingEndpointActivation>,
    catalog: &endpoint::EndpointCatalog,
    endpoint_id: &endpoint::ClientEndpointId,
) -> Option<ClientLoopEvent> {
    if endpoint_id.is_local() {
        if let Some(event) = take_ready_local_activation(state, endpoints) {
            return Some(event);
        }
    }
    let selected_endpoint = desired_endpoint(state, catalog, endpoints);
    let activation_ready = state.shell.as_ref().is_some_and(|shell| {
        shell.endpoint_has_snapshot(&selected_endpoint)
            && (!endpoints
                .connection(endpoints.active_id())
                .is_some_and(|connection| connection.surface_active)
                || shell.endpoint_boot_id(endpoints.active_id()).is_some())
    });
    let needs_surface = endpoints
        .connection(&selected_endpoint)
        .is_some_and(|connection| !connection.surface_active);
    (activation_ready
        && needs_surface
        && pending.is_none()
        && state.deferred_local_activation.is_none())
    .then_some(ClientLoopEvent::ActivateEndpoint {
        endpoint_id: selected_endpoint,
        target: None,
        force: false,
    })
}

/// `PRESENTATION_EFFECTS_READY_KIND`: the activation's last fence.
pub(super) fn receive_presentation_effects_ready(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    pending: &mut Option<endpoint::PendingEndpointActivation>,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
    endpoint_id: &endpoint::ClientEndpointId,
    generation: u64,
    data: &str,
) -> Result<Option<ClientLoopEvent>, ClientError> {
    let progress = pending.as_mut().map(|activation| {
        activation.receive_presentation_effects_ready(endpoint_id, generation, data)
    });
    if matches!(progress, Some(endpoint::SurfaceActivationProgress::Ready)) {
        return complete_endpoint_activation(state, endpoints, pending, endpoint_commands);
    }
    Ok(None)
}

/// What a response chunk was.
pub(super) enum ResponseChunk {
    /// The activation's, a surface lease's or a placement's: handled here.
    Handled(Option<ClientLoopEvent>),
    /// A command lane's: for [`receive_command_response`].
    Command,
}

/// The response chunks the loop handles before the command lane: an
/// activation's acknowledgements, a released surface lease's and a browser
/// placement's answers.
pub(super) fn receive_lifecycle_response(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    pending: &mut Option<endpoint::PendingEndpointActivation>,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
    endpoint_id: &endpoint::ClientEndpointId,
    generation: u64,
    boot_id: &str,
    request_id: &str,
    final_chunk: bool,
    data: &[u8],
    now: std::time::Instant,
) -> Result<ResponseChunk, ClientError> {
    if pending.as_ref().is_some_and(|pending| {
        pending.accepts_response(endpoint_id, generation, boot_id, request_id)
    }) {
        if !final_chunk {
            rollback_endpoint_activation(
                state,
                endpoints,
                pending,
                "endpoint returned a chunked activation acknowledgement".into(),
                false,
            );
            return Ok(ResponseChunk::Handled(None));
        }
        let progress = pending.as_mut().map(|pending| {
            pending.receive_response_for_boot(
                endpoint_id,
                generation,
                boot_id,
                request_id,
                data,
                endpoints,
            )
        });
        let event = match progress {
            Some(endpoint::SurfaceActivationProgress::Ready) => {
                complete_endpoint_activation(state, endpoints, pending, endpoint_commands)?
            }
            Some(endpoint::SurfaceActivationProgress::Rejected {
                message,
                source_release_rejected,
            }) => {
                rollback_endpoint_activation(
                    state,
                    endpoints,
                    pending,
                    message,
                    source_release_rejected,
                );
                None
            }
            _ => None,
        };
        return Ok(ResponseChunk::Handled(event));
    }
    if request_id.starts_with("client-shell-surface:") {
        return Ok(ResponseChunk::Handled(None));
    }
    // Fork (companion launcher): placements are not waited for. Fork (dock
    // sync): a refused one is sent again (`browser_dock.rs`).
    if request_id.starts_with(shell::BROWSER_PLACEMENT_REQUEST_PREFIX) {
        if let Some(shell) = state.shell.as_mut() {
            let answer = endpoint_commands::parse_response(request_id, data);
            if let Err(error) = &answer {
                // at most one a second (`REFUSED_RETRY`)
                debug!(
                    endpoint = ?endpoint_id,
                    request_id,
                    code = error.code.as_deref().unwrap_or("none"),
                    message = %error.message,
                    "browser placement refused; sending it again"
                );
            }
            let accepted = final_chunk && answer.is_ok();
            shell.receive_browser_placement_answer(request_id, accepted, now);
        }
        return Ok(ResponseChunk::Handled(None));
    }
    Ok(ResponseChunk::Command)
}

/// A command lane's response chunk: the shell's result, the actions it
/// starts and the next command. Returns whether the client detaches.
#[allow(clippy::too_many_arguments)] // the loop's state, one reference each
pub(super) fn receive_command_response(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    pending: &mut Option<endpoint::PendingEndpointActivation>,
    endpoint_commands: &mut endpoint_commands::EndpointCommands,
    prefix_input_source: &mut impl crate::platform::PrefixInputSource,
    scheduled_activation: &mut Option<ClientLoopEvent>,
    endpoint_id: &endpoint::ClientEndpointId,
    generation: u64,
    boot_id: &str,
    request_id: &str,
    final_chunk: bool,
    data: Vec<u8>,
    now: std::time::Instant,
) -> Result<bool, ClientError> {
    let completed = endpoint_commands
        .receive_chunk(
            endpoint_id,
            generation,
            boot_id,
            request_id,
            final_chunk,
            data,
        )
        .map_err(ClientError::ConnectionLost)?;
    let Some(completed) = completed else {
        return Ok(false);
    };
    let (repaint, actions) = state.shell.as_mut().map_or_else(
        || (false, Vec::new()),
        |shell| {
            if completed.generation == generation
                && shell.endpoint_is_active(&completed.endpoint_id)
            {
                shell.handle_endpoint_result(
                    &completed.boot_id,
                    &completed.request_id,
                    completed.result,
                )
            } else {
                (
                    shell.cancel_endpoint_request(&completed.request_id),
                    Vec::new(),
                )
            }
        },
    );
    if let Some(shell) = state.shell.as_mut() {
        shell.reconcile_input_source();
    }
    apply_client_shell_input_source_changes(state, prefix_input_source);
    let (replay_mouse, dispatch_repaint) = dispatch_client_shell_actions(
        actions,
        endpoint_commands,
        endpoints,
        state.shell.as_mut(),
        &mut state.detached_process_children,
        scheduled_activation,
    )?;
    let repaint = repaint || dispatch_repaint;
    if replay_mouse.is_empty() {
        if repaint {
            if let Some(frame) = state
                .shell
                .as_mut()
                .and_then(|shell| shell.compose(state.reported_size.0, state.reported_size.1))
            {
                state.present_frame(frame);
            }
        }
        return Ok(false);
    }
    let (outcome, frame) = {
        let Some(shell) = state.shell.as_mut() else {
            return Ok(false);
        };
        let mut outcome = shell.replay_mouse_events(replay_mouse);
        outcome.repaint |= repaint;
        let frame = outcome
            .repaint
            .then(|| shell.compose(state.reported_size.0, state.reported_size.1))
            .flatten();
        (outcome, frame)
    };
    finish_client_shell_input(
        state,
        outcome,
        frame,
        endpoints,
        pending,
        endpoint_commands,
        prefix_input_source,
        scheduled_activation,
        now,
    )
}

/// The shell's part of `ClientLoopEvent::Resize`: the new size, the
/// invalidated surface and the size report (to a pending activation's
/// endpoint while one runs).
pub(super) fn apply_terminal_resize(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    pending: &mut Option<endpoint::PendingEndpointActivation>,
    new_cols: u16,
    new_rows: u16,
    cell_width_px: u32,
    cell_height_px: u32,
    pixel_geometry_exact: bool,
    pixel_mouse_available: bool,
) -> Result<(), ClientError> {
    state.reported_size = (new_cols, new_rows);
    state.reported_cell_size = (cell_width_px, cell_height_px);
    state.pixel_geometry_exact = pixel_geometry_exact;
    // Resizing invalidates both the host-side blit baseline and pane hit geometry.
    state.request_repaint();
    if let Some(shell) = state.shell.as_mut() {
        shell.set_graphics_cell_size(cell_width_px, cell_height_px);
        shell.invalidate_pane_surface();
        // Fork (companion launcher): the browser placement asks
        // the terminal for its pixel extent again.
        shell.note_terminal_resized();
    }
    let msg = if let Some(shell) = &state.shell {
        client_shell_resize_message(
            shell,
            new_cols,
            new_rows,
            cell_width_px,
            cell_height_px,
            pixel_mouse_available,
        )
    } else {
        ClientMessage::Resize {
            cols: new_cols,
            rows: new_rows,
            cell_width_px,
            cell_height_px,
            pixel_mouse: pixel_mouse_available,
        }
    };
    if let Some(activation) = pending.as_mut() {
        if let Err(error) = activation.update_resize(msg, endpoints) {
            rollback_endpoint_activation(state, endpoints, pending, error, false);
        }
    } else if state.shell.is_some() {
        // Fork (dock sync): the size the shell's layout reported, recorded.
        if endpoints.send(&msg) == endpoint::EndpointSendOutcome::Sent {
            let active = endpoints.active_id().clone();
            state.note_surface_reported(endpoints, &active, &msg);
        }
    } else if let Err(e) = write_to_server(endpoints, &msg) {
        return Err(ClientError::ConnectionLost(e));
    }
    Ok(())
}

/// Fork (dock sync): what the loop checks before it waits for the next
/// event (`run_client_loop`'s top, after every event): the surface size the
/// layout wants reaches the endpoint shown, whatever changed the layout (a
/// `browser.get` answer adding the view strip, a dock toggled before the
/// first frame, an activation that sized the target by the layout it left),
/// and a browser placement held back for a frame is placed once one is
/// presented. `surface_size` is the layout's pane surface for the
/// terminal's reported size, computed once per loop pass (`None` without a
/// shell): a compare of plain data, a `ClientShellResize` only when they
/// differ.
pub(super) fn reconcile_after_event(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    pending: &mut Option<endpoint::PendingEndpointActivation>,
    surface_size: Option<crate::protocol::ClientSurfaceSize>,
    now: std::time::Instant,
) {
    reconcile_surface_size(state, endpoints, pending, surface_size);
    send_browser_placement_recheck(state, endpoints, pending, now);
}

fn reconcile_surface_size(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    pending: &mut Option<endpoint::PendingEndpointActivation>,
    surface_size: Option<crate::protocol::ClientSurfaceSize>,
) {
    let (Some(shell), Some(surface_size)) = (state.shell.as_ref(), surface_size) else {
        return;
    };
    let active = endpoints.active_id();
    // Until an activation projects its endpoint, the shell still lays out the
    // one it leaves (or, for a Local tab click, the tab before the click):
    // only once projected is the layout the endpoint's own.
    let ready = match pending.as_ref() {
        Some(activation) => activation.projected_endpoint() == Some(active),
        None => endpoints.active_surface_available(),
    };
    if !ready || !shell.endpoint_is_active(active) || !shell.endpoint_is_online(active) {
        return;
    }
    // `client_shell_resize_message` for the size computed this pass
    let want = ClientMessage::ClientShellResize {
        cell_width_px: state.reported_cell_size.0,
        cell_height_px: state.reported_cell_size.1,
        surface_size,
        pixel_mouse: state.pixel_mouse_available(),
    };
    if let Some(activation) = pending.as_mut() {
        if *activation.surface_resize() == want {
            return;
        }
        if let Err(error) = activation.update_resize(want, endpoints) {
            rollback_endpoint_activation(state, endpoints, pending, error, false);
        }
        return;
    }
    let Some(generation) = endpoints
        .connection(active)
        .map(|connection| connection.generation)
    else {
        return;
    };
    if state.reported_surface.as_ref().is_some_and(|reported| {
        reported.endpoint_id == *active
            && reported.generation == generation
            && reported.message == want
    }) {
        return;
    }
    if endpoints.send(&want) == endpoint::EndpointSendOutcome::Sent {
        let active = endpoints.active_id().clone();
        state.note_surface_reported(endpoints, &active, &want);
    }
}

/// A placement held back for a frame (or one to send again after an
/// activation committed) goes before the loop waits, beside the command
/// lane like the input path's (`finish_client_shell_input`).
fn send_browser_placement_recheck(
    state: &mut ClientState,
    endpoints: &mut endpoint::EndpointRegistry,
    pending: &Option<endpoint::PendingEndpointActivation>,
    now: std::time::Instant,
) {
    if pending.is_some() || !endpoints.active_surface_available() {
        return;
    }
    let active = endpoints.active_id();
    let Some(shell) = state.shell.as_mut() else {
        return;
    };
    if !shell.endpoint_is_online(active) || !shell.take_browser_placement_recheck() {
        return;
    }
    if let Some(placement) = shell.take_browser_placement_request(now) {
        if endpoints.send(&placement) != endpoint::EndpointSendOutcome::Sent {
            shell.browser_placement_not_sent();
        }
    }
}
