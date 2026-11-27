//! The bundled app fonts: Inter, then Pretendard, then system fonts, for every role but Mono;
//! JetBrains Mono, then Pretendard, for Mono (design review 2026-11-13 C6). The launcher
//! registers Mono on first need and Inter is subset to what Pretendard lacks (NFR-L1, #1082).

use std::collections::BTreeSet;
use std::path::Path;

use compose_rust::{ColorScheme, TypeRole};
use ember_app::ui::fonts::{
    app_font, app_theme, editor_theme, mono_font, mono_registered, text_role, Face, INTER, JETBRAINS_MONO, LICENSES,
    PRETENDARD,
};

fn fonts_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/fonts"))
}

#[test]
fn every_bundled_face_is_an_sfnt_font() {
    for face in INTER.iter().chain(PRETENDARD.iter()).chain(JETBRAINS_MONO.iter()) {
        let magic = &face.bytes[..4];
        assert!(
            magic == [0x00, 0x01, 0x00, 0x00] || magic == b"OTTO",
            "{} is not a TrueType or OpenType font",
            face.file
        );
        assert!(fonts_dir().join(face.file).is_file(), "{} is missing", face.file);
    }
}

#[test]
fn license_texts_sit_beside_the_faces() {
    for license in LICENSES {
        let text = std::fs::read_to_string(fonts_dir().join(license)).expect(license);
        assert!(text.contains("SIL Open Font License, Version 1.1"), "{license} is not the OFL");
    }
}

/// Runs `f` on a thread of its own, so the thread-local `Mono` registration starts unset.
fn on_fresh_thread(f: impl FnOnce() + Send + 'static) {
    std::thread::spawn(f).join().expect("the test thread");
}

#[test]
fn app_theme_sets_the_bundled_font_and_jetbrains_mono_on_first_need() {
    on_fresh_thread(|| {
        let theme = app_theme(ColorScheme::Light);
        assert!(theme.font(TypeRole::Body).is_some_and(|id| id != 0));
        assert!(theme.font(TypeRole::Title).is_some_and(|id| id != 0));
        assert_eq!(theme.font(TypeRole::Body), theme.font(TypeRole::Title));
        assert_eq!(theme.font(TypeRole::Body), Some(app_font()));
        // NFR-L1 (#1082): the launcher starts without JetBrains Mono.
        assert_eq!(theme.font(TypeRole::Mono), None);
        assert!(!mono_registered());
        // The first mono text registers it and gives the running theme its `Mono` font.
        compose_rust::theme::set_theme(theme);
        assert_eq!(text_role(TypeRole::Mono), TypeRole::Mono);
        assert!(mono_registered());
        let running = compose_rust::theme::current_theme();
        assert_eq!(running.font(TypeRole::Mono), Some(mono_font()));
        assert_ne!(running.font(TypeRole::Mono), running.font(TypeRole::Body));
        assert_eq!(app_theme(ColorScheme::Dark).font(TypeRole::Mono), Some(mono_font()));
    });
}

#[test]
fn design_c6_jetbrains_mono_ships_with_its_license() {
    let files: Vec<&str> = JETBRAINS_MONO.iter().map(|f| f.file).collect();
    assert_eq!(files, ["JetBrainsMono-Regular.ttf", "JetBrainsMono-Medium.ttf", "JetBrainsMono-Italic.ttf"]);
    assert!(LICENSES.contains(&"JetBrainsMono-OFL.txt"));
    let text = std::fs::read_to_string(fonts_dir().join("JetBrainsMono-OFL.txt")).unwrap();
    assert!(text.contains("JetBrains Mono Project Authors"));
    on_fresh_thread(|| {
        // The editor window is a code view: Mono is set from the start.
        assert_eq!(editor_theme(ColorScheme::Dark).font(TypeRole::Mono), Some(mono_font()));
    });
}

