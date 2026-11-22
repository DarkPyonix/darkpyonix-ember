//! The `MainThread*` actors: every RPC target the extension host can call on the renderer side,
//! generated from Code-OSS's `extHost.protocol.ts` at [`crate::PINNED_COMMIT`].
//!
//! [`generated`] holds, per actor, its proxy name and nid, a `Method` enum with the wire names, an
//! argument struct with `decode` and `encode` for each method, a trait whose methods all default to
//! [`Outcome::Unimplemented`], and a `dispatch` function. [`generated::Actors`] routes a request to
//! whichever actors Ember implements. Behaviour is written by implementing the traits; the
//! generated file is never edited by hand (regenerate it with `scripts/gen-actors/gen.sh`).
//!
//! Type mapping: `string` to `String`, `boolean` to `bool`, `number` to `i64` (or `f64` for the
//! few parameters that carry a double, such as timings), `UriComponents` to [`UriComponents`],
//! `VSBuffer` to [`codec::VsBuffer`], `SerializableObjectWithBuffers<T>` to
//! [`codec::ObjectWithBuffers`], arrays of those to `Vec`, and every other type to
//! `serde_json::Value` for the implementation to read. A parameter that is `?`, `| undefined` or
//! `| null` is an `Option`. A trailing `CancellationToken` is not an argument: it makes the
//! request cancellable ([`Ctx::cancellable`]).

pub mod codec;
pub mod generated;

use serde_json::Value;

use crate::rpc::{IncomingRequest, Reply};
use crate::Result;

pub use codec::{ObjectWithBuffers, UriComponents, VsBuffer};

/// The request a trait method is answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ctx {
    pub req: u32,
    /// The caller passed a `CancellationToken`; a `Cancel` for `req` may follow.
    pub cancellable: bool,
}

/// What an actor method did with a request.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Answer now.
    Reply(Reply),
    /// Fail now, with the JSON of a JS error (see [`crate::rpc::js_error`]) or no error body.
    Fail(Option<Value>),
    /// The implementation answers later with [`crate::rpc::RpcPeer::respond`] for [`Ctx::req`].
    Deferred,
    /// Ember does not implement this method yet.
    Unimplemented,
}

/// One parameter of a method as the protocol declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamInfo {
    pub name: &'static str,
    /// The TypeScript type, whitespace collapsed.
    pub ts_type: &'static str,
    pub optional: bool,
    pub rest: bool,
}

/// One `$method` of an actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodInfo {
    pub name: &'static str,
    /// Line in extHost.protocol.ts at the pinned commit.
    pub line: u32,
    /// The TypeScript return type.
    pub returns: &'static str,
    pub cancellable: bool,
    pub params: &'static [ParamInfo],
}

/// One `MainThread*` actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActorInfo {
    pub proxy: &'static str,
    pub nid: u8,
    /// The shape interface in extHost.protocol.ts.
    pub shape: &'static str,
    pub methods: &'static [MethodInfo],
}

impl ActorInfo {
    pub fn by_proxy(proxy: &str) -> Option<&'static ActorInfo> {
        generated::ACTORS.iter().find(|a| a.proxy == proxy)
    }

    pub fn method(&self, name: &str) -> Option<&'static MethodInfo> {
        self.methods.iter().find(|m| m.name == name)
    }
}

/// Routes `r` through `actors`. `Ok(None)`: the proxy or the method is not part of the pinned
/// protocol (a server of another commit, which the handshake refuses, or an `ExtHost*` id).
pub fn dispatch(actors: &mut generated::Actors<'_>, r: &IncomingRequest) -> Result<Option<Outcome>> {
    let Some(proxy) = r.proxy else { return Ok(None) };
    let cx = Ctx { req: r.req, cancellable: r.cancellable };
    actors.dispatch(proxy, &cx, &r.method, &r.args)
}

#[cfg(test)]
mod tests {
    use super::generated::*;
    use super::*;
    use crate::rpc::Arg;
    use crate::rpc_ids::PROXY_IDS;
    use serde_json::json;

