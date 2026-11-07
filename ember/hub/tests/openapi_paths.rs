//! Every hub path the client uses exists in the vendored `hub.openapi.yaml` (darkpyonix-core,
//! commit daa80b5), and none carries a `/v<N>` segment (core NFR-V1: unversioned paths).

use ember_hub::paths;

fn spec_paths() -> Vec<String> {
    let spec = include_str!("fixtures/hub.openapi.yaml");
    let mut in_paths = false;
    let mut out = Vec::new();
    for line in spec.lines() {
        if line.starts_with("paths:") {
            in_paths = true;
        } else if in_paths && !line.is_empty() && !line.starts_with(' ') && !line.starts_with('#') {
            break; // the next top-level key
        } else if in_paths && line.starts_with("  /") {
            out.push(line.trim().trim_end_matches(':').to_string());
        }
    }
    out
}

fn has_version_segment(path: &str) -> bool {
    path.split('/').any(|s| s.len() > 1 && s.starts_with('v') && s[1..].chars().all(|c| c.is_ascii_digit()))
}

#[test]
fn the_spec_has_paths() {
    let p = spec_paths();
    assert!(p.len() > 10, "{p:?}");
    assert!(p.iter().any(|x| x == "/config"));
    assert!(!p.iter().any(|x| has_version_segment(x)), "the spec is unversioned");
}

#[test]
fn every_client_path_is_in_the_spec() {
    let spec = spec_paths();
    for t in paths::TEMPLATES {
        assert!(spec.iter().any(|s| s == t), "{t} is not in hub.openapi.yaml; spec paths: {spec:?}");
    }
}

#[test]
fn the_path_builders_match_their_templates() {
    let built = [
        paths::CONFIG.to_string(),
        paths::DEVICE_LINKS.to_string(),
        paths::device_link("{link_id}"),
        paths::device_link_token("{link_id}"),
        paths::link_code("{user_code}"),
        paths::ME.to_string(),
        paths::ME_RESOLVE_TOKEN.to_string(),
        paths::DEVICES.to_string(),
        paths::device("{endpoint_id}"),
        paths::device_readmit("{endpoint_id}"),
        paths::device_addresses("{endpoint_id}"),
    ];
    for b in &built {
        assert!(paths::TEMPLATES.contains(&b.as_str()), "{b} missing from TEMPLATES");
    }
}

#[test]
fn no_client_path_has_a_version_segment() {
    for t in paths::TEMPLATES {
        assert!(!has_version_segment(t), "{t}");
    }
    assert!(has_version_segment("/v1/config") && has_version_segment("/api/v12"));
}
