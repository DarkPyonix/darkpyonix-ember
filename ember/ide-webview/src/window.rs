//! Native `tao` window and `wry` webview event loop.

use std::collections::HashMap;
use std::process::Command;
use std::time::Duration;

use ember_bridge::{BridgeError, BridgeHost, HostOutcome, WebviewBridge};
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Theme, Window, WindowBuilder, WindowId};
use wry::{WebContext, WebView, WebViewBuilder};

use crate::cli::{
    classify_navigation, format_document_title, ChromeMode, Config, NavigationDecision,
    DARKPYONIX_BG_RGBA, DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, MIN_WINDOW_HEIGHT,
    MIN_WINDOW_WIDTH,
};
use crate::opener::{
    bridge_init_script, receive_eval_script, OpenWindowCommand, QueuedWindowOpener, WindowRegistry,
};

#[derive(Debug)]
pub enum UserEvent {
    Ipc {
        source_window_id: String,
        payload: String,
    },
    TitleChanged {
        logical_id: String,
        title: String,
    },
    SmokeExit,
}

struct ManagedWindow {
    logical_id: String,
    fallback_title: String,
    window: Window,
    webview: WebView,
}

impl WebviewBridge for ManagedWindow {
    fn send_to_native(&self, _payload: &str) -> Result<(), BridgeError> {
        Err(BridgeError::Platform(
            "send_to_native is only driven by the webview IPC callback".to_owned(),
        ))
    }

    fn send_to_webview(&self, payload: &str) -> Result<(), BridgeError> {
        let script = receive_eval_script(payload);
        self.webview
            .evaluate_script(&script)
            .map_err(|e| BridgeError::Platform(e.to_string()))
    }
}

/// Opens an external `http://` or `https://` link in the OS default browser.
fn open_external_link(url: &str) {
    let argv: Vec<&str> = if cfg!(target_os = "macos") {
        vec!["open", url]
    } else if cfg!(target_os = "windows") {
        vec!["rundll32", "url.dll,FileProtocolHandler", url]
    } else {
        vec!["xdg-open", url]
    };
    if let Err(err) = Command::new(argv[0]).args(&argv[1..]).spawn() {
        tracing::warn!(url = %url, error = %err, "failed to open external link");
    }
}

fn spawn_window(
    target: &EventLoopWindowTarget<UserEvent>,
    proxy: EventLoopProxy<UserEvent>,
    web_context: &mut WebContext,
    session_origin: &str,
    chrome_mode: ChromeMode,
    cmd: &OpenWindowCommand,
) -> Result<(WindowId, ManagedWindow), String> {
    let mut wb = WindowBuilder::new()
        .with_title(&cmd.title)
        .with_inner_size(LogicalSize::new(DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT))
        .with_min_inner_size(LogicalSize::new(MIN_WINDOW_WIDTH, MIN_WINDOW_HEIGHT))
        .with_theme(Some(Theme::Dark));

    #[cfg(target_os = "macos")]
    if chrome_mode == ChromeMode::Integrated {
        use tao::platform::macos::WindowBuilderExtMacOS;
        wb = wb
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true)
            .with_title_hidden(true);
    }

    #[cfg(not(target_os = "macos"))]
    let _ = chrome_mode;

    if let Some(pos) = cmd.screen {
        wb = wb.with_position(LogicalPosition::new(pos.x.max(0.0), pos.y.max(0.0)));
    }
    let window = wb
        .build(target)
        .map_err(|e| format!("failed to create native window: {e}"))?;
    let tao_id = window.id();

    let source_id = cmd.window_id.clone();
    let title_logical_id = cmd.window_id.clone();
    let ipc_proxy = proxy.clone();
    let title_proxy = proxy;
    let nav_origin = session_origin.to_owned();
    let popup_origin = session_origin.to_owned();
    let init_js = bridge_init_script(&cmd.window_id, chrome_mode);

    let builder = WebViewBuilder::with_web_context(web_context)
        .with_url(&cmd.url)
        .with_background_color(DARKPYONIX_BG_RGBA)
        .with_clipboard(true)
        .with_focused(true)
        .with_initialization_script(&init_js)
        .with_navigation_handler(move |target_url| match classify_navigation(&nav_origin, &target_url) {
            NavigationDecision::Allow => true,
            NavigationDecision::OpenExternal => {
                tracing::info!(url = %target_url, "opening external link outside IDE window");
                open_external_link(&target_url);
                false
            }
            NavigationDecision::Block => {
                tracing::warn!(url = %target_url, "blocked disallowed navigation in IDE window");
                false
            }
        })
        .with_new_window_req_handler(move |target_url| {
            match classify_navigation(&popup_origin, &target_url) {
                NavigationDecision::Allow => true,
                NavigationDecision::OpenExternal => {
                    tracing::info!(url = %target_url, "opening external popup outside IDE window");
                    open_external_link(&target_url);
                    false
                }
                NavigationDecision::Block => {
                    tracing::warn!(url = %target_url, "blocked disallowed popup in IDE window");
                    false
                }
            }
        })
        .with_document_title_changed_handler(move |title| {
            let _ = title_proxy.send_event(UserEvent::TitleChanged {
                logical_id: title_logical_id.clone(),
                title,
            });
        })
        .with_ipc_handler(move |req| {
            let payload = req.into_body();
            let _ = ipc_proxy.send_event(UserEvent::Ipc {
                source_window_id: source_id.clone(),
                payload,
            });
        });

    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        let vbox = window
            .default_vbox()
            .ok_or_else(|| "GTK window has no default vbox".to_owned())?;
        builder
            .build_gtk(vbox)
            .map_err(|e| format!("failed to create GTK webview: {e}"))?
    };

    #[cfg(not(target_os = "linux"))]
    let webview = builder
        .build(&window)
        .map_err(|e| format!("failed to create webview: {e}"))?;

    Ok((
        tao_id,
        ManagedWindow {
            logical_id: cmd.window_id.clone(),
            fallback_title: cmd.title.clone(),
            window,
            webview,
        },
    ))
}

