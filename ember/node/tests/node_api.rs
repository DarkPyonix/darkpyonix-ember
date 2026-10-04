//! In-process integration tests: the daemon on an ephemeral port, driven through the client.
//! Covers FR-X1 (files, search, exec, PTY), FR-X4 (jobs and completion events), the path
//! policy and authentication.

use std::path::PathBuf;
use std::time::Duration;

use ember_node::api::{self, Node};
use ember_node::client::{shell, ClientError, NodeClient};
use ember_node::config::NodeConfig;
use ember_node::proto::*;

const TOKEN: &str = "test-token";

struct Fixture {
    client: NodeClient,
    base: String,
    /// Allowed root (canonical).
    root: PathBuf,
    /// A sibling directory outside the root.
    outside: PathBuf,
    _dir: tempfile::TempDir,
}

async fn start() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base_dir = dir.path().canonicalize().unwrap();
    let root = base_dir.join("root");
    let outside = base_dir.join("outside");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let node = Node::new(NodeConfig::new(TOKEN, vec![root.clone()])).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(api::serve(listener, node));
    Fixture { client: NodeClient::new(&base, TOKEN).unwrap(), base, root, outside, _dir: dir }
}

async fn within<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(20), fut).await.expect("timed out")
}

#[tokio::test]
async fn health_is_open_but_everything_else_needs_the_token() {
    let f = start().await;
    let h = NodeClient::new(&f.base, "wrong").unwrap().health().await.unwrap();
    assert!(h.ok);
    assert_eq!(h.protocol, PROTOCOL_VERSION);

    for token in ["wrong", ""] {
        let bad = NodeClient::new(&f.base, token).unwrap();
        let e = bad.stat(&f.root).await.unwrap_err();
        assert!(matches!(&e, ClientError::Api { status: 401, .. }), "{e:?}");
        assert_eq!(e.code(), Some(ErrorCode::Unauthorized));
        // WebSocket upgrades are refused too.
        let e = bad.exec(&ExecRequest { command: shell("true", &f.root), pty: None }).await.err().unwrap();
        assert!(matches!(e, ClientError::Api { status: 401, .. }), "{e:?}");
        let e = bad.events(0).await.err().unwrap();
        assert!(matches!(e, ClientError::Api { status: 401, .. }), "{e:?}");
    }

    // Raw request without any Authorization header.
    let r = reqwest::Client::new().get(format!("{}/v1/jobs", f.base)).send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert!(f.client.stat(&f.root).await.is_ok());
}

#[tokio::test]
async fn read_write_with_hash_precondition() {
    let f = start().await;
    let c = &f.client;
    let path = f.root.join("src/main.rs");

    // Create-only write fails without create_parents, then succeeds with it.
    let e = c.write_file(&path, "fn main() {}\n", Some(Expect::Absent)).await.unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::NotFound), "{e:?}");
    let w = c
        .write(&WriteRequest {
            path: path.clone(),
            data: b"fn main() {}\n".to_vec(),
            expect: Some(Expect::Absent),
            create_parents: true,
        })
        .await
        .unwrap();
    assert_eq!(w.size, 13);

    // Read returns content and the whole-file hash.
    let r = c.read_file(&path).await.unwrap();
    assert_eq!(r.data, b"fn main() {}\n");
    assert_eq!(r.sha256.as_deref(), Some(w.sha256.as_str()));
    assert!(r.eof);

    // Byte range: the hash still covers the whole file.
    let part = c.read(&ReadRequest { path: path.clone(), offset: 3, len: Some(4), hash: true }).await.unwrap();
    assert_eq!(part.data, b"main");
    assert!(!part.eof);
    assert_eq!(part.size, 13);
    assert_eq!(part.sha256, r.sha256);

    // Create-only on an existing file fails and reports the current hash.
    let e = c.write_file(&path, "x", Some(Expect::Absent)).await.unwrap_err();
    assert_eq!(e.precondition_actual(), Some(Some(w.sha256.as_str())));

    // Edit with the hash we read: succeeds.
    let w2 = c.write_file(&path, "fn main() { run() }\n", Some(Expect::Sha256(w.sha256.clone()))).await.unwrap();

    // A second editor still holding the old hash is rejected; the file keeps the first edit.
    let e = c.write_file(&path, "stale", Some(Expect::Sha256(w.sha256.clone()))).await.unwrap_err();
    assert!(matches!(&e, ClientError::Api { status: 412, .. }), "{e:?}");
    assert_eq!(e.precondition_actual(), Some(Some(w2.sha256.as_str())));
    assert_eq!(std::fs::read(&path).unwrap(), b"fn main() { run() }\n");

    // No temp files are left behind.
    let names: Vec<_> = c.list(f.root.join("src")).await.unwrap().entries.into_iter().map(|e| e.name).collect();
    assert_eq!(names, vec!["main.rs"]);

    // Existing permission bits survive an atomic replace.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    c.write_file(&path, "#!/bin/sh\n", None).await.unwrap();
    assert_eq!(c.stat(&path).await.unwrap().mode & 0o777, 0o755);
}

