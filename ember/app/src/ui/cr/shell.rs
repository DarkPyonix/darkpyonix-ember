//! compose-rust version of `ui::shell` (#220 phase 2): the sidebar, the rail, the floating bottom
//! tabs and the navigation between menus (FR-L10, FR-L28, `docs/DESIGN.md` section 4.7).
//!
//! The decisions (menu order and labels, the screen a menu shows, which bottom tab a menu
//! belongs to, which layout a window size gets, the badges) are the plain functions and enums of
//! `ui::shell`, re-exported here and tested there; this file draws them. They move here when
//! the Dioxus shell is deleted (phase 8).
//!
//! Content: the screens are still on dioxus-compose, so the content area shows a placeholder
//! per menu until each screen lands in its phase.
//!
//! TODO(#213, #204): the macOS full-size title bar and safe area, and the Windows caption
//! buttons' 138 dp, wait for the toolkit's caption inset (compose-rust and dioxus-compose issues
//! of the same name). The Windows strip below reserves the width meanwhile, as the Dioxus shell
//! does.

#![allow(non_snake_case)]

use std::cell::RefCell;
use std::collections::HashMap;

use compose_rust::runtime::{composable, mutable_state_of, remember};
use compose_rust::schema::Window;
use compose_rust::ui as cu;
use compose_rust::ui::Modifier;
use compose_rust::{
    Alignment, AssetKind, ColorScheme, DesignSystem, Paint, SpaceRole, Theme,
};

pub use crate::ui::shell::{
    active_menu, avatar_initial, badge_text, caption_reserved_width, nav_badge, nav_mode,
    screen_for, tab_badge, tab_for_menu, toggled_collapse, tinted_svg, BottomTab, NavBadge,
    NavCounts, NavMenu, Screen, ShellMode, BOTTOM_TABS, DEMO_USER, ICON_GEAR_SVG, ICON_MOON_SVG,
    ICON_MORE_SVG, ICON_SUN_SVG, MORE_MENUS, ORG_NAME, PLACEHOLDER_TEXT, WINDOWS_CAPTION_HEIGHT_DP,
};
use super::components::{Clickable, EmberSheet, Label};
use super::theme::*;
pub const ICON_SEARCH_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"24\" height=\"24\" fill=\"none\" viewBox=\"0 0 24 24\" stroke-width=\"1.5\" stroke=\"currentColor\"><path stroke-linecap=\"round\" stroke-linejoin=\"round\" d=\"m21 21-5.197-5.197m0 0A7.5 7.5 0 1 0 5.196 5.196a7.5 7.5 0 0 0 10.607 10.607Z\"/></svg>";
pub const ICON_MENU_SVG: &str = include_str!("../../../assets/nav/menu.svg");
pub const ICON_HASH_SVG: &str = include_str!("../../../assets/nav/hash.svg");
pub const ICON_USERS_SVG: &str = include_str!("../../../assets/nav/users.svg");
pub const ICON_SMILE_SVG: &str = include_str!("../../../assets/nav/smile.svg");
pub const ICON_PLUS_SVG: &str = include_str!("../../../assets/nav/plus.svg");
pub const ICON_CPU_SVG: &str = include_str!("../../../assets/nav/cpu.svg");
pub const ICON_GPU_SVG: &str = include_str!("../../../assets/nav/gpu.svg");
pub const ICON_MEMORY_SVG: &str = include_str!("../../../assets/nav/memory.svg");
pub const ICON_THERMO_SVG: &str = include_str!("../../../assets/nav/thermo.svg");
pub const ICON_DISK_SVG: &str = include_str!("../../../assets/nav/disk.svg");
pub const ICON_NETWORK_SVG: &str = include_str!("../../../assets/nav/network.svg");
pub const ICON_FAN_SVG: &str = include_str!("../../../assets/nav/fan.svg");
pub const ICON_GAUGE_SVG: &str = include_str!("../../../assets/nav/gauge.svg");
pub const ICON_CLOSE_SVG: &str = include_str!("../../../assets/nav/close.svg");
pub const ICON_INFO_SVG: &str = include_str!("../../../assets/nav/info.svg");
pub const ICON_CHEVRON_DOWN_SVG: &str = include_str!("../../../assets/nav/chevron-down.svg");

use crate::model;
use crate::ui::coexist::{shared_ui, SharedUi};
use crate::ui::Route;

fn transparent() -> Paint {
    Paint::Literal(compose_rust::Color::argb(0))
}

// ---------------------------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------------------------

/// Go to a menu. Projects and a conversation are real screens; the rest only change the menu.
pub fn choose(ui: &SharedUi, menu: NavMenu) {
    let in_conversation = matches!(ui.route.get_untracked(), Route::Conversation(_));
    match menu {
        NavMenu::Projects => ui.go(Route::Projects),
        NavMenu::Sessions => {
            if !in_conversation {
                let recent = ui.with_rows_no_previews(|s, cx| model::recent_sessions(s, cx, 1));
                if let Some(row) = recent.first() {
                    ui.open_conversation(&row.id);
                }
            }
        }
        _ => {
            if in_conversation {
                ui.go(Route::Projects);
            }
        }
    }
    ui.detail_open.set(false);
    ui.set_menu(menu);
}

// ---------------------------------------------------------------------------------------------
// Pieces
// ---------------------------------------------------------------------------------------------

thread_local! {
    /// Tinted icon documents by (icon, size, colour). The toolkit takes `'static` bytes, so each
    /// distinct icon is leaked once and then reused.
    static ICONS: RefCell<HashMap<(usize, i32, u32), &'static [u8]>> = RefCell::new(HashMap::new());
}

