//! A partial receive folder is adopted only by the offer that wrote it (SPEC `FR-F11`, #773).

use std::fs;

use ember_transfer::limits::Limits;
use ember_transfer::store::{DeclaredFile, ReceiveWriter};

fn decl(size: u64, sha: Option<&str>) -> Vec<DeclaredFile> {
    vec![DeclaredFile { name: "a.txt".into(), size, sha256: sha.map(String::from) }]
}

const LIMITS: Limits = Limits { max_bytes: 1 << 20 };

#[test]
fn fr_f11_part_file_without_record_is_not_adopted() {
    let dir = tempfile::tempdir().unwrap();
    let part = dir.path().join(".partial/tr-1");
    fs::create_dir_all(&part).unwrap();
    fs::write(part.join("0.part"), b"stale bytes").unwrap();

    let w = ReceiveWriter::open(dir.path(), "tr-1", "dev", &decl(20, None), LIMITS).unwrap();
    assert_eq!(w.total_written(), 0, "a part file with no offer record must not be adopted");
    assert_eq!(fs::metadata(part.join("0.part")).unwrap().len(), 0);
}

#[test]
fn fr_f11_same_offer_resumes_but_different_size_or_hash_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = ReceiveWriter::open(dir.path(), "tr-x", "dev", &decl(20, None), LIMITS).unwrap();
    w.write_chunk(0, 0, b"0123456789", None).unwrap();
    drop(w);

    // Same id, same declaration: resume at 10.
    let w = ReceiveWriter::open(dir.path(), "tr-x", "dev", &decl(20, None), LIMITS).unwrap();
    assert_eq!(w.total_written(), 10);
    drop(w);

    // Same id, different declared size: start at 0.
    let w = ReceiveWriter::open(dir.path(), "tr-x", "dev", &decl(30, None), LIMITS).unwrap();
    assert_eq!(w.total_written(), 0, "a different declared size must not adopt the part file");
    let mut w = w;
    w.write_chunk(0, 0, b"0123456789", None).unwrap();
    drop(w);

    // Same id and size, different hash: start at 0.
    let sha = "ab".repeat(32);
    let w = ReceiveWriter::open(dir.path(), "tr-x", "dev", &decl(30, Some(&sha)), LIMITS).unwrap();
    assert_eq!(w.total_written(), 0, "a different declared hash must not adopt the part file");
}
