//! The remote browser on the user's machine (fork): the per-machine helper
//! (`herdr browser-link <machine>`), its gate, proxy and companion, and the
//! client's side of it. The frames it speaks with the remote server are in
//! [`crate::browser::link_proto`].

mod api_socket;
pub(crate) mod classify;
pub(crate) mod client;
mod clients;
mod companion;
mod files;
mod forward;
mod gate;
pub(crate) mod helper;
pub(crate) mod links;
mod parts;
mod proxy;
mod sidecar;
mod ssh;

/// `herdr browser-link <machine>`: the helper of one saved machine (its
/// catalog id or label), started by a client that shows it. Its log goes
/// to stderr, which the client points at the machine's `helper.log`.
pub fn run(args: &[String]) -> std::io::Result<()> {
    let [machine] = args else {
        eprintln!("usage: herdr browser-link <machine>");
        std::process::exit(2);
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("HERDR_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("herdr=info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
    helper::main(machine, parts::installed)
}
