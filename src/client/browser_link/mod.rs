//! The remote browser on the user's machine (fork): the per-machine helper
//! (`herdr browser-link <machine>`), its gate, proxy and companion, and the
//! client's side of it. The frames it speaks with the remote server are in
//! [`crate::browser::link_proto`].

// the packages of the remote browser land here one by one; X1 wires them up
// and takes this out
#![allow(dead_code)]

mod forward;
mod gate;
mod proxy;
mod ssh;