/// A generated document (a ring, a mark) as `'static` bytes, leaked once per distinct text.
pub fn leak_doc(doc: String) -> &'static [u8] {
    thread_local! {
        static DOCS: RefCell<HashMap<String, &'static [u8]>> = RefCell::new(HashMap::new());
    }
    DOCS.with(|cache| {
        *cache
            .borrow_mut()
            .entry(doc.clone())
            .or_insert_with(|| Box::leak(doc.into_bytes().into_boxed_slice()))
    })
}

fn icon_bytes(svg: &'static str, size: f32, argb: u32) -> &'static [u8] {
    ICONS.with(|cache| {
        *cache
            .borrow_mut()
            .entry((svg.as_ptr() as usize, size.round() as i32, argb))
            .or_insert_with(|| {
                Box::leak(tinted_svg(svg, size, argb).into_bytes().into_boxed_slice())
            })
    })
}

/// A stroke icon from the app's own SVG set, tinted to `argb` (the toolkit's `IconRole` set has
/// no such glyphs).
pub fn NavIcon(svg: &'static str, argb: u32, size: f32) {
    let id = compose_rust::asset(AssetKind::Svg, icon_bytes(svg, size, argb));
    cu::Image(id).modifier(Modifier::new().size(size, size));
}

/// The 18 px red count badge (`.redbadge`): white 11 px number on `red`.
pub fn RedBadge(n: usize) {
    let p = ember_theme().palette;
    let text = badge_text(n);
    let narrow = text.chars().count() <= 1;
    let width = if narrow { Some(18.0) } else { None };
    cu::Box()
        .modifier(
            Modifier::new()
                .width_if(width)
                .height(18.0)
                .background(paint(p.red.strong))
                .corner_radius(9.0),
        )
        .alignment(Alignment::Center)
        .content(|| {
            cu::Row().alignment(Alignment::Center).content(|| {
                if !narrow {
                    cu::Spacer().modifier(Modifier::new().width(5.0));
                }
                Label(&text, Fs::Xs2, Weight::Semibold, Some(paint(compose_rust::Color::rgb(0xffffff))), None);
                if !narrow {
                    cu::Spacer().modifier(Modifier::new().width(5.0));
                }
            });
        });
}

/// One sidebar row: 38 px pill, icon, label, then a muted count or a red badge at the right.
pub fn NavRow(menu: NavMenu, active: bool, badge: NavBadge, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let fg = if active { p.ink_text } else { p.text_2 };
    let bg = if active { paint(p.ink) } else { transparent() };
    Clickable(
        Modifier::new()
            .fill_max_width()
            .height(H_NAV_ITEM)
            .background(bg)
            .corner_radius(H_NAV_ITEM / 2.0),
        Alignment::CenterStart,
        on_click,
        || {
            cu::Row().alignment(Alignment::CenterStart).modifier(Modifier::new().fill_max_width()).content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
                NavIcon(menu.svg(), fg.to_argb(), 18.0);
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
                Label(menu.label(), Fs::Md, Weight::Medium, Some(paint(fg)), Some(1));
                cu::Spacer().modifier(Modifier::new().weight(1.0));
                match badge {
                    NavBadge::None => {}
                    NavBadge::Count(n) => {
                        let ink = if active { p.ink_text } else { p.muted };
                        Label(&n.to_string(), Fs::Xs, Weight::Regular, Some(paint(ink)), None);
                    }
                    NavBadge::Alert(n) => RedBadge(n),
                }
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
            });
        },
    );
}

/// One rail item: a 44 px rounded square with the icon centred and the red badge at its top
/// right corner.
pub fn RailItem(menu: NavMenu, active: bool, badge: NavBadge, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let fg = if active { p.ink_text } else { p.text_2 };
    let bg = if active { paint(p.ink) } else { transparent() };
    Clickable(
        Modifier::new().size(44.0, 44.0).background(bg).corner_radius(12.0),
        Alignment::Center,
        on_click,
        || {
            NavIcon(menu.svg(), fg.to_argb(), 20.0);
            if let NavBadge::Alert(n) = badge {
                cu::Box()
                    .modifier(Modifier::new().fill_max_width().fill_max_height())
                    .alignment(Alignment::TopEnd)
                    .content(|| {
                        // `.nav .redbadge`: top 2, right 0.
                        cu::Column().content(|| {
                            cu::Spacer().modifier(Modifier::new().height(2.0));
                            RedBadge(n);
                        });
                    });
            }
        },
    );
}

/// A footer tool row (theme, settings): icon and label in the sidebar, icon alone in the rail.
/// `.tool`: 34 px high (44 x 40 in the rail), 12 px side padding and gap, 13 px medium `--muted`.
pub fn ToolRow(svg: &'static str, label: &str, rail: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    if rail {
        Clickable(
            Modifier::new().size(44.0, 40.0).corner_radius(CORNER_SM),
            Alignment::Center,
            on_click,
            || NavIcon(svg, p.muted.to_argb(), 20.0),
        );
        return;
    }
    Clickable(
        Modifier::new().fill_max_width().height(34.0).corner_radius(CORNER_SM),
        Alignment::CenterStart,
        on_click,
        || {
            cu::Row().alignment(Alignment::CenterStart).content(|| {
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
                NavIcon(svg, p.muted.to_argb(), 18.0);
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
                Label(label, Fs::Sm, Weight::Medium, Some(paint(p.muted)), Some(1));
            });
        },
    );
}

