//! A test node that owns the terminal sessions it starts or re-adopts in its fixture directory.

use std::time::{Duration, Instant};

use ember_node::api::{self, Node};
use ember_node::client::NodeClient;
use ember_node::config::NodeConfig;
use ember_node::exec::SIGKILL;
use ember_node::proto::TermListQuery;
use ember_node::term::{Terms, pty};
use tokio::task::JoinHandle;

pub struct TermServer {
    node: Node,
    server: JoinHandle<std::io::Result<()>>,
    #[cfg(unix)]
    keepers: Option<KeeperProcesses>,
}

impl TermServer {
    pub async fn start(config: NodeConfig, token: &str) -> (NodeClient, Self) {
        #[cfg(unix)]
        let keepers = config.state_dir
            .as_ref()
            .zip(config.pty_keeper.as_ref())
            .map(|(state, executable)| KeeperProcesses::new(state.join("terms"), executable.clone()));
        let node = Node::new(config).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let client = NodeClient::new(&base, token).unwrap();
        let server = tokio::spawn(api::serve(listener, node.clone()));
        (
            client,
            Self {
                node,
                server,
                #[cfg(unix)]
                keepers,
            },
        )
    }

    pub fn terms(&self) -> &Terms {
        self.node.terms()
    }
}

impl Drop for TermServer {
    fn drop(&mut self) {
        self.server.abort();
        let terms = self.terms();
        // A handler may already have queued a blocking create. Close that path before
        // listing terms. A create already holding the sessions lock finishes before list.
        terms.set_max_running(0);
        let sessions: Vec<_> = terms
            .list(&TermListQuery::default())
            .iter()
            .filter_map(|term| terms.get(&term.id))
            .map(|session| {
                let socket = session.record().keeper_socket;
                (session, socket)
            })
            .collect();
        for (session, socket) in &sessions {
            session.kill(Some(SIGKILL));
            if let Some(socket) = socket {
                pty::stop_keeper(socket);
            }
        }

        // Wait for processes as well as sockets. A keeper can remain alive after its socket
        // disappears. Keep the wait synchronous so it also runs while a test unwinds.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let stopping = sessions.iter().find(|(session, socket)| {
                session.is_running() || socket.as_ref().is_some_and(|path| path.exists())
            });
            #[cfg(unix)]
            let keepers = self.keepers.as_mut().map_or_else(Vec::new, KeeperProcesses::running);
            #[cfg(not(unix))]
            let keepers: Vec<u32> = Vec::new();
            if stopping.is_none() && keepers.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                #[cfg(unix)]
                if let Some(processes) = self.keepers.as_ref() {
                    processes.kill_and_wait(&keepers);
                }
                let session = stopping.map(|(session, _)| &session.id);
                let error = format!("terminal fixture cleanup timed out (session {session:?}, keepers {keepers:?})");
                if std::thread::panicking() {
                    eprintln!("{error}");
                } else {
                    panic!("{error}");
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(unix)]
struct KeeperProcesses {
    socket_dir: std::path::PathBuf,
    executable: std::path::PathBuf,
    system: sysinfo::System,
}

#[cfg(unix)]
impl KeeperProcesses {
    fn new(socket_dir: std::path::PathBuf, executable: std::path::PathBuf) -> Self {
        Self { socket_dir, executable, system: sysinfo::System::new() }
    }

    fn running(&mut self) -> Vec<u32> {
        use std::path::Path;
        use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, UpdateKind};
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        );
        self.system
            .processes()
            .values()
            .filter(|process| process.status() != ProcessStatus::Zombie)
            .filter(|process| {
                matches!(
                    process.cmd(),
                    [exe, command, socket_flag, socket, ..]
                        if Path::new(exe) == self.executable.as_path()
                            && command == pty::KEEPER_SUBCOMMAND
                            && socket_flag == "--socket"
                            && Path::new(socket).parent() == Some(self.socket_dir.as_path())
                )
            })
            .map(|process| process.pid().as_u32())
            .collect()
    }

    fn kill_and_wait(&self, pids: &[u32]) {
        for &pid in pids {
            if let Some(process) = self.system.process(sysinfo::Pid::from_u32(pid)) {
                process.kill();
            }
        }

        // spawn_keeper owns the child handle and joins the process in its reaper thread.
        let deadline = Instant::now() + Duration::from_secs(2);
        while pids.iter().any(|&pid| pty::alive(pid)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
