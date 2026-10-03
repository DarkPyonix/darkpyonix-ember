//! Search across sessions (FR-L9).
//!
//! Matches titles (including local renames), projects, agents, accounts, folders and the text of
//! every transcript this client holds. TODO(server FR-S4): full-text search on the main server
//! across transcripts this client has never opened.

use dioxus_compose::prelude::*;

use crate::model::{self, SessionRow};
use crate::ui::launcher::{ConnectionBanner, SessionRowView};
use crate::ui::use_ui;

#[component]
pub fn SearchScreen() -> Element {
    let ui = use_ui();
    let mut query = use_signal(String::new);
    let q = query();
    let rows: Vec<SessionRow> = ui.with_rows(|s, cx| model::search(s, &q, cx));
    let count = rows.len();
    let keys: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
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
                on_value_change: move |v: String| query.set(v),
            }
            if q.trim().is_empty() {
                Text {
                    text: "Type to search titles, projects, agents, accounts and conversation text.",
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
                    let row = rows[i].clone();
                    rsx! {
                        Column {
                            fill_max_width: true,
                            space_role: SpaceRole::Xs,
                            Text {
                                text: row.project.clone(),
                                type_role: TypeRole::Caption,
                                color: Paint::Role(ColorRole::OnSurfaceVariant),
                            }
                            SessionRowView { row }
                        }
                    }
                },
            }
        }
    }
}