#[tokio::test]
async fn list_glob_and_grep() {
    let f = start().await;
    let c = &f.client;
    std::fs::create_dir_all(f.root.join("src/nested")).unwrap();
    std::fs::create_dir_all(f.root.join("target")).unwrap();
    std::fs::write(f.root.join(".gitignore"), "target/\n").unwrap();
    std::fs::write(f.root.join("src/a.rs"), "fn alpha() {}\n// TODO one\n").unwrap();
    std::fs::write(f.root.join("src/nested/b.rs"), "fn beta() {}\n// todo two\n").unwrap();
    std::fs::write(f.root.join("src/c.txt"), "TODO three\n").unwrap();
    std::fs::write(f.root.join("target/gen.rs"), "// TODO ignored\n").unwrap();

    let l = c.list(f.root.join("src")).await.unwrap();
    let names: Vec<_> = l.entries.iter().map(|e| (e.name.as_str(), e.kind)).collect();
    assert_eq!(names, vec![("a.rs", FileKind::File), ("c.txt", FileKind::File), ("nested", FileKind::Dir)]);

    let g = c
        .glob(&GlobRequest { root: f.root.clone(), pattern: "**/*.rs".into(), respect_ignore: true, limit: None })
        .await
        .unwrap();
    let mut got: Vec<_> = g.paths.iter().map(|p| p.strip_prefix(&f.root).unwrap().to_path_buf()).collect();
    got.sort();
    assert_eq!(got, vec![PathBuf::from("src/a.rs"), PathBuf::from("src/nested/b.rs")]);

    // `*` does not cross directories.
    let g = c
        .glob(&GlobRequest { root: f.root.join("src"), pattern: "*.rs".into(), respect_ignore: true, limit: None })
        .await
        .unwrap();
    assert_eq!(g.paths, vec![f.root.join("src/a.rs")]);

    // Ignored files are skipped unless asked for.
    let g = c
        .glob(&GlobRequest { root: f.root.clone(), pattern: "**/*.rs".into(), respect_ignore: false, limit: None })
        .await
        .unwrap();
    assert_eq!(g.paths.len(), 3);

    let grep = |pattern: &str, glob: Option<&str>, ci: bool| GrepRequest {
        root: f.root.clone(),
        pattern: pattern.into(),
        glob: glob.map(Into::into),
        case_insensitive: ci,
        respect_ignore: true,
        limit: None,
    };
    let r = c.grep(&grep("TODO", None, false)).await.unwrap();
    let mut hits: Vec<_> = r.matches.iter().map(|m| (m.path.file_name().unwrap().to_str().unwrap().to_string(), m.line)).collect();
    hits.sort();
    assert_eq!(hits, vec![("a.rs".into(), 2), ("c.txt".into(), 1)]);

    let r = c.grep(&grep("todo", Some("*.rs"), true)).await.unwrap();
    assert_eq!(r.matches.len(), 2);
    assert!(r.matches.iter().all(|m| m.path.extension().unwrap() == "rs"));
    assert!(r.matches.iter().any(|m| m.text == "// todo two"));

    let mut req = grep("fn|TODO", None, false);
    req.limit = Some(1);
    let r = c.grep(&req).await.unwrap();
    assert_eq!(r.matches.len(), 1);
    assert!(r.truncated);

    let e = c.grep(&grep("(", None, false)).await.unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::BadRequest));
}

