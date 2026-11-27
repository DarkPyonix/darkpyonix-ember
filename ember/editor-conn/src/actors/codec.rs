//! Argument codecs the generated actor code calls: one positional RPC argument to a Rust value
//! and back.
//!
//! A JS caller may leave trailing optional arguments out, pass `undefined` (mixed encoding,
//! [`Arg::Undefined`]) or pass `null`. All three decode to `None` for an optional parameter. A
//! required parameter that is missing is an error naming the parameter.

use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;

use crate::rpc::Arg;
use crate::{Error, Result};

pub use crate::uri::UriComponents;
pub use serde_json::Value;

/// A `VSBuffer` argument: raw bytes, sent as a buffer in the mixed encoding.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VsBuffer(pub Vec<u8>);

/// A `SerializableObjectWithBuffers<T>` argument: JSON whose `{"$$ref$$": i}` nodes point into
/// `buffers`. Kept as it came, because what the refs mean depends on `T`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ObjectWithBuffers {
    pub value: JsonValue,
    pub buffers: Vec<Vec<u8>>,
}

/// One argument to and from its wire form.
pub trait ArgCodec: Sized {
    fn from_arg(arg: &Arg) -> std::result::Result<Self, String>;
    fn to_arg(&self) -> Result<Arg>;
}

fn from_json<T: DeserializeOwned>(arg: &Arg) -> std::result::Result<T, String> {
    match arg {
        Arg::Json(v) | Arg::JsonWithBuffers { value: v, .. } => serde_json::from_value(v.clone()).map_err(|e| e.to_string()),
        Arg::Buffer(_) => Err("a buffer where JSON was expected".into()),
        Arg::Undefined => Err("undefined".into()),
    }
}

macro_rules! json_codec {
    ($($t:ty),*) => {$(
        impl ArgCodec for $t {
            fn from_arg(arg: &Arg) -> std::result::Result<Self, String> {
                from_json(arg)
            }
            fn to_arg(&self) -> Result<Arg> {
                Arg::json(self)
            }
        }
    )*};
}

json_codec!(String, bool, i64, f64, UriComponents);

impl ArgCodec for JsonValue {
    fn from_arg(arg: &Arg) -> std::result::Result<Self, String> {
        match arg {
            Arg::Json(v) | Arg::JsonWithBuffers { value: v, .. } => Ok(v.clone()),
            Arg::Buffer(b) => Ok(serde_json::json!({ "$vsbuffer": b })),
            Arg::Undefined => Ok(JsonValue::Null),
        }
    }
    fn to_arg(&self) -> Result<Arg> {
        Ok(Arg::Json(self.clone()))
    }
}

impl<T: DeserializeOwned + serde::Serialize> ArgCodec for Vec<T> {
    fn from_arg(arg: &Arg) -> std::result::Result<Self, String> {
        from_json(arg)
    }
    fn to_arg(&self) -> Result<Arg> {
        Arg::json(self)
    }
}

impl ArgCodec for VsBuffer {
    fn from_arg(arg: &Arg) -> std::result::Result<Self, String> {
        match arg {
            Arg::Buffer(b) => Ok(VsBuffer(b.clone())),
            _ => Err("not a buffer".into()),
        }
    }
    fn to_arg(&self) -> Result<Arg> {
        Ok(Arg::Buffer(self.0.clone()))
    }
}

impl ArgCodec for ObjectWithBuffers {
    fn from_arg(arg: &Arg) -> std::result::Result<Self, String> {
        match arg {
            Arg::JsonWithBuffers { value, buffers } => Ok(ObjectWithBuffers { value: value.clone(), buffers: buffers.clone() }),
            Arg::Json(v) => Ok(ObjectWithBuffers { value: v.clone(), buffers: Vec::new() }),
            _ => Err("not an object with buffers".into()),
        }
    }
    fn to_arg(&self) -> Result<Arg> {
        Ok(Arg::JsonWithBuffers { value: self.value.clone(), buffers: self.buffers.clone() })
    }
}

fn absent(arg: Option<&Arg>) -> bool {
    matches!(arg, None | Some(Arg::Undefined) | Some(Arg::Json(JsonValue::Null)))
}

/// A required parameter.
pub fn req<T: ArgCodec>(args: &[Arg], i: usize, name: &str) -> Result<T> {
    match args.get(i) {
        None | Some(Arg::Undefined) => Err(Error::Malformed(format!("argument {i} ({name}) is missing"))),
        Some(a) => T::from_arg(a).map_err(|e| Error::Malformed(format!("argument {i} ({name}): {e}"))),
    }
}

/// An optional parameter: missing, `undefined` and `null` are `None`.
pub fn opt<T: ArgCodec>(args: &[Arg], i: usize, name: &str) -> Result<Option<T>> {
    let a = args.get(i);
    if absent(a) {
        return Ok(None);
    }
    T::from_arg(a.expect("not absent")).map(Some).map_err(|e| Error::Malformed(format!("argument {i} ({name}): {e}")))
}

/// A rest parameter (`...args`): every argument from `i` on, as JSON.
pub fn rest(args: &[Arg], i: usize) -> Vec<JsonValue> {
    args.iter().skip(i).map(|a| JsonValue::from_arg(a).unwrap_or(JsonValue::Null)).collect()
}

pub fn put<T: ArgCodec>(out: &mut Vec<Arg>, v: &T) -> Result<()> {
    out.push(v.to_arg()?);
    Ok(())
}

pub fn put_opt<T: ArgCodec>(out: &mut Vec<Arg>, v: &Option<T>) -> Result<()> {
    out.push(match v {
        Some(v) => v.to_arg()?,
        None => Arg::Undefined,
    });
    Ok(())
}

pub fn put_rest(out: &mut Vec<Arg>, v: &[JsonValue]) {
    out.extend(v.iter().cloned().map(Arg::Json));
}

/// Drops trailing `undefined`s, as a JS call that leaves trailing optional arguments out does, so
/// a call without buffers stays in the plain JSON encoding.
pub fn trim_undefined(out: &mut Vec<Arg>) {
    while matches!(out.last(), Some(Arg::Undefined)) {
        out.pop();
    }
}
