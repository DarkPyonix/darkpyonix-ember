//! FR-X1 (#521): denied paths inside a root are refused through the node API exactly like paths
//! outside the roots. The policy is not a sandbox; this only narrows the file API.

use std::path::{Path, PathBuf};

use ember_node::api::{self, Node};
use ember_node::client::{ClientError, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::proto::*;

const TOKEN: &str = "test-token";

struct Fx {
    c: NodeClient,
    root: PathBuf,
    denied: PathBuf,
    outside: PathBuf,
    _dir: tempfile::TempDir,
}

async fn start() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let root = base.join("root");
    let denied = root.join(".ember");
    let outside = base.join("outside");
    std::fs::create_dir_all(denied.join("sub")).unwrap();
    std::fs::create_dir_all(root.join(".ember-other")).unwrap();
    std::fs::create_dir_all(root.join("work")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(denied.join("token"), "s3cret needle").unwrap();
    std::fs::write(denied.join("sub/deep"), "needle").unwrap();
    std::fs::write(root.join(".ember-other/ok"), "needle").unwrap();
    std::fs::write(root.join("work/ok"), "needle").unwrap();
    std::fs::write(outside.join("secret"), "needle").unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()]).with_denied(vec![denied.clone()])).unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(api::serve(l, node));
    Fx { c: NodeClient::new(&url, TOKEN).unwrap(), root, denied, outside, _dir: dir }
}

fn grep_req(root: &Path) -> GrepRequest {
    GrepRequest {
        root: root.to_path_buf(),
        pattern: "needle".into(),
        glob: None,
        case_insensitive: false,
        respect_ignore: false,
        limit: None,
    }
}

/// (status, code, message with `path` removed)
fn refusal<T>(r: Result<T, ClientError>, path: &Path) -> (u16, ErrorCode, String) {
    match r.map(drop) {
        Ok(()) => panic!("expected a refusal for {path:?}"),
        Err(ClientError::Api { status, body }) => (status, body.code, body.error.replace(path.to_str().unwrap(), "<path>")),
        Err(e) => panic!("{e:?}"),
    }
}

#[tokio::test]
async fn fr_x1_policy_denied_refuses_every_file_operation_like_outside_roots() {
    let f = start().await;
    let c = &f.c;
    let token = f.denied.join("token");
    let want = refusal(c.read_file(f.outside.join("secret")).await, &f.outside.join("secret"));
    assert_eq!((want.0, want.1), (403, ErrorCode::ForbiddenPath));

    assert_eq!(refusal(c.read_file(&token).await, &token), want);
    assert_eq!(refusal(c.stat(&token).await, &token), want);
    assert_eq!(refusal(c.stat(&f.denied).await, &f.denied), want);
    assert_eq!(refusal(c.list(&f.denied).await, &f.denied), want);
    assert_eq!(refusal(c.list(f.denied.join("sub")).await, &f.denied.join("sub")), want);
    let new = f.denied.join("new");
    assert_eq!(refusal(c.write_file(&new, "x", None).await, &new), want);
    let deep = f.denied.join("missing/dir/new");
    assert_eq!(refusal(c.write_file(&deep, "x", None).await, &deep), want);
    assert!(!new.exists() && !f.denied.join("missing").exists());
    let g = GlobRequest { root: f.denied.clone(), pattern: "**/*".into(), respect_ignore: false, limit: None };
    assert_eq!(refusal(c.glob(&g).await, &f.denied), want);
    assert_eq!(refusal(c.grep(&grep_req(&f.denied)).await, &f.denied), want);
    assert_eq!(refusal(c.grep(&grep_req(&token)).await, &token), want);
}

#[tokio::test]
async fn fr_x1_policy_denied_siblings_and_prefix_lookalikes_are_allowed() {
    let f = start().await;
    let c = &f.c;
    assert!(c.read_file(f.root.join("work/ok")).await.is_ok());
    assert!(c.read_file(f.root.join(".ember-other/ok")).await.is_ok());
    assert!(c.list(&f.root).await.is_ok());
    assert!(c.write_file(f.root.join(".ember-other/new"), "x", None).await.is_ok());
    let hits = c.grep(&grep_req(&f.root)).await.unwrap();
    // A search over the root finds the allowed files and never reads inside the denied dir.
    assert!(hits.matches.iter().any(|m| m.path == f.root.join("work/ok")));
    assert!(hits.matches.iter().any(|m| m.path == f.root.join(".ember-other/ok")));
    assert!(hits.matches.iter().all(|m| !m.path.starts_with(&f.denied)), "{hits:?}");
    let g = GlobRequest { root: f.root.clone(), pattern: "**/*".into(), respect_ignore: false, limit: None };
    let found = c.glob(&g).await.unwrap();
    assert!(found.paths.contains(&f.root.join("work/ok")));
    assert!(found.paths.iter().all(|p| !p.starts_with(&f.denied)), "{found:?}");
}

#[tokio::test]
async fn fr_x1_policy_denied_symlink_into_the_denied_dir_and_dotdot_are_refused() {
    let f = start().await;
    let c = &f.c;
    std::os::unix::fs::symlink(&f.denied, f.root.join("work/link")).unwrap();
    std::os::unix::fs::symlink(f.denied.join("token"), f.root.join("work/tlink")).unwrap();
    let want = refusal(c.read_file(f.outside.join("secret")).await, &f.outside.join("secret"));

    for p in [f.root.join("work/link/token"), f.root.join("work/tlink"), f.root.join("work/../.ember/token")] {
        assert_eq!(refusal(c.read_file(&p).await, &p), want, "{p:?}");
    }
    let p = f.root.join("work/link");
    assert_eq!(refusal(c.list(&p).await, &p), want);
    let p = f.root.join("work/link/new");
    assert_eq!(refusal(c.write_file(&p, "x", None).await, &p), want);
    assert!(!f.denied.join("new").exists());
}