#[tokio::test]
async fn path_policy_rejects_paths_outside_the_roots() {
    let f = start().await;
    let c = &f.client;
    std::fs::write(f.outside.join("secret"), "s3cret").unwrap();
    std::os::unix::fs::symlink(&f.outside, f.root.join("escape")).unwrap();

    let forbidden = |r: Result<(), ClientError>| {
        let e = r.unwrap_err();
        assert_eq!(e.code(), Some(ErrorCode::ForbiddenPath), "{e:?}");
        assert!(matches!(e, ClientError::Api { status: 403, .. }));
    };
    forbidden(c.read_file(f.outside.join("secret")).await.map(drop));
    forbidden(c.read_file(f.root.join("../outside/secret")).await.map(drop));
    forbidden(c.read_file(f.root.join("escape/secret")).await.map(drop));
    forbidden(c.stat(&f.outside).await.map(drop));
    forbidden(c.list(f.root.join("escape")).await.map(drop));
    forbidden(c.write_file(f.outside.join("new"), "x", None).await.map(drop));
    forbidden(c.write_file(f.root.join("escape/new"), "x", None).await.map(drop));
    forbidden(
        c.grep(&GrepRequest {
            root: f.outside.clone(),
            pattern: "s3".into(),
            glob: None,
            case_insensitive: false,
            respect_ignore: true,
            limit: None,
        })
        .await
        .map(drop),
    );
    assert!(!f.outside.join("new").exists());

    // Relative paths are refused outright.
    let e = c.read_file("relative/path").await.unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::BadRequest));

    // Commands and jobs may not start outside the roots either.
    let e = c.exec(&ExecRequest { command: shell("pwd", &f.outside), pty: None }).await.err().unwrap();
    assert!(e.to_string().contains("outside the allowed roots"), "{e}");
    let e = c.start_job(&JobRequest { command: shell("pwd", &f.outside), label: None }).await.unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::ForbiddenPath));
}

#[tokio::test]
async fn exec_streams_stdout_stderr_and_exit_status() {
    let f = start().await;
    within(async {
        let mut cmd = shell("echo out; echo err >&2; printf '%s' \"$EMBER_X\"; pwd; exit 3", &f.root);
        cmd.env.insert("EMBER_X".into(), "from-env\n".into());
        let out = f.client.run(cmd).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), format!("out\nfrom-env\n{}\n", f.root.display()));
        assert_eq!(out.stderr, b"err\n");
        assert_eq!(out.code, Some(3));
        assert_eq!(out.signal, None);

        // Output arrives as it is produced, not only at exit.
        let mut s = f
            .client
            .exec(&ExecRequest { command: shell("echo first; sleep 0.5; echo second", &f.root), pty: None })
            .await
            .unwrap();
        let t0 = std::time::Instant::now();
        let ev = s.recv().await.unwrap().unwrap();
        assert_eq!(ev, ExecEvent::Stdout { data: b"first\n".to_vec() });
        assert!(t0.elapsed() < Duration::from_millis(400), "first chunk was not streamed");
        let mut rest = Vec::new();
        while let Some(ev) = s.recv().await.unwrap() {
            rest.push(ev);
        }
        assert_eq!(rest.last(), Some(&ExecEvent::Exit { code: Some(0), signal: None }));

        // Stdin is forwarded; closing it ends `cat`.
        let mut s = f
            .client
            .exec(&ExecRequest {
                command: CommandSpec { program: Program::Argv(vec!["cat".into()]), ..shell("", &f.root) },
                pty: None,
            })
            .await
            .unwrap();
        s.tx.stdin("ping\n").await.unwrap();
        assert_eq!(s.recv().await.unwrap(), Some(ExecEvent::Stdout { data: b"ping\n".to_vec() }));
        s.send(ExecInput::CloseStdin).await.unwrap();
        assert_eq!(s.recv().await.unwrap(), Some(ExecEvent::Exit { code: Some(0), signal: None }));

        // Kill signals the whole process group.
        let mut s = f.client.exec(&ExecRequest { command: shell("sleep 30", &f.root), pty: None }).await.unwrap();
        s.send(ExecInput::Kill { signal: None }).await.unwrap();
        assert_eq!(s.recv().await.unwrap(), Some(ExecEvent::Exit { code: None, signal: Some(libc_sigterm()) }));

        // A missing program is reported, not hung.
        let e = f
            .client
            .exec(&ExecRequest {
                command: CommandSpec { program: Program::Argv(vec!["/nonexistent/bin".into()]), ..shell("", &f.root) },
                pty: None,
            })
            .await
            .err()
            .unwrap();
        assert!(e.to_string().contains("spawn"), "{e}");
    })
    .await;
}

fn libc_sigterm() -> i32 {
    15
}

