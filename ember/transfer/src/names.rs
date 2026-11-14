//! Name cleaning (SPEC `FR-F9`, `FR-F8`, `#527`). A name from the network is never trusted.
//!
//! Rules for [`clean_file_name`]:
//! - take the last non-empty component after splitting on `/` and `\`;
//! - strip NTFS Alternate Data Stream suffixes (`:stream` after the first `:`);
//! - remove ASCII and C1 control characters (`U+0000..=U+001F`, `U+007F..=U+009F`), Unicode bidi
//!   overrides (`U+202A..=U+202E`, `U+2066..=U+2069`), zero-width characters (`U+200B..=U+200D`,
//!   `U+FEFF`), and line/paragraph separators (`U+2028`, `U+2029`);
//! - strip leading and trailing dots and spaces;
//! - prefix `_` to Windows reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1`..`COM9`,
//!   `LPT1`..`LPT9`, case-insensitive, bare or with an extension);
//! - cap at [`MAX_NAME_BYTES`] bytes on a char boundary.
//! An empty result gets `received-<8 hex>`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// The longest cleaned single component name, in bytes.
pub const MAX_NAME_BYTES: usize = 200;
/// Maximum number of components in a cleaned relative path (`#527`).
pub const MAX_PATH_DEPTH: usize = 32;
/// Maximum total bytes across all components of a cleaned relative path (`#527`).
pub const MAX_PATH_BYTES: usize = 1024;
/// Maximum number of files in one offer (`#527`).
pub const MAX_OFFER_FILES: usize = 1024;

static FALLBACK_SEQ: AtomicU32 = AtomicU32::new(1);

