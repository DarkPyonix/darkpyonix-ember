//! Debugging in the session core: [`crate::debugger::DebugState`] decides, this module executes
//! what it returns and routes the requests of the extension host's `MainThreadDebugService`.

use ember_editor_conn::actors::generated::main_thread_debug_service as actor;
use ember_editor_conn::actors::{Ctx, Outcome};
use ember_editor_conn::debug::DapMessage;
use ember_editor_conn::rpc::Reply;
use serde_json::Value;

use super::{is_missing_file, Effect, Pending, ReadPurpose, SessionCore, SessionUpdate, Token};
use crate::debugger::{DebugRequest, DebugState, Env, Out, Provider};

fn empty() -> Outcome {
    Outcome::Reply(Reply::Empty)
}

impl SessionCore {
    /// What a debug request may need from the rest of the session.
    fn debug_env(&self) -> Env {
        let active_file = self.active.as_ref().and_then(|key| self.docs.get(key)).map(|doc| doc.uri.path.clone());
        Env { active_file }
    }

    /// Run `f` on the debug state at the current time and execute what it returns.
    fn with_debug(&mut self, f: impl FnOnce(&mut DebugState) -> Vec<Out>) {
        self.debug.set_now(self.now);
        let outs = f(&mut self.debug);
        self.apply_debug(outs);
    }

    fn apply_debug(&mut self, outs: Vec<Out>) {
        for o in outs {
            match o {
                Out::Rpc(call) => self.rpc(call),
                Out::RpcWith(call, pending) => {
                    let _token: Token = self.rpc_with(call, Pending::Debug(pending));
                }
                Out::Respond { req, result } => self.respond(req, result),
                Out::Ui(update) => self.ui(SessionUpdate::Debug(update)),
                Out::ReadLaunchJson { uri, start } => self.out.push(Effect::ReadFile { uri, purpose: ReadPurpose::LaunchJson { start } }),
            }
        }
    }

    pub(super) fn on_debug_request(&mut self, request: DebugRequest) {
        let env = self.debug_env();
        self.with_debug(|d| d.request(request, &env));
    }

    pub(super) fn on_debug_reply(&mut self, pending: crate::debugger::Pending, result: Result<Value, String>) {
        self.with_debug(|d| d.on_reply(pending, result));
    }

    pub(super) fn on_launch_json_read(&mut self, start: u64, result: Result<Vec<u8>, String>) {
        let result = match result {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if is_missing_file(&e) => Ok(None),
            Err(e) => Err(e),
        };
        self.with_debug(|d| d.on_launch_json(start, result));
    }

    pub(super) fn debug_tick(&mut self) {
        let now = self.now;
        let outs = self.debug.tick(now);
        self.apply_debug(outs);
    }
}

impl actor::DebugService for SessionCore {
    fn register_debug_types(&mut self, _cx: &Ctx, args: actor::RegisterDebugTypesArgs) -> Outcome {
        self.debug.register_types(args.debug_types);
        empty()
    }

    fn session_cached(&mut self, _cx: &Ctx, args: actor::SessionCachedArgs) -> Outcome {
        self.debug.session_cached(args.session_id);
        empty()
    }

    fn accept_da_message(&mut self, _cx: &Ctx, args: actor::AcceptDAMessageArgs) -> Outcome {
        match DapMessage::from_value(&args.message) {
            Ok(message) => self.with_debug(|d| d.on_dap(args.handle, message)),
            Err(e) => tracing::warn!("ember-editor: cannot decode a message of debug adapter {}: {e}", args.handle),
        }
        empty()
    }

    fn accept_da_error(&mut self, _cx: &Ctx, args: actor::AcceptDAErrorArgs) -> Outcome {
        self.with_debug(|d| d.on_da_error(args.handle, &args.name, &args.message));
        empty()
    }

    fn accept_da_exit(&mut self, _cx: &Ctx, args: actor::AcceptDAExitArgs) -> Outcome {
        self.with_debug(|d| d.on_da_exit(args.handle, args.code));
        empty()
    }

    fn register_debug_configuration_provider(&mut self, _cx: &Ctx, args: actor::RegisterDebugConfigurationProviderArgs) -> Outcome {
        self.debug.register_provider(Provider {
            handle: args.handle,
            debug_type: args.r#type,
            trigger_kind: args.trigger_kind.as_i64().unwrap_or(1),
            has_provide: args.has_provide_method,
            has_resolve: args.has_resolve_method,
            has_resolve2: args.has_resolve2_method,
        });
        empty()
    }

    fn register_debug_adapter_descriptor_factory(&mut self, _cx: &Ctx, args: actor::RegisterDebugAdapterDescriptorFactoryArgs) -> Outcome {
        self.debug.register_factory(args.r#type, args.handle);
        empty()
    }

    fn unregister_debug_configuration_provider(&mut self, _cx: &Ctx, args: actor::UnregisterDebugConfigurationProviderArgs) -> Outcome {
        self.debug.unregister_provider(args.handle);
        empty()
    }

    fn unregister_debug_adapter_descriptor_factory(&mut self, _cx: &Ctx, args: actor::UnregisterDebugAdapterDescriptorFactoryArgs) -> Outcome {
        self.debug.unregister_factory(args.handle);
        empty()
    }

    /// Answered when the adapter answered `launch`, or when the start was cancelled or failed.
    fn start_debugging(&mut self, cx: &Ctx, args: actor::StartDebuggingArgs) -> Outcome {
        let env = self.debug_env();
        let req = cx.req;
        self.with_debug(|d| d.ext_start_debugging(req, args.folder, args.name_or_config, &args.options, &env));
        Outcome::Deferred
    }

    fn stop_debugging(&mut self, cx: &Ctx, args: actor::StopDebuggingArgs) -> Outcome {
        let req = cx.req;
        self.with_debug(|d| d.ext_stop_debugging(req, args.session_id));
        Outcome::Deferred
    }

    fn set_debug_session_name(&mut self, _cx: &Ctx, args: actor::SetDebugSessionNameArgs) -> Outcome {
        self.with_debug(|d| d.ext_set_session_name(&args.id, args.name));
        empty()
    }

    fn custom_debug_adapter_request(&mut self, cx: &Ctx, args: actor::CustomDebugAdapterRequestArgs) -> Outcome {
        let req = cx.req;
        self.with_debug(|d| d.ext_custom_request(req, &args.id, &args.command, args.args));
        Outcome::Deferred
    }

    fn get_debug_protocol_breakpoint(&mut self, cx: &Ctx, args: actor::GetDebugProtocolBreakpointArgs) -> Outcome {
        let req = cx.req;
        self.with_debug(|d| d.ext_protocol_breakpoint(req, &args.id, &args.breakpoin_id));
        Outcome::Deferred
    }

    fn append_debug_console(&mut self, _cx: &Ctx, args: actor::AppendDebugConsoleArgs) -> Outcome {
        self.with_debug(|d| d.ext_append_console(args.value));
        empty()
    }

    fn register_breakpoints(&mut self, _cx: &Ctx, args: actor::RegisterBreakpointsArgs) -> Outcome {
        self.with_debug(|d| d.register_breakpoints(&args.breakpoints));
        empty()
    }

    fn unregister_breakpoints(&mut self, _cx: &Ctx, args: actor::UnregisterBreakpointsArgs) -> Outcome {
        let mut ids = args.breakpoint_ids;
        ids.extend(args.function_breakpoint_ids);
        self.with_debug(|d| d.unregister_breakpoints(&ids));
        empty()
    }
}
