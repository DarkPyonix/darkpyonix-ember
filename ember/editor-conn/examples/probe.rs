//! Probe of an OSE server the way the editor connects to it: the version gate, a management
//! connection (upgrade, auth, sign, connection type), the environment data, and the extension scan
//! (which reads the server's extension folders: it fails when the server's install or data directory
//! was deleted under it). Prints one line and exits 0 when all of it works, otherwise names the step.
//!
//! ```sh
//! cargo run --release --example probe -- 127.0.0.1:18765
//! ```
//! Opens one management connection and closes it; it does not open a workspace or an extension host.

use std::time::Duration;

use ember_editor_conn::handshake::{self, ConnectOptions, ConnectionType};
use ember_editor_conn::ipc::IpcClient;
use ember_editor_conn::management::{self, RemoteAgentConnectionContext};
use ember_editor_conn::PINNED_COMMIT;
use tokio::net::TcpStream;

async fn within<T>(step: &str, secs: u64, fut: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(Duration::from_secs(secs), fut).await {
        Ok(v) => v,
        Err(_) => fail(step, &format!("timed out after {secs} s")),
    }
}

fn fail(step: &str, why: &str) -> ! {
    eprintln!("PROBE FAIL at {step}: {why}");
    std::process::exit(1);
}

async fn dial(addr: &str) -> TcpStream {
    match TcpStream::connect(addr).await {
        Ok(s) => {
            let _ = s.set_nodelay(true);
            s
        }
        Err(e) => fail("tcp connect", &format!("{addr}: {e}")),
    }
}

#[tokio::main]
async fn main() {
    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:18765".to_string());
    let base = ConnectOptions::new(addr.clone());

    let commit = within("GET /version", 15, async { handshake::verify_server(dial(&addr).await, &base).await })
        .await
        .unwrap_or_else(|e| fail("version gate", &e.to_string()));
    if commit != PINNED_COMMIT {
        fail("version gate", &format!("the server is {commit}, the editor is pinned to {PINNED_COMMIT}"));
    }

    let mut opts = base.clone();
    opts.commit = Some(commit);
    opts.reconnection_token = uuid::Uuid::new_v4().to_string();
    let (mconn, first) = within("management handshake", 20, async {
        handshake::connect(dial(&addr).await, &opts, ConnectionType::Management, None).await
    })
    .await
    .unwrap_or_else(|e| fail("management handshake", &e.to_string()));
    if first["type"] != "ok" {
        fail("management handshake", &format!("the server answered {first}"));
    }

    let ctx = RemoteAgentConnectionContext { remote_authority: addr.clone(), client_id: format!("ember-probe-{}", std::process::id()) }
        .to_ipc()
        .unwrap_or_else(|e| fail("ipc context", &e.to_string()));
    let (ipc, _lifecycle) = IpcClient::start(mconn, ctx);
    let env = within("getEnvironmentData", 20, management::get_environment_data(&ipc, &addr, None))
        .await
        .unwrap_or_else(|e| fail("getEnvironmentData", &e.to_string()));
    let extensions = within("scanExtensions", 60, management::scan_extensions(&ipc, "ko"))
        .await
        .unwrap_or_else(|e| fail("scanExtensions (the server's extension folders)", &e.to_string()));
    if extensions.is_empty() {
        fail("scanExtensions", "the server lists no extensions: its extension folders are empty or unreadable");
    }
    println!("PROBE OK: {addr} Code-OSS {PINNED_COMMIT}, appRoot {}, {} extensions", env.app_root.path, extensions.len());
}