/// The brand block: mark, "Ember" 15 px semibold and the org line 12 px muted. A click folds
/// or unfolds the sidebar (the mockup's logo button).
pub fn Brand(compact: bool, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    let size = 32.0;
    let mark = crate::ui::shell::mark_svg_for(size).as_bytes();
    let m = Modifier::new().fill_max_width_if(!compact);
    Clickable(m, if compact { Alignment::Center } else { Alignment::CenterStart }, on_click, || {
        // A Box centers both children on the cross axis: the logo against the two-line text.
        cu::Box().alignment(Alignment::CenterStart).content(|| {
            if !compact {
                cu::Row().alignment(Alignment::CenterStart).content(|| {
                    // 6 side padding, the 32 px mark, then the 10 px gap.
                    cu::Spacer().modifier(Modifier::new().width(6.0 + size + 10.0));
                    cu::Column().spacing(6.0).content(|| {
                        Label("Ember", Fs::Lg, Weight::Semibold, None, None);
                        Label(ORG_NAME, Fs::Xs, Weight::Regular, Some(paint(p.muted)), None);
                    });
                });
            }
            cu::Row().content(|| {
                if !compact {
                    cu::Spacer().modifier(Modifier::new().width(6.0));
                }
                cu::Image(compose_rust::asset(AssetKind::Svg, mark))
                    .modifier(Modifier::new().size(size, size));
            });
        });
    });
}

/// The user row at the foot of the sidebar: blue round avatar, name, handle. In the rail only
/// the avatar stays (`.me div { display: none }`). `.me`: 12 8 padding, 12 gap.
pub fn UserRow(rail: bool) {
    let p = ember_theme().palette;
    let (name, handle) = DEMO_USER;
    cu::Row()
        .alignment(Alignment::CenterStart)
        .modifier(Modifier::new().fill_max_width())
        .content(|| {
            if rail {
                cu::Spacer().modifier(Modifier::new().weight(1.0));
            } else {
                cu::Spacer().modifier(Modifier::new().width(SP[1]));
            }
            cu::Box()
                .modifier(
                    Modifier::new()
                        .size(28.0, 28.0)
                        .background(paint(p.blue.strong))
                        .corner_radius(14.0),
                )
                .alignment(Alignment::Center)
                .content(|| {
                    Label(
                        &avatar_initial(name),
                        Fs::Sm,
                        Weight::Semibold,
                        Some(paint(compose_rust::Color::rgb(0xffffff))),
                        None,
                    );
                });
            if rail {
                cu::Spacer().modifier(Modifier::new().weight(1.0));
            } else {
                cu::Spacer().modifier(Modifier::new().width(SP[2]));
                cu::Column().spacing(4.0).content(|| {
                    Label(name, Fs::Sm, Weight::Semibold, None, Some(1));
                    Label(handle, Fs::Xs, Weight::Regular, Some(paint(p.muted)), Some(1));
                });
            }
        });
}

/// Whether the demo-only sidebar/phone-menu rows (`플랫폼: 기타`, `데모 초기화`) are shown.
pub fn show_demo_tools() -> bool {
    crate::demo::active_from_env()
}

/// The sidebar, or the rail when `rail`. `.side`: 20 12 padding (16 0 in the rail), 2 gap.
pub fn Sidebar(rail: bool, active: NavMenu, counts: NavCounts, on_collapse: impl FnMut() + 'static, on_settings: impl FnMut() + 'static) {
    let ui = shared_ui().clone();
    let theme = ember_theme();
    let p = theme.palette;
    let pref = ui.theme_pref.get();
    let width = if rail { RAIL_W } else { SIDE_W };
    let theme_svg = if theme.scheme == Scheme::Dark { ICON_MOON_SVG } else { ICON_SUN_SVG };
    let side = if rail { 0.0 } else { SP[2] };
    // Spacers include the column's 2 px gap on either side.
    let top = if rail { 14.0 } else { SP[4] };
    // macOS: the traffic lights float over the sidebar's top-left, so the first row starts below them.
    let top = top + crate::ui::safe_area::sidebar_top_inset(crate::ui::safe_area::safe_area());
    let below_brand = if rail { 10.0 } else { 20.0 };
    let bottom = if rail { 26.0 } else { 30.0 };
    let demo_tools = show_demo_tools();
    let plat_ios = remember(|| mutable_state_of(false));
    let plat_label = if plat_ios.get() { "플랫폼: iOS" } else { "플랫폼: 기타" };
    cu::Row().modifier(Modifier::new().fill_max_height()).content(|| {
        cu::Column()
            .modifier(Modifier::new().width(width - 1.0).fill_max_height().background(paint(p.bg)))
            .content(|| {
                cu::Row().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
                    cu::Spacer().modifier(Modifier::new().width(side));
                    cu::Column()
                        .spacing(2.0)
                        .alignment(if rail { Alignment::TopCenter } else { Alignment::TopStart })
                        .modifier(Modifier::new().weight(1.0).fill_max_height())
                        .content(|| {
                            cu::Spacer().modifier(Modifier::new().height(top));
                            Brand(rail, on_collapse);
                            cu::Spacer().modifier(Modifier::new().height(below_brand));
                            for menu in NavMenu::ALL {
                                let u = ui.clone();
                                let badge = nav_badge(menu, &counts);
                                if rail {
                                    RailItem(menu, menu == active, badge, move || choose(&u, menu));
                                } else {
                                    NavRow(menu, menu == active, badge, move || choose(&u, menu));
                                }
                            }
                            cu::Spacer().modifier(Modifier::new().weight(1.0));
                            // `.sidetools`: 8 margin, a 1 px `--line` border, 8 padding.
                            cu::Spacer().modifier(Modifier::new().height(SP[1]));
                            // In the rail the border spans the 44 px tool width only.
                            let divider = Modifier::new().height(1.0).background(paint(p.line));
                            cu::Spacer().modifier(if rail { divider.width(44.0) } else { divider.fill_max_width() });
                            cu::Spacer().modifier(Modifier::new().height(4.0));
                            let u = ui.clone();
                            ToolRow(theme_svg, &format!("테마: {}", pref.label()), rail, move || {
                                u.set_theme_pref(u.theme_pref.get_untracked().next());
                            });
                            let us = ui.clone();
                            ToolRow(ICON_SEARCH_SVG, "검색", rail, move || us.go(Route::Search));
                            ToolRow(ICON_GEAR_SVG, "설정", rail, on_settings);
                            if demo_tools {
                                let pl = plat_ios.clone();
                                ToolRow(ICON_CPU_SVG, plat_label, rail, move || {
                                    pl.set(!pl.get_untracked());
                                });
                                ToolRow(NavMenu::Routines.svg(), "데모 초기화", rail, || {
                                    if let Some(s) = crate::services::try_services() {
                                        if s.reset_demo() {
                                            super::notice::post("데모를 초기 상태로 되돌렸습니다");
                                        }
                                    }
                                });
                            }
                            // `.me`: 8 margin and 12 padding above the avatar.
                            cu::Spacer().modifier(Modifier::new().height(16.0));
                            UserRow(rail);
                            cu::Spacer().modifier(Modifier::new().height(bottom));
                        });
                    cu::Spacer().modifier(Modifier::new().width(side));
                });
            });
        cu::Spacer().modifier(Modifier::new().width(1.0).fill_max_height().background(paint(p.line)));
    });
}

