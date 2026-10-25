//! Search across sessions (FR-L9, FR-S4).
//!
//! The main server's full-text index finds messages in every session, including ones this
//! client never opened (`GET /search`); each hit shows the text around the match and opens its
//! session. Sessions whose title, project, agent, account or folder match are listed after the
//! message hits, from the client's own state.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dioxus_compose::prelude::*;

use ember_client::wire::SearchHit;

use crate::model::{self, SearchResult};
use crate::services::{run, services};
use crate::ui::launcher::{ConnectionBanner, SessionRowView};
use crate::ui::use_ui;

/// Typing pause before the server is asked.
const DEBOUNCE: Duration = Duration::from_millis(250);
/// Message hits asked for per query.
const HIT_LIMIT: usize = 100;

#[component]
pub fn SearchScreen() -> Element {
    let ui = use_ui();
    let mut query = use_signal(String::new);
    // The hits for `hits_for`; stale answers (an older query) are dropped.
    let mut hits = use_signal(Vec::<SearchHit>::new);
    let mut hits_for = use_signal(String::new);
    let mut error = use_signal(|| None::<String>);
    let generation = use_hook(|| Arc::new(AtomicU64::new(0)));

    let on_change = move |v: String| {
        query.set(v.clone());
        let my_gen = generation.fetch_add(1, Ordering::SeqCst) + 1;
        let current = generation.clone();
        let q = v.trim().to_string();
        if q.is_empty() {
            hits.set(Vec::new());
            hits_for.set(String::new());
            error.set(None);
            return;
        }
        run(
            async move {
                tokio::time::sleep(DEBOUNCE).await;
                if current.load(Ordering::SeqCst) != my_gen {
                    return None; // superseded while waiting
                }
                Some((q.clone(), services().client.search(&q, Some(HIT_LIMIT)).await))
            },
            move |r| {
                let Some((q, r)) = r else { return };
                if q != query.peek().trim() {
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
    };

    let q = query();
    let error_text = error();
    let searching = !q.trim().is_empty() && hits_for() != q.trim();
    let results: Vec<SearchResult> = {
        let hits = hits.read();
        ui.with_rows(|s, cx| model::search_results(s, &q, &hits, cx))
    };
    let count = results.len();
    let keys: Vec<String> = results
        .iter()
        .map(|r| match r.seq {
            Some(seq) => format!("{}#{seq}", r.row.id),
            None => r.row.id.clone(),
        })
        .collect();
    rsx! {
        Column {
            fill_max_width: true,
            fill_max_height: true,
            padding_role: SpaceRole::Md,
            space_role: SpaceRole::Md,
            ConnectionBanner {}
            TextField {
                fill_max_width: true,
                placeholder: "Search conversations",
                on_value_change: on_change,
            }
            if error_text.is_some() {
                Text {
                    text: format!("Message search failed: {}. Showing title matches only.", error_text.clone().unwrap_or_default()),
                    type_role: TypeRole::Caption,
                    color: Paint::Role(ColorRole::Error),
                }
            }
            if q.trim().is_empty() {
                Text {
                    text: "Type to search every conversation's messages, and titles, projects, agents, accounts and folders.",
                    type_role: TypeRole::Body,
                    color: Paint::Role(ColorRole::OnSurfaceVariant),
                }
            } else if count == 0 && searching {
                Text {
                    text: "Searching\u{2026}",
                    type_role: TypeRole::Body,
                    color: Paint::Role(ColorRole::OnSurfaceVariant),
                }
            } else if count == 0 {
                Text {
                    text: "Nothing matches.",
                    type_role: TypeRole::Body,
                    color: Paint::Role(ColorRole::OnSurfaceVariant),
                }
            } else {
                Text {
                    text: format!("{count} result{}", if count == 1 { "" } else { "s" }),
                    type_role: TypeRole::Caption,
                    color: Paint::Role(ColorRole::OnSurfaceVariant),
                }
            }
            LazyColumn {
                fill_max_width: true,
                weight: 1.0,
                item_count: count,
                key_of: move |i: usize| keys[i].clone(),
                item: move |i: usize| {
                    let r = results[i].clone();
                    let snippet = r.snippet.clone().unwrap_or_default();
                    rsx! {
                        Column {
                            fill_max_width: true,
                            space_role: SpaceRole::Xs,
                            Text {
                                text: r.row.project.clone(),
                                type_role: TypeRole::Caption,
                                color: Paint::Role(ColorRole::OnSurfaceVariant),
                            }
                            SessionRowView { row: r.row.clone() }
                            if !snippet.is_empty() {
                                Text {
                                    text: snippet,
                                    type_role: TypeRole::Body,
                                    max_lines: 3,
                                    overflow: TextOverflow::Ellipsis,
                                }
                            }
                        }
                    }
                },
            }
        }
    }
}