#[tokio::test]
async fn dropping_the_exec_connection_kills_the_command() {
    let f = start().await;
    within(async {
        let marker = f.root.join("survived");
        let s = f
            .client
            .exec(&ExecRequest { command: shell(format!("sleep 1; touch {}", marker.display()), &f.root), pty: None })
            .await
            .unwrap();
        drop(s);
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!marker.exists(), "foreground command outlived its connection");
    })
    .await;
}

/// Collect PTY output until `needle` appears.
async fn pty_until(s: &mut ember_node::client::ExecSession, out: &mut String, needle: &str) {
    while !out.contains(needle) {
        match s.recv().await.unwrap() {
            Some(ExecEvent::Stdout { data }) => out.push_str(&String::from_utf8_lossy(&data)),
            Some(other) => panic!("unexpected {other:?} while waiting for {needle:?}; output so far {out:?}"),
            None => panic!("closed while waiting for {needle:?}; output so far {out:?}"),
        }
    }
}

#[tokio::test]
async fn pty_echoes_input_resizes_and_reports_a_tty() {
    let f = start().await;
    within(async {
        let mut s = f
            .client
            .exec(&ExecRequest {
                command: CommandSpec { program: Program::Argv(vec!["/bin/sh".into()]), ..shell("", &f.root) },
                pty: Some(PtySize { rows: 24, cols: 80 }),
            })
            .await
            .unwrap();
        let mut out = String::new();
        s.tx.stdin("stty size; test -t 0 && echo IS_A_TTY\n").await.unwrap();
        pty_until(&mut s, &mut out, "24 80").await;
        pty_until(&mut s, &mut out, "IS_A_TTY\r\n").await;

        s.send(ExecInput::Resize { size: PtySize { rows: 40, cols: 100 } }).await.unwrap();
        s.tx.stdin("stty size\n").await.unwrap();
        pty_until(&mut s, &mut out, "40 100").await;

        // The terminal echoes typed input back, as an interactive program expects.
        out.clear();
        s.tx.stdin("echo he''llo\n").await.unwrap();
        pty_until(&mut s, &mut out, "echo he''llo").await;
        pty_until(&mut s, &mut out, "hello\r\n").await;

        s.tx.stdin("exit 7\n").await.unwrap();
        loop {
            match s.recv().await.unwrap() {
                Some(ExecEvent::Stdout { .. }) => continue,
                Some(ev) => {
                    assert_eq!(ev, ExecEvent::Exit { code: Some(7), signal: None });
                    break;
                }
                None => panic!("closed before exit"),
            }
        }
    })
    .await;
}

#[tokio::test]
async fn background_job_outlives_its_connection_and_reports_completion() {
    let f = start().await;
    within(async {
        // Subscribe, start a job, then drop the subscription mid-job (the server moved away).
        let mut events = f.client.events(0).await.unwrap();
        let job = f
            .client
            .start_job(&JobRequest {
                command: shell("echo building; sleep 1; echo done; exit 4", &f.root),
                label: Some("session-1".into()),
            })
            .await
            .unwrap();
        assert_eq!(job.state, JobState::Running);
        let ev = events.recv().await.unwrap().unwrap();
        assert!(matches!(&ev.kind, NodeEventKind::JobStarted { job: j } if j.id == job.id));
        let last_seen = ev.seq;
        drop(events);

        // Still running and visible in the listing.
        let listed = f.client.jobs().await.unwrap();
        assert!(listed.iter().any(|j| j.id == job.id && j.state == JobState::Running));

        // Reconnect later and receive the completion that happened while disconnected.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let mut events = f.client.events(last_seen).await.unwrap();
        let ev = events.recv().await.unwrap().unwrap();
        assert!(ev.seq > last_seen);
        match ev.kind {
            NodeEventKind::JobFinished { job: done, tail } => {
                assert_eq!(done.id, job.id);
                assert_eq!(done.state, JobState::Exited);
                assert_eq!(done.exit_code, Some(4));
                assert_eq!(done.label.as_deref(), Some("session-1"));
                assert_eq!(tail, "building\ndone\n");
            }
            other => panic!("expected JobFinished, got {other:?}"),
        }

        let detail = f.client.job(&job.id, Some(5)).await.unwrap();
        assert_eq!(detail.tail, "done\n");
        assert_eq!(detail.info.output_bytes, 14);
        f.client.remove_job(&job.id).await.unwrap();
        assert_eq!(f.client.job(&job.id, None).await.unwrap_err().code(), Some(ErrorCode::NotFound));
    })
    .await;
}

