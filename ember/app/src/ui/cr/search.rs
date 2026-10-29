//! compose-rust version of `ui::search` (#220 phase 3): search across sessions (FR-L9, FR-S4).
//!
//! The main server's full-text index finds messages in every session, including ones this
//! client never opened (`GET /search`); each hit shows the text around the match and opens its
//! session. Sessions whose title, project, agent, account or folder match are listed after the
//! message hits, from the client's own state.

#![allow(non_snake_case)]

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use compose_rust::runtime::{mutable_state_of, remember};
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{ColorRole, Paint, SpaceRole, TextOverflow, TypeRole};

use ember_client::wire::SearchHit;

use super::components::{EmberField, FieldOptions, Txt};
use super::launcher::{ConnectionBanner, SessionRowView};
use crate::model::{self, SearchResult};
use crate::services::{run_shared, services};
use crate::ui::coexist::shared_ui;

/// Typing pause before the server is asked.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// Message hits asked for per query.
const HIT_LIMIT: usize = 100;

/// The line above the results.
pub fn search_note(query: &str, count: usize, searching: bool) -> String {
    if query.trim().is_empty() {
        "Type to search every conversation's messages, and titles, projects, agents, accounts and folders.".to_string()
    } else if count == 0 && searching {
        "Searching\u{2026}".to_string()
    } else if count == 0 {
        "Nothing matches.".to_string()
    } else {
        format!("{count} result{}", if count == 1 { "" } else { "s" })
    }
}

/// A stable list key: one per message hit, one per session.
pub fn result_key(r: &SearchResult) -> String {
    match r.seq {
        Some(seq) => format!("{}#{seq}", r.row.id),
        None => r.row.id.clone(),
    }
}

/// Whether a newer keystroke has replaced the query this answer is for.
pub fn superseded(current: u64, mine: u64) -> bool {
    current != mine
}

fn muted() -> Option<Paint> {
    Some(Paint::Role(ColorRole::OnSurfaceVariant))
}

