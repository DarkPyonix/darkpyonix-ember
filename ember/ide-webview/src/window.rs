//! Native `tao` window and `wry` webview event loop.

use std::collections::HashMap;
use std::time::Duration;

use ember_bridge::{BridgeError, BridgeHost, HostOutcome, WebviewBridge};
use tao::dpi::{LogicalPosition, LogicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder, WindowId};
use wry::{WebView, WebViewBuilder};

use crate::cli::Config;
use crate::opener::{
    bridge_init_script, receive_eval_script, OpenWindowCommand, QueuedWindowOpener, WindowRegistry,
};

#[derive(Debug)]
pub enum UserEvent {
    Ipc {
        source_window_id: String,
        payload: String,
    },
    SmokeExit,
}

struct ManagedWindow {
    logical_id: String,
    _window: Window,
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

fn spawn_window(
    target: &EventLoopWindowTarget<UserEvent>,
    proxy: EventLoopProxy<UserEvent>,
    cmd: &OpenWindowCommand,
) -> Result<(WindowId, ManagedWindow), String> {
    let mut wb = WindowBuilder::new()
        .with_title(&cmd.title)
        .with_inner_size(LogicalSize::new(1280.0, 840.0));
    if let Some(pos) = cmd.screen {
        wb = wb.with_position(LogicalPosition::new(pos.x.max(0.0), pos.y.max(0.0)));
    }
    let window = wb
        .build(target)
        .map_err(|e| format!("failed to create native window: {e}"))?;
    let tao_id = window.id();

    let source_id = cmd.window_id.clone();
    let init_js = bridge_init_script(&cmd.window_id);
    let builder = WebViewBuilder::new()
        .with_url(&cmd.url)
        .with_initialization_script(&init_js)
        .with_ipc_handler(move |req| {
            let payload = req.into_body();
            let _ = proxy.send_event(UserEvent::Ipc {
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
            _window: window,
            webview,
        },
    ))
}

pub fn run(config: Config) -> Result<(), String> {
    if config.validate_only {
        println!(
            "ember-ide-webview ok: window_id={} origin={} url={}",
            config.window_id, config.origin, config.url
        );
        return Ok(());
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let initial = OpenWindowCommand {
        window_id: config.window_id.clone(),
        url: config.url.clone(),
        title: config.title.clone(),
        screen: None,
    };

    let mut registry: WindowRegistry<ManagedWindow> = WindowRegistry::new();
    let mut tao_to_logical: HashMap<WindowId, String> = HashMap::new();
    let (first_tao_id, first_win) = spawn_window(&event_loop, proxy.clone(), &initial)?;
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
                    match spawn_window(target, proxy.clone(), &cmd) {
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