#[tokio::test]
async fn running_job_can_be_killed_and_completion_is_pushed_live() {
    let f = start().await;
    within(async {
        let mut events = f.client.events(0).await.unwrap();
        let job = f.client.start_job(&JobRequest { command: shell("sleep 30", &f.root), label: None }).await.unwrap();
        // A running job cannot be removed.
        assert!(matches!(f.client.remove_job(&job.id).await.unwrap_err(), ClientError::Api { status: 409, .. }));
        f.client.kill_job(&job.id, None).await.unwrap();
        loop {
            let ev = events.recv().await.unwrap().unwrap();
            if let NodeEventKind::JobFinished { job: done, .. } = ev.kind {
                assert_eq!(done.id, job.id);
                assert_eq!(done.state, JobState::Signaled);
                assert_eq!(done.signal, Some(15));
                break;
            }
        }
    })
    .await;
}

#[tokio::test]
async fn env_describes_the_computer() {
    let f = start().await;
    let e = f.client.env().await.unwrap();
    assert_eq!(e.os, std::env::consts::OS);
    assert_eq!(e.arch, std::env::consts::ARCH);
    assert!(!e.hostname.is_empty());
    assert_eq!(e.roots, vec![f.root.clone()]);
    assert!(e.kernel.is_some());
    // `sh` is not probed, but every test machine has git.
    let git = e.toolchains.get("git").expect("git on PATH");
    assert!(git.version.as_deref().unwrap_or("").starts_with("git version"), "{git:?}");
}