/// The floating bottom bar: five equal-width tabs, icon above an 11 px label, red badges.
/// TODO(compose-rust#300): backdrop blur (Liquid Glass on iOS) and the safe-area inset; only the
/// 10 px gap is added.
pub fn BottomBar(active: NavMenu, counts: NavCounts, on_choose: impl FnMut(BottomTab) + Clone + 'static) {
    let theme = ember_theme();
    let p = theme.palette;
    let marked = tab_for_menu(active);
    cu::Column().modifier(Modifier::new().fill_max_width()).content(|| {
        cu::Row().modifier(Modifier::new().fill_max_width()).content(|| {
            cu::Spacer().modifier(Modifier::new().width(TABBAR_SIDE_GAP));
            cu::Box()
                .modifier(
                    with_shadows(Modifier::new(), theme.shadow_md())
                        .weight(1.0)
                        .height(TABBAR_H)
                        .background(paint(p.surface))
                        .border(1.0, paint(p.line))
                        .corner_radius(TABBAR_RADIUS),
                )
                .alignment(Alignment::Center)
                .content(|| {
                    cu::Row().alignment(Alignment::Center).modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
                        // `.side` padding 6 4: the tabs sit 4 px inside the border.
                        cu::Spacer().modifier(Modifier::new().width(SP[0]));
                        for tab in BOTTOM_TABS {
                            let mut choose_tab = on_choose.clone();
                            let ink = if tab == marked { p.text } else { p.muted };
                            // The selected tab sits on a `--sel` pill, 52 px high.
                            let bg = if tab == marked { paint(p.sel) } else { transparent() };
                            Clickable(
                                Modifier::new().weight(1.0).height(52.0),
                                Alignment::Center,
                                move || choose_tab(tab),
                                || {
                                  // The pill is the tab's content width, not the whole cell.
                                  cu::Box()
                                    .modifier(Modifier::new().width(70.0).height(52.0).background(bg).corner_radius(20.0))
                                    .alignment(Alignment::Center)
                                    .content(|| {
                                    cu::Box().modifier(Modifier::new().fill_max_width().fill_max_height()).alignment(Alignment::Center).content(|| {
                                        cu::Column().alignment(Alignment::TopCenter).spacing(2.0).content(|| {
                                            NavIcon(tab.svg(), ink.to_argb(), 18.0);
                                            Label(tab.label(), Fs::Xs2, Weight::Medium, Some(paint(ink)), Some(1));
                                        });
                                        // `.nav.tab .redbadge`: top 0, right 4, on the tab itself.
                                        if let Some(n) = tab_badge(tab, &counts) {
                                            cu::Box()
                                                .modifier(Modifier::new().fill_max_width().fill_max_height())
                                                .alignment(Alignment::TopEnd)
                                                .content(|| {
                                                    cu::Row().content(|| {
                                                        RedBadge(n);
                                                        cu::Spacer().modifier(Modifier::new().width(SP[0]));
                                                    });
                                                });
                                        }
                                    });
                                    });
                                },
                            );
                        }
                        cu::Spacer().modifier(Modifier::new().width(SP[0]));
                    });
                });
            cu::Spacer().modifier(Modifier::new().width(TABBAR_SIDE_GAP));
        });
        cu::Spacer().modifier(Modifier::new().height(TABBAR_BOTTOM_GAP));
    });
}

