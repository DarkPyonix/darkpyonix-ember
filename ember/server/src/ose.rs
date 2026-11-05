//! Manages local OSE server processes for editor launches.

use std::collections::HashMap;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::api::ide::EmberOseConnection;

struct Process {
    child: Child,
    connection: EmberOseConnection,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Owns one server per local project folder.
pub struct OseManager {
    binary: Option<PathBuf>,
    processes: Arc<Mutex<HashMap<PathBuf, Process>>>,
}

impl Default for OseManager {
    fn default() -> Self {
        Self::new(server_binary())
    }
}

impl OseManager {
    pub fn new(binary: Option<PathBuf>) -> Self {
        let processes: Arc<Mutex<HashMap<PathBuf, Process>>> = Arc::new(Mutex::new(HashMap::new()));
        let weak = Arc::downgrade(&processes);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            let Some(processes) = weak.upgrade() else { break };
            let mut processes = processes.lock().unwrap_or_else(|e| e.into_inner());
            processes.retain(|_, process| matches!(process.child.try_wait(), Ok(None)));
        });
        Self { binary, processes }
    }

    pub fn connection(&self, folder: &Path) -> Result<EmberOseConnection, String> {
        let binary = self.binary.as_ref().filter(|p| p.is_file())
            .ok_or_else(|| "OSE server binary is not installed".to_string())?;
        let folder = folder.canonicalize().map_err(|e| format!("cannot open project folder: {e}"))?;
        let mut processes = self.processes.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(process) = processes.get_mut(&folder) {
            if matches!(process.child.try_wait(), Ok(None)) {
                return Ok(process.connection.clone());
            }
        }
        processes.remove(&folder);
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| format!("cannot allocate OSE port: {e}"))?;
        let address = listener.local_addr().map_err(|e| format!("cannot read OSE port: {e}"))?;
        drop(listener);
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let child = Command::new(binary)
            .args(["--host", "127.0.0.1", "--port", &address.port().to_string(), "--connection-token", &token,
                "--accept-server-license-terms", "--telemetry-level", "off"])
            .current_dir(&folder)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().map_err(|e| format!("cannot start OSE server: {e}"))?;
        let connection = EmberOseConnection { endpoint: format!("http://{address}"), connection_token: token };
        let mut process = Process { child, connection: connection.clone() };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(Some(status)) = process.child.try_wait() {
                return Err(format!("OSE server exited during startup: {status}"));
            }
            if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok() { break; }
            if Instant::now() >= deadline { return Err("OSE server did not start in time".into()); }
            std::thread::sleep(Duration::from_millis(50));
        }
        processes.insert(folder, process);
        Ok(connection)
    }
}

fn server_binary() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("EMBER_OSE_SERVER").filter(|v| !v.is_empty()) {
        return Some(path.into());
    }
    let share = PathBuf::from(std::env::var_os("HOME")?).join(".local/share");
    let mut candidates: Vec<_> = std::fs::read_dir(share).ok()?.flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("dpx-ose-"))
        .map(|e| e.path().join("bin/dpx-ose-server")).filter(|p| p.is_file()).collect();
    candidates.sort();
    candidates.pop()
}
