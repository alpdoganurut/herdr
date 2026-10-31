use super::projection::ParseError;
use super::workspace_bindings::{command, LEAVES};

pub(super) fn run_workspace_command(args: &[String]) -> std::io::Result<i32> {
    let print_commands = || eprintln!("{}", super::projection::group_commands(&command()));
    let Some(subcommand) = args.first().map(String::as_str) else {
        print_commands();
        return Ok(2);
    };
    if matches!(subcommand, "help" | "--help" | "-h") {
        print_commands();
        return Ok(0);
    }
    let Some(leaf) = LEAVES.iter().find(|leaf| leaf.name == subcommand) else {
        print_commands();
        return Ok(2);
    };
    let request = match leaf.request(&args[1..]) {
        Ok(request) => request,
        Err(ParseError::Usage(message)) => {
            eprintln!("{message}");
            return Ok(2);
        }
        Err(ParseError::Io(error)) => return Err(error),
        Err(ParseError::Clap(error)) => return super::print_clap_error(&error),
    };
    // Fork (agents v2): a group rename is team structure for agents, and an
    // agent never closes a whole group.
    if let Some(code) = route_agent_request(&request)? {
        return Ok(code);
    }
    if leaf.quiet {
        super::send_ok_request(request.method)
    } else {
        super::print_response(&super::send_request(&request)?)
    }
}

/// Fork (agents v2): the agent route for a parsed `workspace` request;
/// `None` keeps the upstream path.
fn route_agent_request(request: &crate::api::schema::Request) -> std::io::Result<Option<i32>> {
    use crate::api::schema::Method;
    if !matches!(
        request.method,
        Method::WorkspaceRename(_) | Method::WorkspaceClose(_)
    ) {
        return Ok(None);
    }
    let pane = match super::agent_route::route()? {
        Err(code) => return Ok(Some(code)),
        Ok(super::agent_route::Route::Agent(pane)) => pane,
        Ok(super::agent_route::Route::User | super::agent_route::Route::OldServer) => {
            return Ok(None)
        }
    };
    match &request.method {
        Method::WorkspaceRename(params) => super::agent_route::check(
            &pane,
            crate::api::schema::agents_model::AgentsCheckAction::TeamStructure,
            Some(params.workspace_id.clone()),
        ),
        Method::WorkspaceClose(_) => Ok(Some(super::agent_route::refuse(
            crate::api::schema::agents_model::error_code::AGENT_REFUSED,
            "close tabs one by one with agents_close_tab, or ask your user",
        ))),
        _ => Ok(None),
    }
}