/// The phone's top-right tool menu (`.phonemenu`): a 32 px round button at top 10 / right 12 that
/// opens a popover with the theme switch, settings, and (in demo mode) platform toggle and demo reset.
pub fn PhoneMenu(open: compose_rust::runtime::MutableState<bool>, on_settings: impl FnMut() + Clone + 'static) {
    let theme = ember_theme();
    let p = theme.palette;
    let ui = shared_ui().clone();
    let theme_svg = if ember_theme().scheme == Scheme::Dark { ICON_MOON_SVG } else { ICON_SUN_SVG };
    let demo_tools = show_demo_tools();
    let plat_ios = remember(|| mutable_state_of(false));
    let plat_label = if plat_ios.get() { "플랫폼: iOS" } else { "플랫폼: 기타" };
    cu::Row().modifier(Modifier::new().fill_max_width()).alignment(Alignment::TopStart).content(|| {
        cu::Spacer().modifier(Modifier::new().weight(1.0));
        cu::Column().alignment(Alignment::TopEnd).content(|| {
            cu::Spacer().modifier(Modifier::new().height(10.0));
            let o = open.clone();
            Clickable(
                with_shadows(Modifier::new(), theme.shadow_sm())
                    .size(H_CTL, H_CTL)
                    .background(paint(p.surface))
                    .border(1.0, paint(p.line))
                    .shape_role(compose_rust::ShapeRole::Full),
                Alignment::Center,
                move || o.set(!o.get_untracked()),
                || NavIcon(ICON_MENU_SVG, p.text_2.to_argb(), 16.0),
            );
            if open.get() {
                cu::Spacer().modifier(Modifier::new().height(4.0));
                cu::Box()
                    .modifier(
                        with_shadows(Modifier::new(), theme.shadow_lg())
                            .width(168.0)
                            .background(paint(p.surface))
                            .border(1.0, paint(p.line))
                            .corner_radius(CORNER_MD)
                            .padding(4.0),
                    )
                    .content(|| {
                        cu::Column().content(|| {
                            let (u, o) = (ui.clone(), open.clone());
                            menu_item(theme_svg, "테마 전환", move || {
                                u.set_theme_pref(u.theme_pref.get_untracked().next());
                                o.set(false);
                            });
                            let (u, o) = (ui.clone(), open.clone());
                            menu_item(ICON_SEARCH_SVG, "검색", move || {
                                o.set(false);
                                u.go(Route::Search);
                            });
                            let (o, mut s) = (open.clone(), on_settings.clone());
                            menu_item(ICON_GEAR_SVG, "설정", move || {
                                o.set(false);
                                s();
                            });
                            if demo_tools {
                                let (pl, o) = (plat_ios.clone(), open.clone());
                                menu_item(ICON_CPU_SVG, plat_label, move || {
                                    pl.set(!pl.get_untracked());
                                    o.set(false);
                                });
                                let o = open.clone();
                                menu_item(NavMenu::Routines.svg(), "데모 초기화", move || {
                                    o.set(false);
                                    if let Some(s) = crate::services::try_services() {
                                        if s.reset_demo() {
                                            super::notice::post("데모를 초기 상태로 되돌렸습니다");
                                        }
                                    }
                                });
                            }
                        });
                    });
            }
        });
        cu::Spacer().modifier(Modifier::new().width(SP[2]));
    });
}

/// `.pmenu-pop button`: 38 px high, 10 px side padding, 8 px gap.
fn menu_item(svg: &'static str, label: &str, on_click: impl FnMut() + 'static) {
    let p = ember_theme().palette;
    Clickable(
        Modifier::new().fill_max_width().height(38.0).corner_radius(CORNER_SM),
        Alignment::CenterStart,
        on_click,
        || {
            cu::Row().alignment(Alignment::CenterStart).content(|| {
                cu::Spacer().modifier(Modifier::new().width(10.0));
                NavIcon(svg, p.text.to_argb(), 18.0);
                cu::Spacer().modifier(Modifier::new().width(SP[0] * 2.0));
                Label(label, Fs::Sm, Weight::Regular, Some(paint(p.text)), Some(1));
            });
        },
    );
}

/// A menu whose screen is not built yet ("준비 중입니다").
pub fn Placeholder(menu: NavMenu) {
    // A `Box` centers on both axes; a `Column`'s alignment is the horizontal one only.
    cu::Box().alignment(Alignment::Center).modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
        cu::Column().alignment(Alignment::Center).space_role(SpaceRole::Sm).content(|| {
            Label(menu.label(), Fs::Xl3, Weight::Medium, None, None);
            Label(PLACEHOLDER_TEXT, Fs::Md, Weight::Regular, Some(paint(ember_theme().palette.muted)), None);
        });
    });
}

// ---------------------------------------------------------------------------------------------
// Root
// ---------------------------------------------------------------------------------------------

/// The layout the window's width asks for. `current_window_size` is read through the
/// composition's own state, so the shell recomposes when the renderer reports a new size
/// class; `compose_rust::window_size()` is a plain read that never wakes anything, and reads
/// zero (so: bottom tabs) until the first report.
pub fn current_mode(collapsed: bool) -> ShellMode {
    nav_mode(compose_rust::runtime::current_window_size().width_dp, collapsed)
}

