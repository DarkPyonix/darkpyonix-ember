use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use ember_server::accounts::Accounts;
use ember_server::agents::antigravity::{self, AntigravityAdapter, HookBroker};
use ember_server::agents::claude_code::ClaudeCodeAdapter;
use ember_server::agents::codex::CodexAdapter;
use ember_server::agents::scripted::ScriptedAdapter;
use ember_server::agents::acp::{self, AcpAdapter};
use ember_server::a2a::{A2a, A2aConfig, A2aStore};
use ember_server::agents::AgentAdapter;
use ember_server::browser::{agent as browser_agent, BrowserConfig, BrowserManager};
use ember_server::computers::{self, Computers, Registry};
use ember_server::devices::{self, Devices};
use ember_server::hub::ServerHub;
use ember_transport::Transport;
use ember_server::transport::{self as server_transport, ServerTransport};
use ember_server::mcp::McpRegistry;
use ember_server::schedules::{Scheduler, SystemClock};
use ember_server::session::Sessions;
use ember_server::store::Store;

/// Configuration from the environment.
/// - `EMBER_DATA_DIR`: where `ember.db`, `secret.key` and `accounts/` live (default `~/.ember`)
/// - `EMBER_LISTEN`: listen address (default `127.0.0.1:8740`)
/// - `EMBER_ALLOWED_HOSTS`: extra host names accepted in `Host` (comma-separated, FR-S10); the
///   token is in `<data dir>/server-token` (FR-S9)
/// - `EMBER_IDLE_SECS`: release an agent process after this long without activity (default 300)
/// - `EMBER_CLAUDE_BIN`: the Claude Code CLI (default `claude` on `PATH`)
/// - `EMBER_CODEX_BIN`: the Codex CLI (default `codex` on `PATH`)
/// - `EMBER_AGY_BIN`: the Antigravity CLI (default `agy` on `PATH`). Its sessions keep a private
///   configuration folder each under `<data dir>/agy/`. A version other than the tested one runs
///   read-only (INTENT D14); `EMBER_AGY_SANDBOX=1` adds agy's `--sandbox` (off by default)
/// - `EMBER_SCRIPTED_AGENT=1`: also offer the test agent (development only)
/// - `EMBER_DEMO_DATA=1`: add a demo ChatGPT plan account with 14 days of usage (screenshots only)
/// - `EMBER_ACP_AGENTS`: Agent Client Protocol agents to offer, as JSON: a list of preset names
///   (`["omp"]`) and/or objects `{"name", "command", "args", "model_args", "instructions_args",
///   "version_args", "env"}`; `EMBER_ACP_AGENTS_FILE` reads the same JSON from a file. Presets:
///   `omp` (`omp acp`; `EMBER_OMP_BIN` picks the binary) and `gemini` (`gemini --acp`, API key or
///   Enterprise only; `EMBER_GEMINI_BIN` picks the binary). Both are opt-in. See `agents::acp`.
/// - `EMBER_CHROME_BIN`: the browser for remote browsing (default: found on the machine)
/// - `EMBER_BROWSER_MCP`: `chrome-devtools`, `playwright` or `off` (default): give agents the
///   project's browser as an MCP server, run with `npx`/`bunx` (`EMBER_BROWSER_MCP_RUNNER`)
/// - `EMBER_IDE_COMPUTERS`, `EMBER_IDE_URL`, `EMBER_PUBLIC_URL`: "Open IDE" targets per
///   computer, until ember node reports them (see `api::ide::IdeConfig`)
/// - `EMBER_AGENT_URL`: the server URL given to agents for A2A (default derived from
///   `EMBER_LISTEN`, with an unspecified address replaced by loopback)
/// - `EMBER_A2A=0`: agent-to-agent messaging starts off until a user turns it on
/// - `EMBER_EXEC_BIN`: the `ember-exec` shim for Claude Code on other computers (default: next
///   to this executable)
/// - `EMBER_MOUNT`: `auto` (default), `nfs`, `fuse` or `off`. Mounts a Claude Code session's
///   project directory from its computer at the same path (needs the `mount-nfs` / `mount-fuse`
///   build feature); `EMBER_MOUNT_SHADOW=1` allows mounting over a non-empty local directory,
///   `EMBER_MOUNT_TTL_MS` sets the attribute cache lifetime (default 1000)
/// - `EMBER_HOSTED=1`: this server is a hosted service, not self-hosted; Sign in with ChatGPT
///   (FR-U4) is then off, since OpenAI allows plan usage only for locally hosted apps
/// - `EMBER_CHATGPT_REDIRECT_PORT`: port of the `http://127.0.0.1:<port>/auth/callback` sign-in
///   redirect (default: the `EMBER_LISTEN` port)
/// - `EMBER_TRANSPORT=1`: bind the peer-to-peer transport (key in `<data dir>/transport.key`;
///   peer id and address printed at start). Computers can then be registered by peer, and the
///   API is also served on transport service `ember-server` to allowed devices (FR-N3,
///   `/api/devices`, managed on the TCP listener only)
/// - `EMBER_RELAY_URL`: relay server(s) for the transport (overrides the hub's relay)
/// - `EMBER_SERVER_RELAY_PORT`: letify relay TCP and QAD UDP port (default 7443)
/// - `EMBER_SERVER_RELAY=off`: disable the automatic letify relay and QAD roles
/// - `EMBER_LETIFY_RELAY_LISTEN`, `EMBER_LETIFY_QAD_LISTEN`: override role addresses
/// - `EMBER_HUB_URL`: the darkpyonix.dev hub (default `https://api.darkpyonix.dev`; `off` disables).
///   With the transport on, the server can register to the user's GitHub account there
///   (`POST /api/hub/link`, local only), then publishes its address and resolves computers
///   through the hub's directory and uses its relay (FR-N2). `EMBER_HUB_RELAY_URL` overrides
///   the relay derived from the hub URL; `EMBER_HUB_SYNC_DEVICES=1` keeps the devices
///   allow-list in sync with the account's devices
/// - `EMBER_GUEST_LISTEN`: the guest door (FR-T14), off when unset: a second listener that serves
///   only the guest routes and `/join`, never the owner API. `EMBER_GUEST_LISTEN_ALLOW_REMOTE=1`
///   is required for a non-loopback address; `EMBER_GUEST_ALLOWED_HOSTS` and
///   `EMBER_GUEST_PUBLIC_URL` name the hosts guests use. See `guest_door`.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let started = std::time::SystemTime::now();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();
    // Private MCP config files left by a crashed run hold env values such as runtime tokens.
    ember_server::agents::claude_code::cleanup_stale_mcp_files(&std::env::temp_dir(), started);

    let data_dir = std::env::var_os("EMBER_DATA_DIR").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".ember")
    });
    std::fs::create_dir_all(&data_dir)?;
    let store = Arc::new(Store::open(&data_dir.join("ember.db"))?);
    // Agents run under per-session shims and outlive this server (FR-S8, EMBER_SESSION_SHIM=off
    // keeps direct children). Stopping the server leaves the shims, and so the agents, running.
    let shim_runtime = match ember_server::agents::proc::find_shim_bin() {
        Some(bin) if ember_server::agents::proc::shim_enabled() => {
            let dir = data_dir.join("shims");
            ember_server::session_shim::private_dir(&dir)?;
            let rt = ember_server::agents::proc::ShimRuntime { bin, dir, store: store.clone() };
            ember_server::agents::proc::install_runtime(rt.clone());
            Some(rt)
        }
        _ => {
            tracing::warn!("session shims off; agents stop with the server");
            None
        }
    };
    // Demo only (#193): off unless EMBER_DEMO_DATA=1.
    ember_server::chatgpt::demo::seed(&store, std::env::var(ember_server::chatgpt::demo::ENV).ok().as_deref())?;
    let reset = store.reset_live_statuses()?;
    if reset > 0 {
        tracing::info!("{reset} session(s) were mid-turn at shutdown; marked idle for resume");
    }
    // Repair the per-channel JSONL files against the store before any router serves (FR-T16).
    // It only logs: a problem here never stops the start. It uses its own JsonlLog and shares no lock
    // with the routers' instances, so it must stay synchronous and ahead of every router.
    ember_server::channels::jsonl::JsonlLog::new(&data_dir).recover(&store);

    // Always offered; detection reports a missing binary as not installed (FR-A6).
    // Antigravity's tool approvals arrive over HTTP from its PreToolUse hook (FR-A5).
    let agy_hooks = HookBroker::new();
    let mut adapters: Vec<Arc<dyn AgentAdapter>> = vec![
        Arc::new(ClaudeCodeAdapter::from_env()),
        Arc::new(CodexAdapter::from_env()),
        Arc::new(AntigravityAdapter::from_env(&data_dir, agy_hooks.clone())),
    ];
    if std::env::var("EMBER_SCRIPTED_AGENT").as_deref() == Ok("1") {
        adapters.push(Arc::new(ScriptedAdapter));
    }
    // Configured ACP agents (FR-A2); registering them makes their names valid agent kinds.
    for config in acp::configs_from_env()? {
        tracing::info!(agent = %config.name, command = %config.command.display(), "ACP agent configured");
        adapters.push(Arc::new(AcpAdapter::new(config)?));
    }
    let accounts = Accounts::open(store.clone(), &data_dir)?;
    // Devices allowed over the transport (FR-N3); the table feeds the transport's gate.
    let devices = Devices::open(store.clone())?;
    // The darkpyonix.dev hub (FR-N2): only with the transport, and unless turned off.
    let hub_config = if server_transport::enabled_from_env() { ember_hub::HubConfig::from_env() } else { None };
    let hub = match &hub_config {
        Some(cfg) => Some(ServerHub::new(
            cfg.clone(),
            store.clone(),
            accounts.clone(),
            server_transport::load_key(&data_dir)?,
            devices.clone(),
        )),
        None => None,
    };
    // The peer-to-peer transport (FR-N1), opt-in. `server_peer_id` stays None when it is off.
    let mut server_peer_id: Option<String> = None;
    let net = if server_transport::enabled_from_env() {
        let key = server_transport::load_key(&data_dir)?;
        let registered = hub.as_ref().is_some_and(|h| h.is_registered());
        // The resolve token (`dpr_`) goes in the directory resolver's URL; a registration from
        // before the hub issued them gets one now.
        let token = match &hub {
            Some(h) if registered => h.ensure_resolve_token().await,
            _ => None,
        };
        // The hub's relay and directory from its `/config` (derived when it has none). Asked
        // only when the hub will be used now, so an unregistered server does not contact it.
        let hub_config = match &hub {
            Some(h) if registered || ember_hub::HubConfig::explicitly_enabled() => Some(h.discover().await),
            Some(h) => Some(h.config()),
            None => hub_config,
        };
        let t = Transport::bind(server_transport::config_with_hub(key, hub_config.as_ref(), registered, token)).await?;
        if let Some(h) = &hub {
            h.attach_transport(t.clone());
            let _watch = h.spawn_watch(ember_server::hub::WATCH_PERIOD);
            // A link that was waiting for approval when the server stopped.
            match h.resume_link().await {
                Ok(Some(p)) => tracing::info!(user_code = %p.user_code, "resumed the pending hub link"),
                Ok(None) => {}
                Err(e) => tracing::warn!("could not resume the pending hub link: {e}"),
            }
            let st = h.status()?;
            match (&st.device, st.revoked) {
                (Some(d), false) => tracing::info!(hub = %st.hub_url, name = %d.name, "registered with the hub"),
                (Some(_), true) => tracing::warn!(hub = %st.hub_url, "the hub removed this server; register again with a new key"),
                (None, _) => tracing::info!(hub = %st.hub_url, "not registered with the hub (POST /api/hub/link)"),
            }
        }
        t.wait_online(std::time::Duration::from_secs(5)).await;
        // Printed (not only logged): the peer id goes into each node's allowed peers.
        println!("ember server peer id: {}", t.peer_id());
        server_peer_id = Some(t.peer_id().to_string());
        println!("ember server peer addr: {}", serde_json::to_string(&t.local_addr())?);
        Some(ServerTransport::new(t))
    } else {
        None
    };
    let dialer = net.as_ref().map(|n| n.dialer.clone());
    // Computers and switching (FR-X1–FR-X3, FR-S7 v0); tables live in the main store.
    let computer_registry = Registry::new(store.clone());
    let browser_egress_store: Arc<dyn ember_server::browser::EgressStore> = store.clone();
    let store_for_mcp = store.clone();
    let store_for_channels = store.clone();
    let store_for_guest = store.clone();
    // One `Enrol` for the whole process: the owner's routes (mint, confirm, deny) and the enrol door
    // (redeem, poll, from step C3a) share its failure windows and its pending-token map, which live per instance.
    let enrol = computers::enrol::Enrol::with_server_peer_id(store.clone(), server_peer_id);
    let sessions = Sessions::new(store, adapters);
    // Per-harness memory limits stored by the settings API (FR-L20, FR-S8).
    sessions.load_settings();
    accounts.install(&sessions);
    ember_server::members::install(&sessions);
    let shim = computers::find_shim();
    if shim.is_none() {
        tracing::warn!("ember-exec not found; Claude Code sessions cannot run on other computers");
    }
    let computers = Computers::with_transport(
        computer_registry,
        computers::connector(dialer.clone()),
        shim,
        dialer.clone(),
    );
    // Claude Code's file tools on other computers (EMBER_MOUNT=off|nfs|fuse|auto).
    // Opened like every other node client, so computers registered by peer mount too.
    if let Some(mounts) = computers::mount::ProjectMounts::from_env(computers.node_fs()) {
        if let Err(e) = computers.enable_mounts(mounts) {
            tracing::warn!("project mount disabled: {e:#}");
        }
    } else {
        tracing::info!("project mount off; Claude Code file tools stay on this server");
    }
    computers.install(&sessions);
    // The agent CLI's own API traffic leaves through the session's current computer (FR-X6).
    // EMBER_AGENT_EGRESS=off keeps it on this server.
    if std::env::var("EMBER_AGENT_EGRESS").map(|v| v != "off").unwrap_or(true) {
        let (c, d) = (computers.clone(), dialer.clone());
        let route: ember_server::egress_proxy::Route = Arc::new(move |sid| {
            match c.registry().session_computer(sid)? {
                Some(sc) if sc.computer_id != computers::LOCAL => {
                    let comp = c
                        .registry()
                        .get(&sc.computer_id)?
                        .ok_or_else(|| anyhow::anyhow!("computer {} is gone", sc.computer_id))?;
                    Ok(Some(computers::node_client(&comp, d.as_ref())?))
                }
                _ => Ok(None),
            }
        });
        let egress = ember_server::egress_proxy::SessionEgress::new(route);
        sessions.add_start_hook(egress.clone().start_hook());
        sessions.add_end_hook(egress.end_hook());
    }

    // FR-S10 listen guard: a non-loopback EMBER_LISTEN needs EMBER_ALLOW_NON_LOOPBACK_LISTEN=1.
    let addr: SocketAddr = ember_server::auth::parse_listen(std::env::var("EMBER_LISTEN").ok().as_deref())
        .map_err(|e| anyhow::anyhow!(e))?;
    let allow = std::env::var(ember_server::auth::ALLOW_NON_LOOPBACK_LISTEN).ok();
    if ember_server::auth::check_listen(addr, allow.as_deref()).map_err(|e| anyhow::anyhow!(e))?
        == ember_server::auth::ListenCheck::NonLoopbackAllowed
    {
        tracing::warn!(
            "EMBER_LISTEN={addr} is reachable from that network over plain HTTP; the server token travels in clear. Prefer the transport (EMBER_TRANSPORT=1) for remote devices."
        );
    }
    // The guest door (FR-T14): off unless EMBER_GUEST_LISTEN is set. A bad value is a startup error.
    let guest_door = ember_server::guest_door::GuestDoor::from_env(addr)?;
    // The enrol door (FR-C2e): off unless EMBER_ENROL_LISTEN is set. A bad value, or an address
    // that clashes with the owner's or the guest door's, is a startup error.
    let enrol_door = ember_server::enrol_door::EnrolDoor::from_env(addr, guest_door.as_ref().map(|d| d.listen))?;
    // The URL the enrol command prints is the DOOR's (EMBER_ENROL_PUBLIC_URL, else its specific
    // listen address), never the owner API address: that port serves no /enrol/redeem (FR-C1).
    let enrol_door_url = ember_server::enrol_door::EnrolDoor::door_url(enrol_door.as_ref());
    // This server as seen from this machine (an unspecified address replaced by loopback).
    let local_url = {
        let mut local = addr;
        if local.ip().is_unspecified() {
            local.set_ip(if addr.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        format!("http://{local}")
    };
    // Antigravity's hook runs on this machine and calls the local listener.
    agy_hooks.set_base_url(&local_url);
    let agent_url = std::env::var("EMBER_AGENT_URL").unwrap_or_else(|_| local_url.clone());
    let mut a2a_config = A2aConfig::new(agent_url);
    a2a_config.cli_path = A2aConfig::cli_next_to_current_exe();
    a2a_config.enabled_by_default =
        !matches!(std::env::var("EMBER_A2A").as_deref(), Ok("0" | "off"));
    if a2a_config.cli_path.is_none() {
        tracing::warn!("ember-a2a not found next to the server binary; agents cannot use A2A");
    }
    let a2a = A2a::new(sessions.clone(), A2aStore::open(&data_dir.join("ember.db"))?, a2a_config);
    // `team spawn --computer` (FR-T7) moves the new teammate like a user's switch (FR-X3).
    {
        let (computers, sessions) = (computers.clone(), sessions.clone());
        a2a.set_computer_setter(Arc::new(move |session_id: String, computer_id: String| {
            let (computers, sessions) = (computers.clone(), sessions.clone());
            Box::pin(async move {
                computers
                    .switch(&sessions, &session_id, &computer_id)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            }) as futures::future::BoxFuture<'static, Result<(), String>>
        }));
    }

    let idle = std::time::Duration::from_secs(
        std::env::var("EMBER_IDLE_SECS").ok().and_then(|v| v.parse().ok()).unwrap_or(300),
    );
    {
        let sessions = sessions.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tick.tick().await;
                for id in sessions.reap_idle(idle).await {
                    tracing::info!(session = %id, "released idle agent");
                }
            }
        });
    }

    // Remote browser (SPEC §R). Agents run on this machine, so they reach the CDP relay on loopback.
    // Each project's egress choice is persisted; a computer egress goes through that computer's
    // node via a loopback SOCKS5 listener (FR-R1).
    let browsers = BrowserManager::with_egress(
        BrowserConfig::from_env(&data_dir),
        Some(browser_egress_store),
        Some(computers.clone() as Arc<dyn ember_server::browser::EgressResolver>),
    );
    match &browsers.config().chrome {
        Some(p) => tracing::info!("remote browser: {}", p.display()),
        None => tracing::warn!("remote browser unavailable: no Chrome/Chromium found (EMBER_CHROME_BIN)"),
    }
    match browser_agent::from_env() {
        Some((mcp, runner)) => {
            tracing::info!("agents get the project browser via {mcp:?} MCP ({runner:?})");
            sessions.add_start_config_hook(browser_agent::start_config_hook(
                format!("http://127.0.0.1:{}", addr.port()),
                mcp,
                runner,
            ));
        }
        None => tracing::info!("agents get no browser MCP server (EMBER_BROWSER_MCP is off)"),
    }
    // The central MCP registry (FR-A7): after the browser hook, so its server keeps its name.
    let mcp = McpRegistry::open(store_for_mcp.clone(), &data_dir)?;
    mcp.install(&sessions);

    // Sign in with ChatGPT (FR-U4): self-hosted only; the loopback callback is served below.
    let chatgpt = ember_server::chatgpt::ChatGpt::new(
        accounts.clone(),
        ember_server::chatgpt::ChatGptConfig::from_env(addr.port()),
    )?;
    if !chatgpt.config().enabled {
        tracing::info!("Sign in with ChatGPT is off: EMBER_HOSTED is set");
    } else if !addr.ip().is_loopback() && !addr.ip().is_unspecified() {
        tracing::warn!(
            "ChatGPT sign-in redirects to {}, but the server listens on {addr}; paste the final \
             URL into POST /api/chatgpt/signin/complete",
            chatgpt.config().redirect_uri()
        );
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("ember server listening on http://{addr}");
    // Bound before the owner server starts serving, so a bind failure aborts the start.
    let guest_listener = match &guest_door {
        Some(d) => {
            let l = d.bind().await.map_err(|e| anyhow::anyhow!("guest door: cannot bind {}: {e}", d.listen))?;
            if d.is_remote() {
                tracing::warn!(
                    "guest door on {} is reachable from that network (EMBER_GUEST_LISTEN_ALLOW_REMOTE=1); it \
                     speaks plain HTTP and anyone with an invite link can join a channel; put TLS in \
                     front of it",
                    d.listen
                );
            } else {
                tracing::info!("guest door on {} (loopback)", d.listen);
            }
            Some(l)
        }
        None => None,
    };
    // Bound before the owner server starts serving, so a bind failure aborts the start.
    let enrol_listener = match &enrol_door {
        Some(d) => {
            let l = d.bind().await.map_err(|e| anyhow::anyhow!("enrol door: cannot bind {}: {e}", d.listen))?;
            if d.is_remote() {
                tracing::warn!(
                    "enrol door on {} is reachable from that network (EMBER_ENROL_LISTEN_ALLOW_REMOTE=1); it \
                     speaks plain HTTP, so an enrol code and the credential cross the network in clear; put \
                     TLS in front of it",
                    d.listen
                );
            } else {
                tracing::info!("enrol door on {} (loopback)", d.listen);
            }
            Some(l)
        }
        None => None,
    };
    // After binding, so agents woken by queued messages can reach the API.
    a2a.install();
    // Scheduled tasks (FR-A8): the first tick reports triggers missed while the server was down.
    let scheduler = Scheduler::new(sessions.clone(), Arc::new(SystemClock));
    {
        let (computers, sessions) = (computers.clone(), sessions.clone());
        let place = move |session_id: String,
                          computer_id: String|
              -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
            let (computers, sessions) = (computers.clone(), sessions.clone());
            Box::pin(async move {
                computers
                    .switch(&sessions, &session_id, &computer_id)
                    .await
                    .map(|_| ())
                    .map_err(|e| anyhow::anyhow!("{e}"))
            })
        };
        scheduler.set_computer_placer(Arc::new(place));
    }
    let _scheduler_task = scheduler.start();
    // "Open IDE" takes its computers from the registry and the session's current computer
    // (FR-L7, FR-W4); EMBER_IDE_COMPUTERS stays as an override merged onto them.
    let ide = Arc::new(ember_server::api::ide::IdeConfig::from_env()?);
    let ose = Arc::new(ember_server::ose::OseManager::default());
    // A SIGKILL of the previous server skips the token file drop (#521).
    ose.sweep_stale_tokens();
    let transfers = ember_server::transfers::TransferManager::new(
        &data_dir,
        Some(devices.clone()),
        Some(computers.clone()),
    );
    // The guest router is built only when the door is on, wrapped only by the door's guards, and
    // never merged into `app` (not under auth::protect either).
    let guest_app = guest_door.as_ref().map(|d| {
        d.wrap_limited(
            ember_server::channels::guest::guest_router_with_a2a(
                store_for_guest.clone(),
                sessions.clone(),
                &data_dir,
                a2a.clone(),
            ),
            d.limits(),
        )
    });
    // The enrol door's router is built only when the door is on, wrapped only by the door's
    // guards, and never merged into `app` or `local_app` (not under auth::protect either).
    let enrol_app = enrol_door.as_ref().map(|d| d.app(enrol.clone()));
    let app = ember_server::api::router(sessions.clone())
        .merge(computers::api::router(computers.clone(), sessions.clone()))
        .merge(computers::terms::router(computers.clone()))
        .merge(computers::metrics::router(computers.clone()))
        .merge(computers::fs::router(computers.clone()))
        .merge(computers::ports::router(computers.clone()))
        .merge(ember_server::api::harnesses::router(sessions.clone(), computers.clone()))
        .merge(ember_server::api::git::router(sessions.clone(), computers.clone()))
        .merge(ember_server::api::integrations::router(sessions.clone(), computers.clone()))
        .merge(ember_server::api::ide::router_with_computers_and_ose(sessions.clone(), ide, computers.clone(), ose.clone()))
        .merge(ember_server::api::ide_extensions::router(computers.clone(), ose))
        .merge(ember_server::schedules::api::agent_router(scheduler.clone(), a2a.clone()))
        .merge(ember_server::schedules::api::router(scheduler))
        .merge(ember_server::mcp::api::router(mcp))
        .merge(ember_server::channels::api::router_with_a2a_door(store_for_channels, sessions.clone(), &data_dir, a2a.clone(), guest_door.clone()))
        .merge(ember_server::a2a::api::router(a2a))
        .merge(ember_server::accounts::api::router(accounts))
        .merge(ember_server::chatgpt::api::router(chatgpt))
        .merge(ember_server::browser::api::router(browsers.clone()))
        .merge(ember_server::hub::api::router(hub.clone(), computers.clone()))
        .merge(ember_server::transfers::router(transfers));
    // Over the transport: the same API to allowed devices, without device management.
    let transport_task = match &net {
        Some(n) => {
            let serve = server_transport::serve(&n.transport, app.clone(), devices.gate().clone())?;
            tracing::info!(
                peer = %n.transport.peer_id(),
                service = server_transport::SERVER_SERVICE,
                devices = devices.list()?.len(),
                "ember server serving over the transport"
            );
            Some(tokio::spawn(async move {
                if let Err(e) = serve.await {
                    tracing::error!("transport listener failed: {e}");
                }
            }))
        }
        None => None,
    };
    // Device management, the hub and Antigravity's approval hook are local only (TCP).
    let local_app = app
        .merge(devices::api::router(devices.clone()))
        .merge(antigravity::router(agy_hooks))
        .merge(ember_server::hub::api::admin_router(hub.clone(), computers.clone()))
        // FR-C1: minting an enrol code is an admin act, local and token-protected only. There is
        // no redeem route (docs/design/COMPUTERS.md, "Adding a computer with an enrol code").
        // FR-C2c: the owner's confirm and deny are on this router too, and so is the registrar
        // that adds the confirmed computer to the registry.
        .merge(computers::enrol::router(
            enrol.clone(),
            enrol_door_url,
            Arc::new(
                computers::enrol::ComputersRegistrar::new(computers.clone())
                    .with_server_peer_id(enrol.server_peer_id().map(String::from)),
            ),
        ));
    // FR-S9, FR-S10: the token and the Origin/Host check on the local listener, every route.
    // The transport above serves `app` unwrapped: its callers passed the device gate (FR-N7).
    let local_app = {
        let token = ember_server::auth::load_or_create_token(&data_dir)?;
        tracing::info!(
            "server token file: {} (send it as Authorization: Bearer)",
            ember_server::auth::token_path(&data_dir).display()
        );
        let mut hosts: Vec<String> = std::env::var("EMBER_ALLOWED_HOSTS")
            .unwrap_or_default()
            .split(',')
            .map(str::to_string)
            .collect();
        if let Ok(u) = std::env::var("EMBER_AGENT_URL") {
            if let Some((_, rest)) = u.split_once("://") {
                hosts.push(ember_server::api::origin::host_of(rest.split('/').next().unwrap_or("")));
            }
        }
        ember_server::auth::protect(local_app, ember_server::auth::ServerAuth::new(token, addr, hosts))
    };
    if let Some(rt) = &shim_runtime {
        let rejoined = sessions.rejoin_shims(rt).await;
        if !rejoined.is_empty() {
            tracing::info!("rejoined {} running session(s) from the previous server", rejoined.len());
        }
    }
    // The guest door stops with the owner server. Its failure never stops the owner server.
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let guest_task = match (guest_listener, guest_app) {
        (Some(listener), Some(app)) => {
            let mut stop = stop_rx.clone();
            Some(tokio::spawn(async move {
                let shutdown = async move {
                    let _ = stop.wait_for(|v| *v).await;
                };
                if let Err(e) = ember_server::guest_door::GuestDoor::serve(listener, app, shutdown).await {
                    tracing::error!("guest door failed and is closed: {e}");
                }
            }))
        }
        _ => None,
    };
    // The enrol door is its own task on the same stop watch. Its failure never stops the owner server.
    let enrol_task = match (enrol_listener, enrol_app) {
        (Some(listener), Some(app)) => {
            let mut stop = stop_rx.clone();
            Some(tokio::spawn(async move {
                let shutdown = async move {
                    let _ = stop.wait_for(|v| *v).await;
                };
                if let Err(e) = ember_server::enrol_door::EnrolDoor::serve(listener, app, shutdown).await {
                    tracing::error!("enrol door failed and is closed: {e}");
                }
            }))
        }
        _ => None,
    };
    let served = axum::serve(listener, local_app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    let _ = stop_tx.send(true);
    if let Some(mut task) = guest_task {
        if tokio::time::timeout(std::time::Duration::from_secs(5), &mut task).await.is_err() {
            task.abort();
        }
    }
    if let Some(mut task) = enrol_task {
        if tokio::time::timeout(std::time::Duration::from_secs(5), &mut task).await.is_err() {
            task.abort();
        }
    }
    served?;
    if let Some(task) = transport_task {
        task.abort();
    }
    if let Some(n) = &net {
        n.transport.close().await;
    }
    // Close browsers gracefully so their profiles (cookies, storage) are flushed (FR-R2).
    browsers.shutdown().await;
    // Unmount project directories served from other computers.
    computers.shutdown().await;
    Ok(())
}
