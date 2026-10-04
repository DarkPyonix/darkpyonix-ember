//! Management-connection calls the extension-host bootstrap needs.
//!
//! * `'remoteextensionsenvironment'.getEnvironmentData` → `IRemoteAgentEnvironmentDTO`
//!   (`src/vs/workbench/services/remote/common/remoteAgentEnvironmentChannel.ts` L18-80;
//!   registered in `src/vs/server/node/serverServices.ts` L383).
//! * `'remoteExtensionsScanner'.scanExtensions` → `IExtensionDescription[]`
//!   (`src/vs/workbench/services/remote/common/remoteExtensionsScanner.ts` L43-61; channel name
//!   `src/vs/platform/remote/common/remoteExtensionsScanner.ts` L12).
//! * `'remoteExtensionsScanner'.whenExtensionsReady`.
//!
//! The extension descriptions are passed through to the extension host untouched (as JSON); Ember
//! only reads the few fields it needs (activation events, contributed configuration defaults).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ipc::{IpcClient, IpcValue};
use crate::uri::UriComponents;
use crate::Result;

pub const ENV_CHANNEL: &str = "remoteextensionsenvironment";
pub const SCANNER_CHANNEL: &str = "remoteExtensionsScanner";

/// `RemoteAgentConnectionContext` (`remoteAgentEnvironment.ts` L37): the IPC context the
/// management connection opens with. `remoteAuthority` selects the server's URI transformer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentConnectionContext {
    pub remote_authority: String,
    pub client_id: String,
}

impl RemoteAgentConnectionContext {
    pub fn to_ipc(&self) -> Result<IpcValue> {
        IpcValue::json(self)
    }
}

/// The subset of `IRemoteAgentEnvironmentDTO` used to build the ext-host init data. Unknown
/// fields are kept in `raw`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAgentEnvironment {
    pub pid: i64,
    pub connection_token: String,
    pub app_root: UriComponents,
    pub extension_host_logs_path: UriComponents,
    pub global_storage_home: UriComponents,
    pub workspace_storage_home: UriComponents,
    pub user_home: UriComponents,
    /// `OperatingSystem`: 1 Windows, 2 Macintosh, 3 Linux.
    pub os: u8,
    pub arch: String,
    #[serde(default)]
    pub reconnection_grace_time: Option<u64>,
    #[serde(skip)]
    pub raw: Value,
}

pub async fn get_environment_data(
    ipc: &IpcClient,
    remote_authority: &str,
    profile: Option<&str>,
) -> Result<RemoteAgentEnvironment> {
    let mut args = serde_json::json!({ "remoteAuthority": remote_authority });
    if let Some(p) = profile {
        args["profile"] = Value::from(p);
    }
    let v = ipc.call(ENV_CHANNEL, "getEnvironmentData", IpcValue::Object(args)).await?.to_json();
    let mut env: RemoteAgentEnvironment = serde_json::from_value(v.clone())?;
    env.raw = v;
    Ok(env)
}

/// `scanExtensions(language, profileLocation, workspaceExtensionLocations,
/// extensionDevelopmentLocationURI, languagePack)`.
pub async fn scan_extensions(ipc: &IpcClient, language: &str) -> Result<Vec<Value>> {
    let args = IpcValue::Array(vec![
        IpcValue::str(language),
        IpcValue::Undefined,
        IpcValue::Array(vec![]),
        IpcValue::Undefined,
        IpcValue::Undefined,
    ]);
    let v = ipc.call(SCANNER_CHANNEL, "scanExtensions", args).await?.to_json();
    Ok(match v {
        Value::Array(a) => a,
        _ => Vec::new(),
    })
}

pub async fn when_extensions_ready(ipc: &IpcClient) -> Result<Value> {
    Ok(ipc.call(SCANNER_CHANNEL, "whenExtensionsReady", IpcValue::Undefined).await?.to_json())
}

/// The `ExtensionIdentifier` value (`identifier.value`) of a scanned extension description.
pub fn extension_id(desc: &Value) -> Option<&str> {
    desc.get("identifier").and_then(|i| i.get("value")).and_then(Value::as_str)
}