/// The root layout: navigation around the content area.
#[composable]
pub fn shell() {
    let ui = shared_ui().clone();
    super::notice::NoticeHost();
    let theme = ember_theme();
    let collapsed = remember(|| mutable_state_of(false));
    let more_open = remember(|| mutable_state_of(false));
    let settings_open = remember(|| mutable_state_of(false));
    let phone_menu = remember(|| mutable_state_of(false));

    let route = ui.route.get();
    let active = active_menu(&route, ui.menu.get());
    let screen = screen_for(&route, active);
    let mode = current_mode(collapsed.get());
    let strip = crate::ui::safe_area::top_reservation(crate::ui::safe_area::safe_area(), mode).content_strip;
    let counts = ui.with_rows_no_previews(|st, _| {
        let cards = model::project_cards(st);
        NavCounts {
            projects: Some(cards.len()),
            waiting: cards.iter().map(|c| c.summary.waiting).sum(),
            ..NavCounts::default()
        }
    });

    let content = || {
        cu::Column().modifier(Modifier::new().weight(1.0).fill_max_width().fill_max_height()).content(|| {
            if cfg!(target_os = "windows") {
                // Caption strip: an empty drag region, then the width the caption buttons cover.
                cu::Row()
                    .modifier(Modifier::new().fill_max_width().height(WINDOWS_CAPTION_HEIGHT_DP))
                    .content(|| {
                        cu::Spacer().modifier(Modifier::new().weight(1.0));
                        cu::Spacer().modifier(Modifier::new().width(caption_reserved_width(true)));
                    });
            }
            // macOS without a sidebar or rail (bottom tabs): the content touches the left edge,
            // where the traffic lights float, so every screen starts below them (FR-L33). The
            // reservation is computed in `shell`, beside `mode`, so one read of the window size
            // drives both.
            if strip > 0.0 {
                cu::Spacer().modifier(Modifier::new().fill_max_width().height(strip));
            }
            cu::Column().modifier(Modifier::new().weight(1.0).fill_max_width()).content(|| {
                // TODO(#220 phases 4-7): each screen replaces its placeholder as it moves.
                if ui.characters.get().is_some() {
                    super::characters_screen::CharactersScreen();
                } else if matches!(screen, Screen::Search) {
                    super::search::SearchScreen();
                } else if matches!(screen, Screen::Channels) {
                    super::channels::ChannelsScreen();
                } else if matches!(screen, Screen::Launcher) {
                    super::project::ProjectScreen();
                } else if matches!(screen, Screen::Files) {
                    super::files::FilesScreen();
                } else if matches!(screen, Screen::Home) {
                    super::home::HomeScreen();
                } else if matches!(screen, Screen::VersionControl) {
                    super::vcs::VcsScreen();
                } else if matches!(screen, Screen::Agents) {
                    super::agents::AgentsScreen();
                } else if matches!(screen, Screen::Routines) {
                    super::routines::RoutinesScreen();
                } else if let Screen::Conversation(ref id) = screen {
                    super::session::SessionScreen(Some(id.clone()));
                } else if matches!(screen, Screen::Placeholder(NavMenu::Sessions)) {
                    super::session::SessionScreen(None);
                } else {
                    let menu = match &screen {
                        Screen::Conversation(_) => NavMenu::Sessions,
                        Screen::Placeholder(m) => *m,
                        _ => active,
                    };
                    Placeholder(menu);
                }
            });
        });
    };

    cu::Box()
        .modifier(Modifier::new().fill_max_width().fill_max_height().background(paint(theme.palette.bg)))
        .content(|| {
            if mode == ShellMode::BottomTabs {
                cu::Column().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
                    content();
                    let u = ui.clone();
                    let more = more_open.clone();
                    BottomBar(active, counts, move |tab: BottomTab| match tab {
                        BottomTab::NavMenu(m) => choose(&u, m),
                        BottomTab::More => more.set(true),
                    });
                });
                let s = settings_open.clone();
                // The mockup hides the menu while a detail view (an open room) is shown.
                if !ui.detail_open.get() {
                    PhoneMenu(phone_menu.clone(), move || s.set(true));
                }
            } else {
                cu::Row().modifier(Modifier::new().fill_max_width().fill_max_height()).content(|| {
                    let c = collapsed.clone();
                    let s = settings_open.clone();
                    Sidebar(
                        mode == ShellMode::Rail,
                        active,
                        counts,
                        move || c.set(toggled_collapse(compose_rust::window_size().width_dp, c.get_untracked())),
                        move || s.set(true),
                    );
                    content();
                });
            }

            let close = more_open.clone();
            EmberSheet("더보기", more_open.get(), move || close.set(false), None, || {
                cu::Column().modifier(Modifier::new().fill_max_width().padding_role(SpaceRole::Md)).space_role(SpaceRole::Sm).content(|| {
                    for menu in MORE_MENUS {
                        let (u, more) = (ui.clone(), more_open.clone());
                        NavRow(menu, menu == active, NavBadge::None, move || {
                            more.set(false);
                            choose(&u, menu);
                        });
                    }
                    // The mockup's mobile more sheet also lists the theme and settings (#218).
                    let svg = if theme.scheme == Scheme::Dark { ICON_MOON_SVG } else { ICON_SUN_SVG };
                    let u = ui.clone();
                    ToolRow(svg, &format!("테마: {}", ui.theme_pref.get().label()), false, move || {
                        u.set_theme_pref(u.theme_pref.get_untracked().next());
                    });
                    let (more, u) = (more_open.clone(), ui.clone());
                    ToolRow(ICON_SEARCH_SVG, "검색", false, move || {
                        more.set(false);
                        u.go(Route::Search);
                    });
                    let (more, settings) = (more_open.clone(), settings_open.clone());
                    ToolRow(ICON_GEAR_SVG, "설정", false, move || {
                        more.set(false);
                        settings.set(true);
                    });
                });
            });
            let close = settings_open.clone();
            super::settings::SettingsSheet(settings_open.get(), move || close.set(false));
            super::new_session::NewSessionDialog();
        });
}

// ---------------------------------------------------------------------------------------------
// Review window
// ---------------------------------------------------------------------------------------------

/// The scheme asked for by `EMBER_CR_SHELL` (`light`, `dark`, or `1`), or `None` when off.
pub fn requested() -> Option<ThemePref> {
    match std::env::var("EMBER_CR_SHELL").ok()?.as_str() {
        "" | "0" => None,
        "dark" => Some(ThemePref::Dark),
        _ => Some(ThemePref::Light),
    }
}

/// `EMBER_CR_SIZE=1440x900`: the window's opening size, for capturing at 1440, 1024, 768, 390.
fn requested_size() -> (u16, u16) {
    std::env::var("EMBER_CR_SIZE")
        .ok()
        .and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        })
        .unwrap_or((1440, 900))
}

/// On macOS the window opens with content under the traffic lights (FR-L33, compose-rust
/// v0.0.6 `Chrome::FullSize`); other systems keep their chrome.
pub fn window_chrome(window: Window) -> Window {
    if cfg!(target_os = "macos") {
        window.with_chrome(compose_rust::schema::Chrome::FullSize)
    } else {
        window
    }
}

/// Opens the compose-rust shell instead of the app and returns when it is closed.
/// `services` must be installed.
pub fn launch(pref: ThemePref) {
    let (builder, content) = launch_parts(pref);
    builder.launch(content);
}