pub fn SearchScreen() {
    let ui = shared_ui().clone();
    let query = remember(|| mutable_state_of(std::env::var("EMBER_CR_QUERY").unwrap_or_default()));
    // The hits for `hits_for`; stale answers (an older query) are dropped.
    let hits = remember(|| mutable_state_of(Vec::<SearchHit>::new()));
    let hits_for = remember(|| mutable_state_of(String::new()));
    let error = remember(|| mutable_state_of(None::<String>));
    let generation = remember(|| Arc::new(AtomicU64::new(0)));
    // The demo says once that search is unavailable, not on every keystroke (#202).
    let told = remember(|| mutable_state_of(false));

    let on_change = {
        let (query, hits, hits_for, error, told, generation) =
            (query.clone(), hits.clone(), hits_for.clone(), error.clone(), told.clone(), generation.clone());
        move |v: String| {
            query.set(v.clone());
            let my_gen = generation.fetch_add(1, Ordering::SeqCst) + 1;
            let current = generation.clone();
            let q = v.trim().to_string();
            if !q.is_empty() && services().demo.is_some() {
                if !told.get_untracked() {
                    told.set(true);
                    services().demo_blocked();
                }
                return;
            }
            if q.is_empty() {
                hits.set(Vec::new());
                hits_for.set(String::new());
                error.set(None);
                return;
            }
            let (query, hits, hits_for, error) = (query.clone(), hits.clone(), hits_for.clone(), error.clone());
            run_shared(
                async move {
                    tokio::time::sleep(DEBOUNCE).await;
                    if superseded(current.load(Ordering::SeqCst), my_gen) {
                        return None; // superseded while waiting
                    }
                    Some((q.clone(), services().client.search(&q, Some(HIT_LIMIT)).await))
                },
                move |r| {
                    let Some((q, r)) = r else { return };
                    if q != query.get_untracked().trim() {
                        return;
                    }
                    match r {
                        Ok(list) => {
                            hits.set(list);
                            error.set(None);
                        }
                        Err(e) => {
                            hits.set(Vec::new());
                            error.set(Some(e.to_string()));
                        }
                    }
                    hits_for.set(q);
                },
            );
        }
    };

    let q = query.get();
    let error_text = error.get();
    let searching = !q.trim().is_empty() && hits_for.get() != q.trim();
    let hit_list = hits.get();
    let results: Vec<SearchResult> = ui.with_rows(|s, cx| model::search_results(s, &q, &hit_list, cx));
    let count = results.len();
    let results = Rc::new(results);
    let keys = results.clone();

    cu::Column()
        .space_role(SpaceRole::Md)
        .modifier(Modifier::new().fill_max_width().fill_max_height().padding_role(SpaceRole::Md))
        .content(|| {
            ConnectionBanner();
            EmberField(FieldOptions {
                fill_max_width: true,
                placeholder: "Search conversations".to_string(),
                on_value_change: Some(Box::new(on_change)),
                ..Default::default()
            });
            if let Some(e) = &error_text {
                Txt(
                    &format!("Message search failed: {e}. Showing title matches only."),
                    TypeRole::Caption,
                    Some(Paint::Role(ColorRole::Error)),
                    None,
                    None,
                    None,
                    None,
                );
            }
            let note = search_note(&q, count, searching);
            let role = if count > 0 { TypeRole::Caption } else { TypeRole::Body };
            Txt(&note, role, muted(), None, None, None, None);
            cu::LazyColumn()
                .modifier(Modifier::new().fill_max_width().weight(1.0))
                .content(|scope| {
                    scope.items_keyed(
                        count,
                        move |i| result_key(&keys[i]),
                        move |i| {
                            let r = &results[i];
                            let snippet = r.snippet.clone().unwrap_or_default();
                            cu::Column().space_role(SpaceRole::Xs).modifier(Modifier::new().fill_max_width()).content(|| {
                                Txt(&r.row.project, TypeRole::Caption, muted(), None, None, None, None);
                                SessionRowView(&r.row);
                                if !snippet.is_empty() {
                                    Txt(&snippet, TypeRole::Body, None, Some(3), Some(TextOverflow::Ellipsis), None, None);
                                }
                            });
                        },
                    );
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use compose_rust::protocol::{decode_batch, Mutation, PropertyValue};
    use compose_rust::{Host, Recomposer};

    fn row(id: &str) -> crate::model::SessionRow {
        crate::model::SessionRow {
            id: id.into(),
            project: "p".into(),
            title: "t".into(),
            agent: "a".into(),
            model: None,
            account: None,
            cwd: String::new(),
            updated_at: 0,
            preview: None,
            status: ember_client::LauncherStatus::Running,
            pinned: false,
            archived: false,
        }
    }

    #[test]
    fn fr_l9_search_note_says_what_the_list_is_doing() {
        assert!(search_note("", 0, false).starts_with("Type to search"));
        assert_eq!(search_note("  ", 3, false).chars().next(), Some('T'));
        assert_eq!(search_note("a", 0, true), "Searching\u{2026}");
        assert_eq!(search_note("a", 0, false), "Nothing matches.");
        assert_eq!(search_note("a", 1, false), "1 result");
        assert_eq!(search_note("a", 2, false), "2 results");
    }

    #[test]
    fn fr_s4_message_hits_and_session_matches_get_distinct_keys() {
        let hit = SearchResult { row: row("s1"), seq: Some(7), snippet: None };
        let session = SearchResult { row: row("s1"), seq: None, snippet: None };
        assert_eq!(result_key(&hit), "s1#7");
        assert_eq!(result_key(&session), "s1");
    }

    #[test]
    fn fr_l9_an_answer_for_an_older_keystroke_is_superseded() {
        assert!(superseded(3, 2));
        assert!(!superseded(3, 3));
    }

    #[test]
    fn fr_l9_search_screen_shows_the_field_and_the_hint() {
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(SearchScreen)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        let has = |want: &str| {
            batch.iter().any(|m| {
                matches!(m, Mutation::SetProp { value: PropertyValue::String(t), .. } if t.starts_with(want))
            })
        };
        assert!(has("Search conversations"));
        assert!(has("Type to search"));
    }
}
