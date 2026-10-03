//! ember node — the execution daemon that runs on each computer (`docs/SPEC.md` §X).
//!
//! It carries out tool actions for sessions whose agents run on ember server: file operations,
//! search, commands (with pipes or a PTY) and background jobs, and describes its environment.
//! It never runs agent CLIs or stores transcripts (FR-X5).
//!
//! - [`api`]: the authenticated HTTP/WebSocket API and [`api::serve`], generic over the listener
//!   so the transport (`INTENT.md` Q7) can be swapped without touching the API.
//! - [`client`]: the typed client ember server uses.
//! - [`proto`]: wire types shared by both.
//! - [`term`]: persistent terminal sessions (SPEC §P) — owned by the daemon, attachable by many
//!   clients, with a terminal model for redraw on attach; the `ember-term` binary
//!   (`src/bin/ember-term.rs`) attaches a local terminal (e.g. VS Code's) to one.
//!
//! Known limits: jobs live in the daemon's memory and do not survive a daemon restart (terminal
//! sessions do, best effort, through their PTY keepers — see [`term`]); the path
//! policy confines the file API and working directories, not what a command does; a remote
//! browser egress (SOCKS5, FR-R1) will be a separate listener added next to the API.

pub mod api;
pub mod client;
pub mod config;
pub mod envinfo;
pub mod exec;
pub mod exec_server;
pub mod fs;
pub mod jobs;
pub mod policy;
pub mod proto;
pub mod term;
