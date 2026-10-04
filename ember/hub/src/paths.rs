//! Every path of the hub the client calls, unversioned like the real hub (core NFR-V1, INTENT
//! D15/D16): the API only grows and `/v1/*` is `404`. `hub/tests/openapi_paths.rs` checks each
//! against the vendored `hub.openapi.yaml`.

pub const CONFIG: &str = "/config";
pub const DEVICE_LINKS: &str = "/device-links";
pub const DEVICES: &str = "/devices";
pub const ME: &str = "/me";
pub const ME_RESOLVE_TOKEN: &str = "/me/resolve-token";

/// `/device-links/{link_id}`.
pub fn device_link(link_id: &str) -> String {
    format!("/device-links/{link_id}")
}
/// `/device-links/{link_id}/token`.
pub fn device_link_token(link_id: &str) -> String {
    format!("/device-links/{link_id}/token")
}
/// `/link-codes/{user_code}`.
pub fn link_code(user_code: &str) -> String {
    format!("/link-codes/{user_code}")
}
/// `/devices/{endpoint_id}`.
pub fn device(endpoint_id: impl std::fmt::Display) -> String {
    format!("/devices/{endpoint_id}")
}
/// `/devices/{endpoint_id}/readmit`.
pub fn device_readmit(endpoint_id: impl std::fmt::Display) -> String {
    format!("/devices/{endpoint_id}/readmit")
}
/// `/devices/{endpoint_id}/addresses`.
pub fn device_addresses(endpoint_id: impl std::fmt::Display) -> String {
    format!("/devices/{endpoint_id}/addresses")
}

/// Every path template the client uses, for tests (placeholders as in the OpenAPI spec). The
/// pkarr and link paths belong to `ember_transport` / the browser and are listed too.
pub const TEMPLATES: &[&str] = &[
    "/config",
    "/device-links",
    "/device-links/{link_id}",
    "/device-links/{link_id}/token",
    "/link-codes/{user_code}",
    "/me",
    "/me/resolve-token",
    "/devices",
    "/devices/{endpoint_id}",
    "/devices/{endpoint_id}/readmit",
    "/devices/{endpoint_id}/addresses",
    "/pkarr/{key}",
    "/link",
];
