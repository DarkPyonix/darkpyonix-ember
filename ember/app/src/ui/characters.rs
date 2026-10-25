#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::project::DEFAULT_PROJECT_ID;

    fn spec(style: CharStyle, name: &str, agent: &str) -> CharSpec {
        CharSpec {
            style,
            name: name.into(),
            agent: agent.into(),
            state: CharState::Idle,
            lead: false,
            role: None,
            dark: false,
        }
    }

    // FR-L22: the same name always gives the same character.
    #[test]
    fn same_name_gives_the_same_svg() {
        for style in CharStyle::ALL {
            let a = svg(&spec(style, "ember-리더", "claude"));
            let b = svg(&spec(style, "ember-리더", "claude"));
            assert_eq!(a, b, "{style:?}");
        }
        assert_eq!(seed_hash("ember"), seed_hash("ember"));
        assert_ne!(seed_hash("ember"), seed_hash("embers"));
    }

    // FR-L22: well-formed output, for every style, state and scheme.
    #[test]
    fn svg_is_well_formed_for_every_style_and_state() {
        for style in CharStyle::ALL {
            for state in CharState::ALL {
                for dark in [false, true] {
                    for lead in [false, true] {
                        let mut s = spec(style, "리뷰어", "codex");
                        s.state = state;
                        s.dark = dark;
                        s.lead = lead;
                        s.role = Some("리뷰어".into());
                        let out = svg(&s);
                        assert!(out.starts_with("<svg"), "{style:?} {state:?}");
                        assert!(out.ends_with("</svg>"), "{style:?} {state:?}");
                        assert!(is_balanced(&out), "{style:?} {state:?}: {out}");
                        assert!(!out.contains('\u{2014}'));
                        assert!(!out.contains("NaN") && !out.contains("inf"));
                    }
                }
            }
        }
    }

    // Open tags are closed or self-closing, and `<` and `>` pair up.
    fn is_balanced(s: &str) -> bool {
        let mut stack: Vec<String> = Vec::new();
        let mut rest = s;
        while let Some(i) = rest.find('<') {
            let Some(j) = rest[i..].find('>') else { return false };
            let tag = &rest[i + 1..i + j];
            rest = &rest[i + j + 1..];
            if let Some(name) = tag.strip_prefix('/') {
                if stack.pop().as_deref() != Some(name.trim()) {
                    return false;
                }
            } else if !tag.ends_with('/') {
                let name = tag.split_whitespace().next().unwrap_or("");
                stack.push(name.to_string());
            }
        }
        stack.is_empty() && !rest.contains('>')
    }

    // FR-L22: different seeds give different shapes in every style.
    #[test]
    fn different_names_give_different_drawings() {
        for style in CharStyle::ALL {
            let outs: std::collections::BTreeSet<String> = ["가", "나", "다", "ember-a", "ember-b", "qa", "docs"]
                .iter()
                .map(|n| svg(&spec(style, n, "claude")))
                .collect();
            assert!(outs.len() >= 5, "{style:?} drew only {} looks", outs.len());
        }
    }

    // DESIGN section 7: the harness sets the colour family.
    #[test]
    fn harness_sets_the_hue_family() {
        assert_eq!(family_of("claude-code"), Family::Claude);
        assert_eq!(family_of("codex"), Family::Codex);
        assert_eq!(family_of("gemini"), Family::Gemini);
        assert_eq!(family_of("unknown"), Family::Claude);
        assert_eq!(hue_range(Family::Claude), (20.0, 34.0));
        assert_eq!(hue_range(Family::Codex), (218.0, 238.0));
        assert_eq!(hue_range(Family::Gemini), (196.0, 212.0));
        for name in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            for (agent, fam) in [("claude", Family::Claude), ("codex", Family::Codex), ("gemini", Family::Gemini)] {
                let (lo, hi) = hue_range(fam);
                let h = body_hue(name, agent);
                assert!(h >= lo && h <= hi, "{name} {agent} {h}");
            }
        }
    }

    // Dark mode is slightly darker than light.
    #[test]
    fn dark_scheme_changes_the_colours() {
        let light = svg(&spec(CharStyle::Mongle, "x", "claude"));
        let mut d = spec(CharStyle::Mongle, "x", "claude");
        d.dark = true;
        assert_ne!(light, svg(&d));
    }

    // FR-L24: the crown only when lead, and never in 기본 프로젝트.
    #[test]
    fn crown_only_for_a_leader_outside_the_default_project() {
        assert!(crown_shown("ember", true));
        assert!(!crown_shown("ember", false));
        assert!(!crown_shown(DEFAULT_PROJECT_ID, true));
        for style in CharStyle::ALL {
            let plain = svg(&spec(style, "m", "claude"));
            let mut l = spec(style, "m", "claude");
            l.lead = true;
            assert!(!plain.contains(CROWN_ID), "{style:?}");
            assert!(svg(&l).contains(CROWN_ID), "{style:?}");
        }
    }

    // 크루 role props.
    #[test]
    fn crew_role_props_follow_the_role() {
        assert_eq!(role_prop("리뷰어"), Some(Prop::Tie));
        assert_eq!(role_prop("테스터"), Some(Prop::Phones));
        assert_eq!(role_prop("디자인"), Some(Prop::Cap));
        assert_eq!(role_prop("문서"), Some(Prop::Bow));
        assert_eq!(role_prop("조사"), Some(Prop::Beret));
        assert_eq!(role_prop("없는 역할"), None);
        let mut a = spec(CharStyle::Crew, "m", "claude");
        let mut b = a.clone();
        a.role = Some("리뷰어".into());
        b.role = Some("테스터".into());
        assert_ne!(svg(&a), svg(&b));
    }

    // States are poses: each state draws differently.
    #[test]
    fn each_state_has_its_own_pose() {
        for style in CharStyle::ALL {
            let outs: std::collections::BTreeSet<String> = CharState::ALL
                .iter()
                .map(|st| {
                    let mut s = spec(style, "m", "gemini");
                    s.state = *st;
                    svg(&s)
                })
                .collect();
            assert_eq!(outs.len(), CharState::ALL.len(), "{style:?}");
        }
    }

    #[test]
    fn launcher_status_maps_to_a_state() {
        use ember_client::LauncherStatus as S;
        assert_eq!(CharState::from_status(S::Running), CharState::Run);
        assert_eq!(CharState::from_status(S::WaitingForApproval), CharState::Wait);
        assert_eq!(CharState::from_status(S::Failed), CharState::Fail);
        assert_eq!(CharState::from_status(S::Finished), CharState::Done);
        assert_eq!(CharState::from_status(S::FinishedUnread), CharState::Done);
        assert_eq!(CharState::from_status(S::Idle), CharState::Idle);
    }

    #[test]
    fn six_styles_with_ids_and_korean_names() {
        let names: Vec<&str> = CharStyle::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(names, ["몽글", "말랑", "크루", "포켓", "크림", "키링"]);
        assert_eq!(CharStyle::default(), CharStyle::Mongle);
        for s in CharStyle::ALL {
            assert!(!s.blurb().is_empty());
            assert!(!s.blurb().contains('\u{2014}'));
        }
    }

    #[test]
    fn style_serializes_as_a_lowercase_id() {
        assert_eq!(serde_json::to_string(&CharStyle::Keyring).unwrap(), "\"keyring\"");
        assert_eq!(serde_json::from_str::<CharStyle>("\"crew\"").unwrap(), CharStyle::Crew);
    }
}
