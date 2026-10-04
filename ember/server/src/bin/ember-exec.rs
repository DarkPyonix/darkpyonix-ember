//! `ember-exec`: runs Claude Code's Bash tool on the session's ember node. Set as
//! `CLAUDE_CODE_SHELL_PREFIX` by ember server; see `ember_server::computers::shim`.
//!
//! ```text
//! ember-exec [--local|--remote] [--] <command string>
//! ```
//!
//! Environment: `EMBER_EXEC_NODE_URL`, `EMBER_EXEC_NODE_TOKEN` (without them everything runs
//! locally), `EMBER_EXEC_REMOTE_SHELL` (default `bash`), `EMBER_EXEC_NON_TOOL`
//! (`local`|`remote`), `EMBER_EXEC_FORWARD_ENV` (comma-separated names), `EMBER_MOUNT_CTL`
//! (project mount control socket, notified after each remote command).

use std::os::unix::process::CommandExt;

use ember_server::computers::shim::{self, Plan, ShimEnv};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let inv = match shim::parse_args(&args) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("ember-exec: {e}");
            std::process::exit(2);
        }
    };
    let env = ShimEnv::from_env();
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    match shim::plan(&inv, &env, &cwd) {
        Plan::Local { command } => {
            // Exactly what the shell would have run without the prefix.
            let sh = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
            let err = std::process::Command::new(&sh).arg("-c").arg(&command).exec();
            eprintln!("ember-exec: could not run {sh}: {err}");
            std::process::exit(127);
        }
        Plan::Remote { request, cwd_file } => {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("ember-exec: {e}");
                    std::process::exit(255);
                }
            };
            let code = rt.block_on(shim::run_remote(&env, request, cwd_file));
            // The command may have changed files on the node: drop the project mount's caches
            // so Claude's next Read sees them.
            if let Some(ctl) = std::env::var_os(ember_server::computers::mount::ENV_CTL) {
                ember_server::computers::mount::notify_invalidate(std::path::Path::new(&ctl));
            }
            std::process::exit(code);
        }
    }
}