/// The builder and the root every entry point opens the shell with, so the Android entry
/// (FR-L36) draws what the desktop binary draws.
pub fn launch_parts(pref: ThemePref) -> (compose_rust::LaunchBuilder, fn()) {
    shared_ui().theme_pref.set(pref);
    let scheme = if pref == ThemePref::Dark { ColorScheme::Dark } else { ColorScheme::Light };
    let (w, h) = requested_size();
    // `EMBER_CR_ROUTE` opens a screen directly, for review captures.
    let ui = shared_ui();
    let content: fn() = match std::env::var("EMBER_CR_ROUTE").ok().as_deref() {
        Some("search") => {
            ui.go(Route::Search);
            shell
        }
        Some("new-session") => {
            ui.open_new_session(None);
            shell
        }
        Some("launcher-row") => super::launcher::rows_preview,
        Some("monitor") => super::monitor::preview,
        Some("channels") => {
            ui.set_menu(NavMenu::Channels);
            shell
        }
        Some("project") | Some("project-detail") => {
            ui.set_menu(NavMenu::Projects);
            shell
        }
        Some("files") => {
            ui.set_menu(NavMenu::Files);
            shell
        }
        Some("vcs") => {
            ui.set_menu(NavMenu::VersionControl);
            shell
        }
        Some("agents") => {
            ui.set_menu(NavMenu::Agents);
            shell
        }
        Some("routines") => {
            ui.set_menu(NavMenu::Routines);
            shell
        }
        Some("home") => {
            ui.set_menu(NavMenu::Home);
            shell
        }
        Some("session") | Some("sessions") => {
            ui.set_menu(NavMenu::Sessions);
            shell
        }
        Some("characters") => {
            ui.set_menu(NavMenu::Projects);
            ui.open_characters("");
            shell
        }
        Some("conversation") => {
            ui.open_conversation("demo-s1");
            shell
        }
        _ => shell,
    };
    let builder = compose_rust::LaunchBuilder::new()
        .with_theme(crate::ui::fonts::app_theme(scheme))
        .with_window(crate::ui::cr::shell::window_chrome(Window::new().with_title("Ember").with_size(w, h)));
    (builder, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr_l10_choosing_a_menu_changes_the_menu_and_leaves_a_conversation() {
        let ui = SharedUi::new(crate::prefs::Prefs::default());
        ui.open_conversation("s1");
        assert_eq!(ui.menu.get_untracked(), NavMenu::Sessions);
        choose(&ui, NavMenu::Routines);
        assert_eq!(ui.menu.get_untracked(), NavMenu::Routines);
        assert_eq!(ui.route.get_untracked(), Route::Projects);
        choose(&ui, NavMenu::Projects);
        assert_eq!(ui.menu.get_untracked(), NavMenu::Projects);
    }

    #[test]
    fn fr_l28_icon_documents_are_cached_per_icon_size_and_colour() {
        let a = icon_bytes(NavMenu::Home.svg(), 18.0, 0xff112233);
        let b = icon_bytes(NavMenu::Home.svg(), 18.0, 0xff112233);
        assert!(std::ptr::eq(a, b));
        let c = icon_bytes(NavMenu::Home.svg(), 18.0, 0xff445566);
        assert!(!std::ptr::eq(a, c));
    }

    #[composable]
    fn mode_probe() {
        Label(&format!("{:?}", current_mode(false)), Fs::Md, Weight::Regular, None, None);
    }

    fn has_text(batch: &[compose_rust::protocol::Mutation<'_>], want: &str) -> bool {
        use compose_rust::protocol::{Mutation, PropertyValue};
        batch.iter().any(|m| {
            matches!(m, Mutation::SetProp { value: PropertyValue::String(t), .. } if *t == want)
        })
    }

    #[test]
    fn fr_l28_the_shell_mode_follows_the_window_size_events() {
        use compose_rust::protocol::{decode_batch, HostEvent};
        use compose_rust::{EventPayload, Host, Recomposer, WindowHeightClass, WindowSizeClass};
        compose_rust::window::reset_window_size();
        let mut host = Host::new(|| Box::new(Recomposer::new(mode_probe)));
        let first = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        // Nothing reported yet: the phone layout.
        assert!(has_text(&first, "BottomTabs"));
        let mut report = |width: f32, class: WindowSizeClass| {
            let event = HostEvent {
                node_id: 0,
                handler_id: 0,
                payload: EventPayload::WindowSizeChanged {
                    width_dp: width,
                    height_dp: 900.0,
                    class,
                    height_class: WindowHeightClass::Medium,
                    safe_area_top: 0.0,
                    safe_area_leading: 0.0,
                    safe_area_trailing: 0.0,
                },
            };
            let (bytes, _) = host.dispatch(event).expect("dispatch");
            decode_batch(bytes).expect("decode").iter().map(|m| format!("{m:?}")).collect::<String>()
        };
        assert!(report(1440.0, WindowSizeClass::Expanded).contains("Sidebar"));
        assert!(report(1024.0, WindowSizeClass::Medium).contains("Rail"));
        assert!(report(390.0, WindowSizeClass::Compact).contains("BottomTabs"));
        compose_rust::window::reset_window_size();
    }

    #[test]
    fn fr_l12_the_shell_content_column_has_fill_max_width_in_the_bottom_tabs_layout() {
        // In the 390 bottom-tabs layout width is the cross axis of the content column, which a
        // weight does not fill: without `fill_max_width` the page was sized to its content (about
        // 230 dp, #496). The decoded modifier batch does not expose weight and fill together, so
        // this checks the production source of the shell: the content column and the screen
        // column inside it both carry `fill_max_width`.
        let src = include_str!("shell.rs");
        let production = src.split("#[cfg(test)]").next().unwrap();
        assert!(
            production.contains("cu::Column().modifier(Modifier::new().weight(1.0).fill_max_width().fill_max_height())"),
            "the shell content column must fill the width"
        );
        assert!(
            production.contains("cu::Column().modifier(Modifier::new().weight(1.0).fill_max_width())"),
            "the screen column inside it must fill the width"
        );
    }

    #[composable]
    fn sidebar_probe() {
        Sidebar(false, NavMenu::Sessions, NavCounts::default(), || {}, || {});
    }

    #[composable]
    fn rail_probe() {
        Sidebar(true, NavMenu::Sessions, NavCounts::default(), || {}, || {});
    }

    #[test]
    fn fr_l10_sidebar_and_rail_search_clicks_open_search() {
        use compose_rust::protocol::{decode_batch, HostEvent, Mutation, PropertyValue};
        use compose_rust::{EventPayload, Host, PropertyKind, Recomposer};
        use std::collections::HashMap;

        let _guard = crate::services::test_guard();
        // Locate the labelled sidebar row, then use its position among the sidebar click
        // handlers for the icon-only rail, whose search icon has no text node.
        let mut sidebar = Host::new(|| Box::new(Recomposer::new(sidebar_probe)));
        let batch = decode_batch(sidebar.rebuild().expect("rebuild sidebar")).expect("decode sidebar");
        let mut parents = HashMap::new();
        let mut handlers = Vec::new();
        let mut label = None;
        for mutation in &batch {
            match mutation {
                Mutation::Insert { parent_id, node_id, .. } => { parents.insert(*node_id, *parent_id); }
                Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => {
                    handlers.push((*node_id, *h as u64));
                }
                Mutation::SetProp { node_id, property: PropertyKind::Text, value: PropertyValue::String(s) } if *s == "검색" => {
                    label = Some(*node_id);
                }
                _ => {}
            }
        }
        let mut node = label.expect("sidebar search label");
        let index = loop {
            if let Some(index) = handlers.iter().position(|(id, _)| *id == node) { break index; }
            node = *parents.get(&node).expect("search label has a clickable ancestor");
        };
        let ui = shared_ui();
        ui.go(Route::Projects);
        let (node_id, handler_id) = handlers[index];
        sidebar.dispatch(HostEvent { node_id, handler_id, payload: EventPayload::Clicked }).expect("click sidebar search");
        assert_eq!(ui.route.get_untracked(), Route::Search);

        let mut rail = Host::new(|| Box::new(Recomposer::new(rail_probe)));
        let rail_batch = decode_batch(rail.rebuild().expect("rebuild rail")).expect("decode rail");
        let rail_handlers: Vec<_> = rail_batch.iter().filter_map(|m| match m {
            Mutation::SetProp { node_id, property: PropertyKind::OnClick, value: PropertyValue::Integer(h) } => Some((*node_id, *h as u64)),
            _ => None,
        }).collect();
        assert_eq!(rail_handlers.len(), handlers.len());
        ui.go(Route::Projects);
        let (node_id, handler_id) = rail_handlers[index];
        rail.dispatch(HostEvent { node_id, handler_id, payload: EventPayload::Clicked }).expect("click rail search");
        assert_eq!(ui.route.get_untracked(), Route::Search);
        ui.go(Route::Projects);
    }

    #[composable]
    fn phone_menu_probe() {
        let open = remember(|| mutable_state_of(true));
        PhoneMenu(open, || {});
    }

    #[test]
    fn fr_l10_sidebar_has_a_search_row() {
        use compose_rust::protocol::decode_batch;
        use compose_rust::{Host, Recomposer};
        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(sidebar_probe)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        assert!(has_text(&batch, "검색"));
        let mut host_p = Host::new(|| Box::new(Recomposer::new(phone_menu_probe)));
        let batch_p = decode_batch(host_p.rebuild().expect("rebuild")).expect("decode");
        assert!(has_text(&batch_p, "검색"));
    }

    #[test]
    fn fr_l28_demo_sidebar_tools_are_hidden_outside_demo_mode_and_reset_works() {
        use compose_rust::protocol::decode_batch;
        use compose_rust::{Host, Recomposer};

        let _guard = crate::services::test_guard();
        // Outside demo mode (when EMBER_DEMO_DATA is not "1"), neither Sidebar nor PhoneMenu
        // renders the demo-only rows ("플랫폼: 기타", "데모 초기화").
        if std::env::var(crate::demo::DEMO_ENV).ok().as_deref() != Some("1") {
            assert!(!show_demo_tools());
            let mut host_s = Host::new(|| Box::new(Recomposer::new(sidebar_probe)));
            let batch_s = decode_batch(host_s.rebuild().expect("rebuild")).expect("decode");
            assert!(has_text(&batch_s, "설정"));
            assert!(!has_text(&batch_s, "플랫폼: 기타"));
            assert!(!has_text(&batch_s, "데모 초기화"));

            let mut host_p = Host::new(|| Box::new(Recomposer::new(phone_menu_probe)));
            let batch_p = decode_batch(host_p.rebuild().expect("rebuild")).expect("decode");
            assert!(has_text(&batch_p, "설정"));
            assert!(!has_text(&batch_p, "플랫폼: 기타"));
            assert!(!has_text(&batch_p, "데모 초기화"));
        }

        // Verify `services().reset_demo()` genuinely restores demo state after a mutation.
        let s = crate::services::services();
        let d = s.demo.as_ref().expect("test_services installs DemoSource");
        let initial = d.events_for("demo-s1").len();
        d.send_message(&s.client, "demo-s1", "mutated message");
        let len_after_send = d.events_for("demo-s1").len();
        assert!(len_after_send > initial);
        assert!(s.reset_demo());
        assert_eq!(d.events_for("demo-s1").len(), initial);
    }

    #[test]
    fn fr_l28_shell_composes_settings_sheet_and_every_screen() {
        use compose_rust::protocol::decode_batch;
        use compose_rust::{Host, Recomposer};

        let _guard = crate::services::test_guard();
        let mut host = Host::new(|| Box::new(Recomposer::new(shell)));
        let batch = decode_batch(host.rebuild().expect("rebuild")).expect("decode");
        // The shell mounts SettingsSheet (with theme, keymap, default IDE and archived toggle)
        // instead of the old "준비 중입니다" placeholder.
        assert!(has_text(&batch, "설정"));
        assert!(has_text(&batch, "화면 테마"));
        assert!(has_text(&batch, "에디터 단축키"));
        assert!(has_text(&batch, "기본 IDE"));
        assert!(has_text(&batch, "보관된 세션 표시"));
    }
}