pub fn run(config: Config) -> Result<(), String> {
    if config.validate_only {
        println!(
            "ember-ide-webview ok: window_id={} origin={} url={} chrome={:?} data_dir={}",
            config.window_id,
            config.origin,
            config.url,
            config.chrome_mode,
            config
                .data_dir
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<ephemeral>".to_owned())
        );
        return Ok(());
    }

    if let Some(dir) = config.data_dir.as_deref() {
        if let Err(err) = std::fs::create_dir_all(dir) {
            tracing::warn!(
                dir = %dir.display(),
                error = %err,
                "could not create webview data directory; continuing with platform default"
            );
        }
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let mut web_context = WebContext::new(config.data_dir.clone());

    let initial = OpenWindowCommand {
        window_id: config.window_id.clone(),
        url: config.url.clone(),
        title: config.title.clone(),
        screen: None,
    };

    let session_origin = config.origin.clone();
    let chrome_mode = config.chrome_mode;

    let mut registry: WindowRegistry<ManagedWindow> = WindowRegistry::new();
    let mut tao_to_logical: HashMap<WindowId, String> = HashMap::new();
    let (first_tao_id, first_win) = spawn_window(
        &event_loop,
        proxy.clone(),
        &mut web_context,
        &session_origin,
        chrome_mode,
        &initial,
    )?;
    tao_to_logical.insert(first_tao_id, first_win.logical_id.clone());
    registry.insert(first_win.logical_id.clone(), first_win);

    if let Some(ms) = config.smoke_exit_ms {
        let smoke_proxy = proxy.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(ms));
            let _ = smoke_proxy.send_event(UserEvent::SmokeExit);
        });
    }

    let prefix = format!("ide-{}", std::process::id());
    let mut host = BridgeHost::new(QueuedWindowOpener::new(config.origin, config.title), prefix);

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::UserEvent(UserEvent::SmokeExit) => {
                tracing::info!("smoke timer elapsed; exiting");
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::TitleChanged { logical_id, title }) => {
                if let Some(managed) = registry.get(&logical_id) {
                    let next = format_document_title(&title, &managed.fallback_title);
                    managed.window.set_title(&next);
                }
            }
            Event::UserEvent(UserEvent::Ipc {
                source_window_id,
                payload,
            }) => {
                match host.handle_from_webview(&payload) {
                    HostOutcome::Opened(req) => {
                        tracing::info!(
                            source = %source_window_id,
                            new_window_id = %req.window_id,
                            "opening detached IDE window"
                        );
                    }
                    HostOutcome::OpenFailed { request, error } => {
                        tracing::warn!(
                            source = %source_window_id,
                            window_id = %request.window_id,
                            error = %error,
                            "failed to queue detached IDE window"
                        );
                    }
                    HostOutcome::Ignored { kind, reason } => {
                        tracing::debug!(
                            source = %source_window_id,
                            kind = %kind,
                            reason = %reason,
                            "ignored bridge message"
                        );
                    }
                    HostOutcome::Rejected(err) => {
                        tracing::warn!(
                            source = %source_window_id,
                            error = %err,
                            "rejected invalid bridge payload"
                        );
                    }
                }

                for cmd in host.opener_mut().take_pending() {
                    match spawn_window(
                        target,
                        proxy.clone(),
                        &mut web_context,
                        &session_origin,
                        chrome_mode,
                        &cmd,
                    ) {
                        Ok((tao_id, managed)) => {
                            tao_to_logical.insert(tao_id, managed.logical_id.clone());
                            registry.insert(managed.logical_id.clone(), managed);
                        }
                        Err(err) => {
                            tracing::error!(
                                window_id = %cmd.window_id,
                                error = %err,
                                "failed to spawn detached IDE window"
                            );
                        }
                    }
                }
            }
            Event::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } => {
                if let Some(logical_id) = tao_to_logical.remove(&window_id) {
                    let report = registry.close(&logical_id);
                    for (sibling_id, err) in &report.errors {
                        tracing::warn!(
                            sibling_id = %sibling_id,
                            closed_id = %logical_id,
                            error = %err,
                            "failed to notify sibling window of close"
                        );
                    }
                    if report.removed {
                        tracing::info!(
                            closed_id = %logical_id,
                            notified = report.notified.len(),
                            remaining = report.remaining,
                            "closed IDE window"
                        );
                    }
                }
                if registry.is_empty() {
                    *control_flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    });
}