/// The code points a face maps to a glyph other than `.notdef`, read from its `cmap` table
/// (format 4 or 12, the two the bundled faces carry).
fn cmap(face: &Face) -> BTreeSet<u32> {
    let b = face.bytes;
    let u16_at = |at: usize| u32::from(u16::from_be_bytes([b[at], b[at + 1]]));
    let u32_at = |at: usize| u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    let tables = u16_at(4) as usize;
    let cmap = (0..tables)
        .map(|i| 12 + 16 * i)
        .find(|&rec| &b[rec..rec + 4] == b"cmap")
        .map(|rec| u32_at(rec + 8) as usize)
        .unwrap_or_else(|| panic!("{} has no cmap", face.file));
    let subtables: Vec<(u32, u32, usize)> = (0..u16_at(cmap + 2) as usize)
        .map(|i| cmap + 4 + 8 * i)
        .map(|rec| (u16_at(rec), u16_at(rec + 2), cmap + u32_at(rec + 4) as usize))
        .collect();
    let pick = |platform: u32, encoding: u32| {
        subtables.iter().find(|(p, e, _)| (*p, *e) == (platform, encoding)).map(|(_, _, at)| *at)
    };
    let mut points = BTreeSet::new();
    if let Some(at) = pick(3, 10).or_else(|| pick(0, 4)) {
        assert_eq!(u16_at(at), 12, "{} full-repertoire subtable", face.file);
        for g in 0..u32_at(at + 12) as usize {
            let group = at + 16 + 12 * g;
            let (start, end, glyph) = (u32_at(group), u32_at(group + 4), u32_at(group + 8));
            for c in start..=end {
                if glyph + (c - start) != 0 {
                    points.insert(c);
                }
            }
        }
        return points;
    }
    let at = pick(3, 1).or_else(|| pick(0, 3)).unwrap_or_else(|| panic!("{} has no Unicode cmap", face.file));
    assert_eq!(u16_at(at), 4, "{} BMP subtable", face.file);
    let segs = (u16_at(at + 6) / 2) as usize;
    let ends = at + 14;
    let starts = ends + 2 * segs + 2;
    let deltas = starts + 2 * segs;
    let offsets = deltas + 2 * segs;
    for s in 0..segs {
        let (start, end) = (u16_at(starts + 2 * s), u16_at(ends + 2 * s));
        let delta = u16_at(deltas + 2 * s);
        let range_offset = u16_at(offsets + 2 * s) as usize;
        for c in start..=end.min(0xfffe) {
            let glyph = if range_offset == 0 {
                (c + delta) & 0xffff
            } else {
                let g = u16_at(offsets + 2 * s + range_offset + 2 * (c - start) as usize);
                if g == 0 { 0 } else { (g + delta) & 0xffff }
            };
            if glyph != 0 {
                points.insert(c);
            }
        }
    }
    points
}

/// The code points of `from..=to` that `points` lacks, other than `except`.
fn missing(points: &BTreeSet<u32>, from: u32, to: u32, except: &[u32]) -> Vec<String> {
    (from..=to).filter(|c| !points.contains(c) && !except.contains(c)).map(|c| format!("U+{c:04X}")).collect()
}

