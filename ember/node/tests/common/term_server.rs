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
}

impl TermServer {
    pub async fn start(config: NodeConfig, token: &str) -> (NodeClient, Self) {
        let node = Node::new(config).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let client = NodeClient::new(&base, token).unwrap();
        let server = tokio::spawn(api::serve(listener, node.clone()));
        (client, Self { node, server })
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

        // Use a bounded synchronous wait so cleanup also works while the test unwinds.
        // Let the exit workers finish and the keepers remove their sockets before the
        // caller's TempDir can remove the fixture directory.
        let deadline = Instant::now() + Duration::from_secs(5);
        while let Some((session, _)) = sessions.iter().find(|(session, socket)| {
            session.is_running() || socket.as_ref().is_some_and(|path| path.exists())
        }) {
            if Instant::now() >= deadline {
                eprintln!("test terminal {} is still stopping", session.id);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