    #[test]
    fn fr_e1_generated_actors_are_the_pinned_main_thread_ids() {
        assert_eq!(GENERATED_FROM, crate::PINNED_COMMIT, "generated.rs is from another Code-OSS commit; regenerate it");
        let main_thread: Vec<(u8, &str)> = PROXY_IDS
            .iter()
            .enumerate()
            .filter(|(_, n)| n.starts_with("MainThread"))
            .map(|(i, n)| (i as u8 + 1, *n))
            .collect();
        let generated: Vec<(u8, &str)> = ACTORS.iter().map(|a| (a.nid, a.proxy)).collect();
        assert_eq!(generated, main_thread);
        assert_eq!(ACTORS.len(), ACTOR_COUNT);
        assert_eq!(ACTOR_COUNT, 87);
        assert_eq!(ACTORS.iter().map(|a| a.methods.len()).sum::<usize>(), METHOD_COUNT);
        for a in ACTORS {
            let mut names: Vec<&str> = a.methods.iter().map(|m| m.name).collect();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), a.methods.len(), "{} has a method twice", a.proxy);
            assert!(a.methods.iter().all(|m| m.name.starts_with('$')), "{}", a.proxy);
        }
    }

    #[test]
    fn fr_e1_method_ids_match_the_protocol_table() {
        for (info, names) in [
            (ActorInfo::by_proxy(main_thread_diagnostics::PROXY).unwrap(), main_thread_diagnostics::Method::ALL.iter().map(|m| m.name()).collect::<Vec<_>>()),
            (ActorInfo::by_proxy(main_thread_output_service::PROXY).unwrap(), main_thread_output_service::Method::ALL.iter().map(|m| m.name()).collect()),
        ] {
            assert_eq!(info.methods.iter().map(|m| m.name).collect::<Vec<_>>(), names);
        }
        use main_thread_diagnostics::Method;
        assert_eq!(Method::from_name("$changeMany"), Some(Method::ChangeMany));
        assert_eq!(Method::from_name("$clear").map(Method::name), Some("$clear"));
        assert_eq!(Method::from_name("$nope"), None);
    }

    #[test]
    fn fr_e1_argument_codecs_round_trip_and_name_what_is_wrong() {
        use main_thread_output_service::{RegisterArgs, UpdateArgs};
        // $register(label: string, file: UriComponents, languageId: string | undefined, extensionId: string)
        let wire = vec![
            Arg::Json(json!("Git")),
            Arg::Json(json!({ "scheme": "output", "path": "/git" })),
            Arg::Undefined,
            Arg::Json(json!("vscode.git")),
        ];
        let a = RegisterArgs::decode(&wire).unwrap();
        assert_eq!(a.label, "Git");
        assert_eq!(a.file.path, "/git");
        assert_eq!(a.language_id, None);
        assert_eq!(a.extension_id, "vscode.git");
        assert_eq!(RegisterArgs::decode(&a.encode().unwrap()).unwrap(), a);

        // $update(channelId: string, mode: OutputChannelUpdateMode, till?: number): `till` left out.
        let u = UpdateArgs::decode(&[Arg::Json(json!("ch")), Arg::Json(json!(1))]).unwrap();
        assert_eq!(u.till, None);
        assert_eq!(u.encode().unwrap().len(), 2, "a trailing undefined is not sent");

        let e = RegisterArgs::decode(&wire[..1]).unwrap_err().to_string();
        assert!(e.contains("file"), "{e}");
        let e = UpdateArgs::decode(&[Arg::Json(json!(7))]).unwrap_err().to_string();
        assert!(e.contains("channelId"), "{e}");
    }

    struct Diagnostics(Vec<String>);

    impl main_thread_diagnostics::Diagnostics for Diagnostics {
        fn clear(&mut self, cx: &Ctx, args: main_thread_diagnostics::ClearArgs) -> Outcome {
            self.0.push(format!("{} {}", cx.req, args.owner));
            Outcome::Reply(Reply::Empty)
        }
    }

    fn request(proxy: &'static str, method: &str, args: Vec<Arg>) -> IncomingRequest {
        IncomingRequest { req: 9, rpc_id: 0, proxy: Some(proxy), method: method.into(), args, cancellable: false }
    }

    #[test]
    fn fr_e1_dispatch_reaches_the_implemented_method_and_defaults_the_rest() {
        let mut d = Diagnostics(Vec::new());
        let mut actors = Actors { main_thread_diagnostics: Some(&mut d), ..Default::default() };
        let clear = request(main_thread_diagnostics::PROXY, "$clear", vec![Arg::Json(json!("ts"))]);
        assert_eq!(dispatch(&mut actors, &clear).unwrap(), Some(Outcome::Reply(Reply::Empty)));
        let many = request(main_thread_diagnostics::PROXY, "$changeMany", vec![Arg::Json(json!("ts")), Arg::Json(json!([]))]);
        assert_eq!(dispatch(&mut actors, &many).unwrap(), Some(Outcome::Unimplemented));
        let other = request(main_thread_output_service::PROXY, "$dispose", vec![Arg::Json(json!("ch"))]);
        assert_eq!(dispatch(&mut actors, &other).unwrap(), Some(Outcome::Unimplemented));
        let unknown = request(main_thread_diagnostics::PROXY, "$nope", vec![]);
        assert_eq!(dispatch(&mut actors, &unknown).unwrap(), None);
        let bad = request(main_thread_diagnostics::PROXY, "$clear", vec![]);
        assert!(dispatch(&mut actors, &bad).is_err());
        drop(actors);
        assert_eq!(d.0, vec!["9 ts".to_string()]);
    }
}