/// NFR-L1 (#1082): Inter is subset to the code points Pretendard does not already draw. Inter
/// itself still draws Latin, Latin-1, Latin Extended A, B and Additional, the combining marks of
/// NFD file names, general punctuation and currency, so Latin text keeps its face; the stack with
/// Pretendard still draws Hangul and everything else it drew before the subset. Each `except`
/// list is what the unmodified Inter 4.1 face lacked too (checked with fontTools, recipe in
/// `assets/fonts/Inter-SUBSET.txt`).
#[test]
fn nfr_l1_subset_inter_with_pretendard_keeps_the_app_stacks_coverage() {
    for (inter, pretendard) in INTER.iter().zip(PRETENDARD.iter()) {
        let i = cmap(inter);
        let stack: BTreeSet<u32> = i.union(&cmap(pretendard)).copied().collect();
        let name = inter.file;
        // Inter alone: the Latin text of the UI.
        let inter_rows: [(u32, u32, &[u32]); 7] = [
            (0x0020, 0x007e, &[]),
            (0x00a0, 0x00ff, &[0x00ad]),
            (0x0100, 0x017f, &[0x0149]),
            (0x0180, 0x024f, &[0x01c4]),
            (0x1e00, 0x1eff, &[0x1e9c]),
            (0x2010, 0x2027, &[]),
            (0x20a9, 0x20ac, &[]),
        ];
        for (from, to, except) in inter_rows {
            assert_eq!(missing(&i, from, to, except), Vec::<String>::new(), "{name} U+{from:04X}..U+{to:04X}");
        }
        // The marks a decomposed (NFD) Latin file name carries, from Inter.
        let nfd_marks = [0x300, 0x301, 0x302, 0x303, 0x304, 0x306, 0x307, 0x308, 0x309, 0x30a, 0x30c, 0x323, 0x327, 0x328];
        for mark in nfd_marks {
            assert!(i.contains(&mark), "{name} lacks U+{mark:04X}");
        }
        // The stack: Pretendard behind Inter.
        let stack_rows: [(u32, u32, &[u32]); 9] = [
            (0x00a0, 0x00ff, &[]),
            (0x0300, 0x036f, &[0x031f, 0x0320]),
            (0x0370, 0x03e1, &[0x0378, 0x0379, 0x0380, 0x0381, 0x0382, 0x0383, 0x038b, 0x038d, 0x03a2]),
            (0x0400, 0x045f, &[]),
            (0x2010, 0x205e, &[0x2028, 0x2029, 0x202a, 0x202b, 0x202c, 0x202d, 0x202e]),
            (0x20a0, 0x20c0, &[]),
            (0x3131, 0x3163, &[]),
            (0xac00, 0xd7a3, &[]),
            (0x3000, 0x3003, &[]),
        ];
        for (from, to, except) in stack_rows {
            assert_eq!(missing(&stack, from, to, except), Vec::<String>::new(), "{name} stack U+{from:04X}..U+{to:04X}");
        }
        // The subset itself: Greek and Cyrillic now come from Pretendard, and the face is smaller.
        assert!(!i.contains(&0x03b1) && !i.contains(&0x0430), "{name} still carries Greek or Cyrillic");
        assert!(inter.bytes.len() < 420_000, "{name} is {} bytes, not the subset", inter.bytes.len());
    }
}

/// NFR-L1 (#1082): the three weights the prose roles name stay three faces of their own, in the
/// same order in both families, so the stack's weight fallback is unchanged by the subset.
#[test]
fn nfr_l1_prose_weights_stay_paired() {
    let weight = |face: &Face| {
        let b = face.bytes;
        let tables = u16::from_be_bytes([b[4], b[5]]) as usize;
        let rec = (0..tables).map(|i| 12 + 16 * i).find(|&r| &b[r..r + 4] == b"OS/2").expect("OS/2");
        let at = u32::from_be_bytes([b[rec + 8], b[rec + 9], b[rec + 10], b[rec + 11]]) as usize;
        u16::from_be_bytes([b[at + 4], b[at + 5]])
    };
    let inter: Vec<u16> = INTER.iter().map(weight).collect();
    let pretendard: Vec<u16> = PRETENDARD.iter().map(weight).collect();
    assert_eq!(inter, [400, 500, 600]);
    assert_eq!(pretendard, [400, 500, 600]);
}

/// NFR-L1 (#1082): the subset is a Modified Version under the OFL; the note that says so, and
/// how it was made, sits beside the license.
#[test]
fn nfr_l1_inter_subset_is_noted_beside_its_license() {
    let note = std::fs::read_to_string(fonts_dir().join("Inter-SUBSET.txt")).expect("Inter-SUBSET.txt");
    assert!(note.contains("Modified Version"));
    assert!(note.contains("Inter-OFL.txt"));
    let license = std::fs::read_to_string(fonts_dir().join("Inter-OFL.txt")).unwrap();
    assert!(!license.contains("with Reserved Font Name"), "Inter reserves no font name, so the subset keeps it");
}
