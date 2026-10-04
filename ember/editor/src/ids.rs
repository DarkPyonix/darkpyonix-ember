//! Stable decoration ids.
//!
//! The widget reports clicks, hovers and ghost-text acceptance by the `u64` id the session put on a
//! decoration (FR-38 §38.6). Ids must therefore be:
//!
//! * **non-zero** (0 means "no decoration");
//! * **unique in the session**, so an event can be routed by id alone;
//! * **stable across recomputation**: the same diagnostic, re-published by the language server
//!   after an unrelated edit, keeps its id; a CodeLens keeps its id when it is resolved (only its
//!   title changes); so a click on a decoration that the session has just recomputed still resolves.
//!
//! Each bridge builds a *content key* for what it shows (not the range, which moves with edits) and
//! asks [`DecorationIds::id`] for it. The id encodes the bridge in its top byte ([`IdSpace`]) so an
//! activation can be dispatched without a lookup.

use std::collections::HashMap;

use crate::widget::DecorationId;

/// Which bridge owns an id (top 8 bits of the id).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum IdSpace {
    Diagnostic = 1,
    CodeLens = 2,
    HoverAnchor = 3,
    GhostText = 4,
}

impl IdSpace {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::Diagnostic,
            2 => Self::CodeLens,
            3 => Self::HoverAnchor,
            4 => Self::GhostText,
            _ => return None,
        })
    }
}

const SPACE_SHIFT: u32 = 56;
const COUNTER_MASK: u64 = (1 << SPACE_SHIFT) - 1;

/// Allocates ids by `(space, document, key)`.
#[derive(Debug, Default)]
pub struct DecorationIds {
    next: u64,
    by_key: HashMap<(IdSpace, String, String), u64>,
}

impl DecorationIds {
    pub fn new() -> Self {
        Self::default()
    }

    /// The id for `key` of document `doc` in `space`: the same one every time it is asked for,
    /// until [`Self::forget_document`].
    pub fn id(&mut self, space: IdSpace, doc: &str, key: &str) -> DecorationId {
        let k = (space, doc.to_owned(), key.to_owned());
        if let Some(&id) = self.by_key.get(&k) {
            return DecorationId(id);
        }
        self.next += 1;
        let id = ((space as u64) << SPACE_SHIFT) | (self.next & COUNTER_MASK);
        self.by_key.insert(k, id);
        DecorationId(id)
    }

    /// Which bridge an id belongs to (from its top byte).
    pub fn space_of(id: u64) -> Option<IdSpace> {
        if id == 0 {
            return None;
        }
        IdSpace::from_u8((id >> SPACE_SHIFT) as u8)
    }

    /// Drop every key of a document (it was closed or its content replaced). Ids are never
    /// reissued: the counter keeps going, so a stale id from the old content cannot match a new
    /// decoration.
    pub fn forget_document(&mut self, doc: &str) {
        self.by_key.retain(|(_, d, _), _| d.as_str() != doc);
    }

    /// Drop the keys of one space in one document that are not in `live` (called by a bridge after
    /// it recomputed its full set), so the map does not grow without bound over a long session.
    pub fn retain(&mut self, space: IdSpace, doc: &str, live: &std::collections::HashSet<u64>) {
        self.by_key.retain(|(s, d, _), id| *s != space || d.as_str() != doc || live.contains(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_key_same_id_different_key_different_id() {
        let mut ids = DecorationIds::new();
        let a = ids.id(IdSpace::Diagnostic, "d", "rustc|8|mismatched types|0");
        let b = ids.id(IdSpace::Diagnostic, "d", "rustc|8|unused|0");
        assert_ne!(a, b);
        assert_eq!(ids.id(IdSpace::Diagnostic, "d", "rustc|8|mismatched types|0"), a);
        // Same key in another document or space is another decoration.
        assert_ne!(ids.id(IdSpace::Diagnostic, "e", "rustc|8|mismatched types|0"), a);
        assert_ne!(ids.id(IdSpace::CodeLens, "d", "rustc|8|mismatched types|0"), a);
    }

    #[test]
    fn ids_are_nonzero_and_carry_their_space() {
        let mut ids = DecorationIds::new();
        for space in [IdSpace::Diagnostic, IdSpace::CodeLens, IdSpace::HoverAnchor, IdSpace::GhostText] {
            let id = ids.id(space, "d", "k").get();
            assert_ne!(id, 0);
            assert_eq!(DecorationIds::space_of(id), Some(space));
        }
        assert_eq!(DecorationIds::space_of(0), None);
        assert_eq!(DecorationIds::space_of(42), None);
    }

    #[test]
    fn forgotten_keys_get_fresh_ids_never_reused_ones() {
        let mut ids = DecorationIds::new();
        let a = ids.id(IdSpace::GhostText, "d", "k");
        ids.forget_document("d");
        let b = ids.id(IdSpace::GhostText, "d", "k");
        assert_ne!(a, b);

        let keep = ids.id(IdSpace::CodeLens, "d", "keep");
        let drop = ids.id(IdSpace::CodeLens, "d", "drop");
        let live: std::collections::HashSet<u64> = [keep.get()].into_iter().collect();
        ids.retain(IdSpace::CodeLens, "d", &live);
        assert_eq!(ids.id(IdSpace::CodeLens, "d", "keep"), keep);
        assert_ne!(ids.id(IdSpace::CodeLens, "d", "drop"), drop);
        // Other spaces untouched by retain.
        assert_eq!(ids.id(IdSpace::GhostText, "d", "k"), b);
    }
}