fn is_banned(c: char) -> bool {
    matches!(
        c,
        '\u{0}'..='\u{1F}'
            | '\u{7F}'..='\u{9F}'
            | '\u{200B}'..='\u{200D}'
            | '\u{2028}'..='\u{2029}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

fn is_windows_reserved(stem: &str) -> bool {
    let up = stem.to_ascii_uppercase();
    matches!(
        up.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// `received-<8 hex>` from the system random source, with a monotonic + timestamp fallback so two
/// calls never silently collide if the OS RNG fails (`#527`).
pub fn generated_name() -> String {
    let mut b = [0u8; 4];
    if getrandom::fill(&mut b).is_err() {
        let seq = FALLBACK_SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        b = (seq ^ nanos).to_be_bytes();
    }
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("received-{hex}")
}

/// Splits `name` into `(stem, ext)` where `ext` includes the leading `.` when present.
pub fn split_stem_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// Cleans one path component. `None` when nothing is left.
fn clean_component(raw: &str) -> Option<String> {
    // Strip NTFS Alternate Data Streams (`name:stream:$DATA`).
    let before_ads = raw.split(':').next().unwrap_or("");
    let no_ctl: String = before_ads.chars().filter(|c| !is_banned(*c)).collect();
    let trimmed = no_ctl
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return None;
    }
    let mut end = trimmed.len().min(MAX_NAME_BYTES);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let capped = trimmed[..end].trim_end_matches(['.', ' ']);
    if capped.is_empty() {
        return None;
    }
    let stem = capped.split('.').next().unwrap_or(capped);
    let mut out = if is_windows_reserved(stem) {
        format!("_{capped}")
    } else {
        capped.to_string()
    };
    if out.len() > MAX_NAME_BYTES {
        let mut e = MAX_NAME_BYTES;
        while !out.is_char_boundary(e) {
            e -= 1;
        }
        out.truncate(e);
    }
    Some(out)
}

/// The safe file name for `raw` (basename only).
pub fn clean_file_name(raw: &str) -> String {
    raw.split(['/', '\\'])
        .rfind(|s| !s.is_empty())
        .and_then(clean_component)
        .unwrap_or_else(generated_name)
}

/// The safe relative path of a folder entry, as components. Empty and `.` components are dropped,
/// `..` is dropped (a path never escapes), every component is cleaned; a component with nothing
/// left is dropped. Depth is capped at [`MAX_PATH_DEPTH`] and total length at [`MAX_PATH_BYTES`].
/// If no component is left the result is one generated name.
pub fn clean_relative_path(raw: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut total_bytes: usize = 0;
    for seg in raw.split(['/', '\\']) {
        if seg.is_empty() || seg == "." || seg == ".." {
            continue;
        }
        let Some(cleaned) = clean_component(seg) else {
            continue;
        };
        let add = cleaned.len() + usize::from(!parts.is_empty());
        if parts.len() >= MAX_PATH_DEPTH || total_bytes + add > MAX_PATH_BYTES {
            break;
        }
        total_bytes += add;
        parts.push(cleaned);
    }
    if parts.is_empty() {
        vec![generated_name()]
    } else {
        parts
    }
}

/// Cleans every entry of an offer and disambiguates duplicate cleaned relative paths
/// case-insensitively (`#527`: e.g. `x` and `.x`, or `a/b` and `a\b` in the same offer become
/// `x` and `x (1)`, `a/b` and `a/b (1)`).
pub fn clean_offer_paths(raw_names: &[String]) -> Vec<Vec<String>> {
    let mut seen: HashSet<String> = HashSet::with_capacity(raw_names.len());
    let mut out = Vec::with_capacity(raw_names.len());
    for raw in raw_names {
        let mut comps = clean_relative_path(raw);
        let base_last = comps.last().cloned().unwrap_or_else(generated_name);
        let (stem, ext) = split_stem_ext(&base_last);
        let (stem, ext) = (stem.to_string(), ext.to_string());
        let mut n = 0u32;
        loop {
            let key = comps
                .iter()
                .map(|c| c.to_lowercase())
                .collect::<Vec<_>>()
                .join("/");
            if seen.insert(key) {
                break;
            }
            n += 1;
            if let Some(last) = comps.last_mut() {
                *last = format!("{stem} ({n}){ext}");
            }
        }
        out.push(comps);
    }
    out
}

/// Checks whether `dir` already contains an entry matching `name` (exact or ASCII/Unicode
/// case-insensitive on disk).
fn entry_exists_in_dir(dir: &Path, name: &str) -> bool {
    if dir.join(name).symlink_metadata().is_ok() {
        return true;
    }
    let want = name.to_lowercase();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in rd.flatten() {
        if entry.file_name().to_string_lossy().to_lowercase() == want {
            return true;
        }
    }
    false
}

/// `root` joined with the cleaned `components`; if that path exists (or clashes case-insensitively
/// with an existing entry), the last component gets a numbered suffix: `name (1).ext`,
/// `name (2).ext`, ... (`FR-F8`). If a parent directory component clashes with an existing
/// non-directory file on disk, that parent component also gets a numbered suffix (`#527`).
/// Never overwrites. Touches no file.
pub fn resolve_destination(root: &Path, components: &[String]) -> PathBuf {
    let mut dir = root.to_path_buf();
    let safe: Vec<String> = components
        .iter()
        .take(MAX_PATH_DEPTH)
        .map(|c| {
            if c.is_empty() || c == "." || c == ".." || c.contains(['/', '\\']) {
                generated_name()
            } else {
                c.clone()
            }
        })
        .collect();
    let Some((last, parents)) = safe.split_last() else {
        return root.join(generated_name());
    };
    for p in parents {
        let mut candidate_dir = p.clone();
        let mut n = 1u32;
        while let Ok(meta) = dir.join(&candidate_dir).symlink_metadata() {
            if meta.is_dir() {
                break;
            }
            candidate_dir = format!("{p} ({n})");
            n += 1;
        }
        dir.push(candidate_dir);
    }
    if !entry_exists_in_dir(&dir, last) {
        return dir.join(last);
    }
    let (stem, ext) = split_stem_ext(last);
    let mut n = 1u32;
    loop {
        let candidate_name = format!("{stem} ({n}){ext}");
        if !entry_exists_in_dir(&dir, &candidate_name) {
            return dir.join(candidate_name);
        }
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_generated(s: &str) -> bool {
        s.len() == 17
            && s.starts_with("received-")
            && s[9..].chars().all(|c| c.is_ascii_hexdigit())
    }

    #[test]
    fn fr_f9_leading_dot_is_stripped() {
        assert_eq!(clean_file_name(".bashrc"), "bashrc");
    }

    #[test]
    fn fr_f9_trailing_dots_and_ads_are_stripped() {
        assert_eq!(clean_file_name("a..."), "a");
        assert_eq!(clean_file_name("report.txt:zone.identifier:$DATA"), "report.txt");
    }

    #[test]
    fn fr_f9_separators_give_one_basename() {
        assert_eq!(clean_file_name("a/b\\c"), "c");
        assert_eq!(clean_file_name("a/b/"), "b");
    }

    #[test]
    fn fr_f9_control_and_zero_width_characters_are_removed() {
        assert!(is_generated(&clean_file_name("\u{0}\u{1}\u{7f}\n")));
        assert_eq!(clean_file_name("a\u{200B}b\u{FEFF}c\u{0085}d\u{2028}e.txt"), "abcde.txt");
    }

    #[test]
    fn fr_f9_parent_escape_in_file_name() {
        assert_eq!(clean_file_name("../x"), "x");
        assert_eq!(clean_relative_path("d/../../x"), vec!["d", "x"]);
    }

    #[test]
    fn fr_f9_long_name_is_capped_on_char_boundary() {
        let long = "a".repeat(300);
        assert_eq!(clean_file_name(&long).len(), 200);
        let wide = "한".repeat(100); // 300 bytes
        let out = clean_file_name(&wide);
        assert!(out.len() <= 200);
        assert_eq!(out, "한".repeat(66));
    }

    #[test]
    fn fr_f9_path_depth_and_total_bytes_are_capped() {
        let deep = (0..60).map(|i| format!("d{i}")).collect::<Vec<_>>().join("/");
        let cleaned = clean_relative_path(&deep);
        assert_eq!(cleaned.len(), MAX_PATH_DEPTH);
    }

    #[test]
    fn fr_f9_bidi_override_is_removed() {
        assert_eq!(clean_file_name("a\u{202E}txt.exe"), "atxt.exe");
        assert_eq!(clean_file_name("\u{2066}b\u{2069}"), "b");
    }

    #[test]
    fn fr_f9_empty_and_dots_are_generated() {
        assert!(is_generated(&clean_file_name("")));
        assert!(is_generated(&clean_file_name("....")));
        assert!(is_generated(&clean_file_name("..")));
        assert!(is_generated(&clean_file_name("/")));
    }

    #[test]
    fn fr_f9_reserved_device_names_are_prefixed() {
        assert_eq!(clean_file_name("CON"), "_CON");
        assert_eq!(clean_file_name("con.txt"), "_con.txt");
        assert_eq!(clean_file_name("NUL."), "_NUL");
        assert_eq!(clean_file_name("lpt1.log"), "_lpt1.log");
        assert_eq!(clean_file_name("CONSOLE.txt"), "CONSOLE.txt");
    }

    #[test]
    fn fr_f9_duplicate_cleaned_paths_in_one_offer_are_disambiguated() {
        let raw = vec![
            "x".to_string(),
            ".x".to_string(),
            "a/b.txt".to_string(),
            "a\\b.txt".to_string(),
        ];
        let cleaned = clean_offer_paths(&raw);
        assert_eq!(cleaned[0], vec!["x"]);
        assert_eq!(cleaned[1], vec!["x (1)"]);
        assert_eq!(cleaned[2], vec!["a", "b.txt"]);
        assert_eq!(cleaned[3], vec!["a", "b (1).txt"]);
    }

    #[test]
    fn fr_f9_unicode_name_keeps_characters() {
        assert_eq!(clean_file_name("보고서 ✓ é.txt"), "보고서 ✓ é.txt");
    }

    #[test]
    fn fr_f9_relative_path_components_are_cleaned() {
        assert_eq!(clean_relative_path("./a//.b\\c.txt"), vec!["a", "b", "c.txt"]);
        let all_gone = clean_relative_path("../..");
        assert_eq!(all_gone.len(), 1);
        assert!(is_generated(&all_gone[0]));
    }

    #[test]
    fn fr_f8_clash_gets_numbered_suffix_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let comps = vec!["a.txt".to_string()];
        let p1 = resolve_destination(dir.path(), &comps);
        assert_eq!(p1, dir.path().join("a.txt"));
        std::fs::write(&p1, b"x").unwrap();
        let p2 = resolve_destination(dir.path(), &comps);
        assert_eq!(p2, dir.path().join("a (1).txt"));
        std::fs::write(&p2, b"x").unwrap();
        assert_eq!(resolve_destination(dir.path(), &comps), dir.path().join("a (2).txt"));
        let noext = vec!["Makefile".to_string()];
        std::fs::write(dir.path().join("Makefile"), b"x").unwrap();
        assert_eq!(resolve_destination(dir.path(), &noext), dir.path().join("Makefile (1)"));
        // Parent component clashing with an existing regular file gets a numbered directory name.
        let under_file = vec!["Makefile".to_string(), "sub.txt".to_string()];
        assert_eq!(
            resolve_destination(dir.path(), &under_file),
            dir.path().join("Makefile (1)").join("sub.txt")
        );
    }

    #[test]
    fn fr_f9_destination_stays_inside_root() {
        let root = Path::new("/r");
        let p = resolve_destination(root, &["..".to_string(), "x".to_string()]);
        assert!(p.starts_with(root));
        assert!(!p.components().any(|c| c.as_os_str() == ".."));
    }
}
