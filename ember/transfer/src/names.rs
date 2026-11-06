//! Name cleaning (SPEC `FR-F9`, `FR-F8`). A name from the network is never trusted.
//!
//! Rules for [`clean_file_name`]: take the last non-empty component after splitting on `/` and
//! `\`; remove control characters (U+0000..U+001F, U+007F) and Unicode bidi overrides
//! (U+202A..U+202E, U+2066..U+2069); strip leading dots and spaces; trim trailing spaces; cap at
//! [`MAX_NAME_BYTES`] bytes on a char boundary. An empty result gets `received-<8 hex>`.
//!
//! A Windows-reserved-looking name (`CON`, `NUL`, ...) is NOT special-cased: the receiver stores
//! files under names the platform accepts, and a reserved-name rule belongs to the platform layer
//! that writes the file (follow-up), not to the shared cleaner.

use std::path::{Path, PathBuf};

/// The longest cleaned name, in bytes.
pub const MAX_NAME_BYTES: usize = 200;

fn is_banned(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1F}' | '\u{7F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `received-<8 hex>` from the system random source (`00000000` if it fails).
pub fn generated_name() -> String {
    let mut b = [0u8; 4];
    if getrandom::fill(&mut b).is_err() {
        b = [0; 4];
    }
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("received-{hex}")
}

/// Cleans one path component. `None` when nothing is left.
fn clean_component(raw: &str) -> Option<String> {
    let no_ctl: String = raw.chars().filter(|c| !is_banned(*c)).collect();
    let trimmed = no_ctl
        .trim_start_matches(['.', ' '])
        .trim_end_matches(' ');
    if trimmed.is_empty() {
        return None;
    }
    let mut end = trimmed.len().min(MAX_NAME_BYTES);
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    let capped = trimmed[..end].trim_end_matches(' ');
    if capped.is_empty() {
        None
    } else {
        Some(capped.to_string())
    }
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
/// left is dropped. If no component is left the result is one generated name.
pub fn clean_relative_path(raw: &str) -> Vec<String> {
    let parts: Vec<String> = raw
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .filter_map(clean_component)
        .collect();
    if parts.is_empty() {
        vec![generated_name()]
    } else {
        parts
    }
}

/// `root` joined with the cleaned `components`; if that path exists, the last component gets a
/// numbered suffix: `name (1).ext`, `name (2).ext`, ... (FR-F8). Never overwrites. Touches no file.
///
/// The components must come from [`clean_relative_path`]; one that is not a plain name (empty,
/// `.`, `..`, or containing a separator) is replaced by a generated name, so the result is always
/// inside `root`. A clash on a directory component is not renamed (follow-up with the writer).
pub fn resolve_destination(root: &Path, components: &[String]) -> PathBuf {
    let mut dir = root.to_path_buf();
    let safe: Vec<String> = components
        .iter()
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
        dir.push(p);
    }
    let first = dir.join(last);
    if first.symlink_metadata().is_err() {
        return first;
    }
    let (stem, ext) = match last.rfind('.') {
        Some(i) if i > 0 => (&last[..i], &last[i..]),
        _ => (last.as_str(), ""),
    };
    let mut n = 1u32;
    loop {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if candidate.symlink_metadata().is_err() {
            return candidate;
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
    fn fr_f9_separators_give_one_basename() {
        assert_eq!(clean_file_name("a/b\\c"), "c");
        assert_eq!(clean_file_name("a/b/"), "b");
    }

    #[test]
    fn fr_f9_control_only_name_is_generated() {
        assert!(is_generated(&clean_file_name("\u{0}\u{1}\u{7f}\n")));
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
    fn fr_f9_reserved_looking_name_is_kept() {
        assert_eq!(clean_file_name("CON"), "CON");
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
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("names-{}", generated_name()));
        std::fs::create_dir_all(&dir).unwrap();
        let comps = vec!["a.txt".to_string()];
        let p1 = resolve_destination(&dir, &comps);
        assert_eq!(p1, dir.join("a.txt"));
        std::fs::write(&p1, b"x").unwrap();
        let p2 = resolve_destination(&dir, &comps);
        assert_eq!(p2, dir.join("a (1).txt"));
        std::fs::write(&p2, b"x").unwrap();
        assert_eq!(resolve_destination(&dir, &comps), dir.join("a (2).txt"));
        let noext = vec!["Makefile".to_string()];
        std::fs::write(dir.join("Makefile"), b"x").unwrap();
        assert_eq!(resolve_destination(&dir, &noext), dir.join("Makefile (1)"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn fr_f9_destination_stays_inside_root() {
        let root = Path::new("/r");
        let p = resolve_destination(root, &["..".to_string(), "x".to_string()]);
        assert!(p.starts_with(root));
        assert!(!p.components().any(|c| c.as_os_str() == ".."));
    }
}