/// The namespace and attribute operations the project mount needs (lstat, readlink, symlink,
/// mkdir, remove, rename, setattr, pwrite), their errno names, and the path policy on each.
#[tokio::test]
async fn namespace_operations_for_the_mount() {
    use std::os::unix::fs::PermissionsExt;
    let f = start().await;
    let c = &f.client;
    let root = &f.root;

    // mkdir: single level needs the parent; `parents` is mkdir -p and idempotent.
    let e = c.mkdir(&MkdirRequest { path: root.join("a/b"), parents: false, mode: None }).await.unwrap_err();
    assert_eq!(e.errno(), Some("ENOENT"), "{e:?}");
    let s = c.mkdir(&MkdirRequest { path: root.join("a/b"), parents: true, mode: Some(0o750) }).await.unwrap();
    assert_eq!(s.kind, FileKind::Dir);
    assert_eq!(s.mode & 0o777, 0o750);
    assert!(s.ino.is_some());
    c.mkdir(&MkdirRequest { path: root.join("a/b"), parents: true, mode: None }).await.unwrap();
    let e = c.mkdir(&MkdirRequest { path: root.join("a/b"), parents: false, mode: None }).await.unwrap_err();
    assert_eq!(e.errno(), Some("EEXIST"));
    assert!(matches!(e, ClientError::Api { status: 409, .. }), "{e:?}");

    // pwrite: in place, at an offset, existing files only.
    c.write_file(root.join("a/f.txt"), "hello world", None).await.unwrap();
    let s = c.pwrite(root.join("a/f.txt"), 6, "WORLD!").await.unwrap();
    assert_eq!(s.size, 12);
    assert_eq!(std::fs::read(root.join("a/f.txt")).unwrap(), b"hello WORLD!");
    let e = c.pwrite(root.join("a/missing"), 0, "x").await.unwrap_err();
    assert_eq!(e.errno(), Some("ENOENT"));

    // setattr: truncate, chmod, utimes in one call.
    let s = c
        .setattr(&SetAttrRequest {
            path: root.join("a/f.txt"),
            size: Some(5),
            mode: Some(0o600),
            atime: Some(SetTime::UnixMs(1_000_000_000_000)),
            mtime: Some(SetTime::UnixMs(1_000_000_123_000)),
        })
        .await
        .unwrap();
    assert_eq!(s.size, 5);
    assert_eq!(s.mode & 0o777, 0o600);
    assert_eq!(s.mtime_ms, Some(1_000_000_123_000));
    assert_eq!(std::fs::read(root.join("a/f.txt")).unwrap(), b"hello");
    assert_eq!(std::fs::metadata(root.join("a/f.txt")).unwrap().permissions().mode() & 0o777, 0o600);
    let s = c
        .setattr(&SetAttrRequest { path: root.join("a/f.txt"), mtime: Some(SetTime::Now), ..Default::default() })
        .await
        .unwrap();
    assert!(s.mtime_ms.unwrap() > 1_000_000_123_000);
    // Extending fills with zeros.
    let s = c.setattr(&SetAttrRequest { path: root.join("a/f.txt"), size: Some(8), ..Default::default() }).await.unwrap();
    assert_eq!(s.size, 8);
    assert_eq!(std::fs::read(root.join("a/f.txt")).unwrap(), b"hello\0\0\0");

    // symlink / readlink / lstat: the link itself, never followed, even when it points outside.
    std::fs::write(f.outside.join("secret"), "s3cret").unwrap();
    let s = c.symlink(root.join("a/out"), f.outside.join("secret")).await.unwrap();
    assert_eq!(s.kind, FileKind::Symlink);
    let l = c.readlink(root.join("a/out")).await.unwrap();
    assert_eq!(l.target, f.outside.join("secret"));
    assert_eq!(c.lstat(root.join("a/out")).await.unwrap().kind, FileKind::Symlink);
    // stat follows it, so it is refused; reading through it too.
    assert_eq!(c.stat(root.join("a/out")).await.unwrap_err().code(), Some(ErrorCode::ForbiddenPath));
    assert_eq!(c.read_file(root.join("a/out")).await.unwrap_err().errno(), Some("EACCES"));
    c.symlink(root.join("a/rel"), "f.txt").await.unwrap();
    assert_eq!(c.readlink(root.join("a/rel")).await.unwrap().target, PathBuf::from("f.txt"));
    assert_eq!(c.stat(root.join("a/rel")).await.unwrap().kind, FileKind::File);
    let e = c.symlink(f.outside.join("planted"), root.join("a/f.txt")).await.unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::ForbiddenPath));

    // list reports entry modes without following links.
    let l = c.list(root.join("a")).await.unwrap();
    let out = l.entries.iter().find(|e| e.name == "out").unwrap();
    assert_eq!(out.kind, FileKind::Symlink);
    assert!(out.mode.is_some() && out.ino.is_some());

    // rename: files, directories, overwrite control, and links move as links.
    c.rename(&RenameRequest { from: root.join("a/f.txt"), to: root.join("a/b/g.txt"), overwrite: true }).await.unwrap();
    assert!(!root.join("a/f.txt").exists());
    assert_eq!(std::fs::read(root.join("a/b/g.txt")).unwrap(), b"hello\0\0\0");
    c.write_file(root.join("a/h.txt"), "h", None).await.unwrap();
    let e = c
        .rename(&RenameRequest { from: root.join("a/h.txt"), to: root.join("a/b/g.txt"), overwrite: false })
        .await
        .unwrap_err();
    assert_eq!(e.errno(), Some("EEXIST"));
    c.rename(&RenameRequest { from: root.join("a/out"), to: root.join("a/out2"), overwrite: true }).await.unwrap();
    assert!(std::fs::symlink_metadata(root.join("a/out2")).unwrap().file_type().is_symlink());
    assert!(f.outside.join("secret").exists());
    let e = c
        .rename(&RenameRequest { from: root.join("a/h.txt"), to: f.outside.join("stolen"), overwrite: true })
        .await
        .unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::ForbiddenPath));
    c.rename(&RenameRequest { from: root.join("a/b"), to: root.join("a/c"), overwrite: true }).await.unwrap();
    assert!(root.join("a/c/g.txt").exists());

    // remove: a link (not its target), an empty dir, a non-empty dir only when recursive, never
    // a root.
    c.remove(root.join("a/out2"), false).await.unwrap();
    assert!(f.outside.join("secret").exists());
    let e = c.remove(root.join("a/c"), false).await.unwrap_err();
    assert_eq!(e.errno(), Some("ENOTEMPTY"));
    c.remove(root.join("a/c"), true).await.unwrap();
    assert!(!root.join("a/c").exists());
    let e = c.remove(root.join("a/nope"), false).await.unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::NotFound));
    assert_eq!(e.errno(), Some("ENOENT"));
    let e = c.remove(root.clone(), true).await.unwrap_err();
    assert_eq!(e.errno(), Some("EBUSY"));
    assert!(root.exists());
    let e = c.remove(f.outside.join("secret"), false).await.unwrap_err();
    assert_eq!(e.code(), Some(ErrorCode::ForbiddenPath));
    assert!(f.outside.join("secret").exists());
}
