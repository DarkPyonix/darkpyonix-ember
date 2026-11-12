//! The extensions panel's view of a scan (SPEC FR-L31.14, #446).
//!
//! `remoteExtensionsScanner.scanExtensions` answers `IExtensionDescription[]`
//! (`src/vs/platform/extensions/common/extensions.ts`): the manifest fields plus `identifier`,
//! `isBuiltin`, `isUserBuiltin` and `isUnderDevelopment`. The descriptions go to the extension
//! host untouched; this module reads the few fields a list row shows, and leaves the disabled
//! ones out of what the extension host is given.
//!
//! Enablement follows Code-OSS: the workbench keeps the disabled ids in its own storage
//! (`extensionsIdentifiers/disabled`, `extensionEnablementService.ts`) and does not hand a
//! disabled extension to the extension host. The server is never told.

use serde_json::Value;

/// One scanned extension as the panel lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledExtension {
    /// `publisher.name` as the scan spells it (`identifier.value`).
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub version: String,
    /// `displayName`, or `name` when the manifest has none or left an unresolved `%key%`.
    pub display_name: String,
    pub description: String,
    /// Shipped with the OSE: it cannot be uninstalled.
    pub builtin: bool,
}

impl InstalledExtension {
    /// The id in the form ids are compared in (Code-OSS compares them without regard to case).
    pub fn key(&self) -> String {
        id_key(&self.id)
    }
}

/// The form extension ids are compared in.
pub fn id_key(id: &str) -> String {
    id.trim().to_lowercase()
}

fn text(desc: &Value, field: &str) -> String {
    desc.get(field).and_then(Value::as_str).unwrap_or_default().trim().to_string()
}

/// A manifest string the scanner did not translate is still `%key%`.
fn unresolved(s: &str) -> bool {
    s.len() >= 2 && s.starts_with('%') && s.ends_with('%')
}

/// The id of a description: `identifier.value`, else `publisher.name` from the manifest.
fn description_id(desc: &Value) -> Option<String> {
    if let Some(id) = ember_editor_conn::management::extension_id(desc).filter(|s| !s.trim().is_empty()) {
        return Some(id.trim().to_string());
    }
    let (publisher, name) = (text(desc, "publisher"), text(desc, "name"));
    (!publisher.is_empty() && !name.is_empty()).then(|| format!("{publisher}.{name}"))
}

/// The list rows of a scan, in scan order. A description without an id is skipped.
pub fn parse_installed(extensions: &[Value]) -> Vec<InstalledExtension> {
    extensions
        .iter()
        .filter_map(|desc| {
            let id = description_id(desc)?;
            let name = text(desc, "name");
            let display = text(desc, "displayName");
            let display_name = if display.is_empty() || unresolved(&display) {
                if name.is_empty() { id.clone() } else { name.clone() }
            } else {
                display
            };
            let description = Some(text(desc, "description")).filter(|d| !unresolved(d)).unwrap_or_default();
            Some(InstalledExtension {
                publisher: text(desc, "publisher"),
                version: text(desc, "version"),
                builtin: desc.get("isBuiltin").and_then(Value::as_bool).unwrap_or(false),
                id,
                name,
                display_name,
                description,
            })
        })
        .collect()
}

/// The descriptions the extension host is given: the scan without the disabled ids (compared
/// without regard to case). The order of the scan is kept.
pub fn enabled_only(extensions: &[Value], disabled: &[String]) -> Vec<Value> {
    if disabled.is_empty() {
        return extensions.to_vec();
    }
    let off: std::collections::HashSet<String> = disabled.iter().map(|d| id_key(d)).collect();
    extensions
        .iter()
        .filter(|desc| description_id(desc).map_or(true, |id| !off.contains(&id_key(&id))))
        .cloned()
        .collect()
}

/// The rows of one group (built-in or not), sorted by display name without regard to case, then
/// by id so the order is stable.
pub fn group(list: &[InstalledExtension], builtin: bool) -> Vec<InstalledExtension> {
    let mut rows: Vec<InstalledExtension> = list.iter().filter(|e| e.builtin == builtin).cloned().collect();
    rows.sort_by(|a, b| {
        a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()).then_with(|| a.key().cmp(&b.key()))
    });
    rows
}
