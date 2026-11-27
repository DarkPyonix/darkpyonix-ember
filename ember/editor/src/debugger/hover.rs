//! The debug hover's session side (FR-L31.42): which expression a hover is over, and the value
//! of it in the focused frame.
//!
//! [`expression_at`] is Code-OSS's "one-size-fits-all strategy" of
//! `getEvaluatableExpressionAtPosition` (`debugUtils.ts`), built on
//! `getExactExpressionStartAndEnd`: the run of characters around the pointer that are not
//! operators, brackets or spaces (`->` is kept), without a leading `...`, cut after the word under
//! the pointer, so `a.b.c` hovered on `b` is `a.b`. Columns are 1-based UTF-16 code units, as in
//! Monaco. A language extension's `EvaluatableExpressionProvider` is not asked yet (#445).
//!
//! The value comes as `DebugHoverComputer.evaluate` gets it: an adapter with
//! `supportsEvaluateForHovers` is sent `evaluate` with the context `hover` (and the hovered line,
//! column and source); any other adapter has the expression looked up by name in the frame's
//! scopes that are not expensive, one `.`-separated part at a time ([`HoverSearch`], Code-OSS's
//! `findExpressionInStackFrame`), and it is shown only when every scope that has it agrees on the
//! value.

use super::VariableInfo;

/// What a hover is over: the expression and its 1-based columns on its line, `end_column`
/// exclusive (Monaco's `IRange`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverExpression {
    pub expression: String,
    pub start_column: u32,
    pub end_column: u32,
}

/// The characters `getExactExpressionStartAndEnd` does not take into an expression:
/// `[^()\[\]{}<>\s+\-/%~#^;=|,`!]`.
fn breaks(c: char) -> bool {
    matches!(
        c,
        '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '+' | '-' | '/' | '%' | '~' | '#' | '^'
            | ';' | '=' | '|' | ',' | '`' | '!'
    ) || js_space(c)
}

/// JavaScript's `\s`.
fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
            | '\u{feff}'
    )
}

/// `(\w|\p{L})`: a character of the word the expression is cut after.
fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c.is_alphabetic()
}

fn utf16_len(chars: &[char]) -> u32 {
    chars.iter().map(|c| c.len_utf16() as u32).sum()
}

/// Code-OSS's `getExactExpressionStartAndEnd`: the 1-based first and last column (both
/// inclusive) of the expression that spans `loose_start..=loose_end`, or `(0, 0)` when there is
/// none.
pub fn exact_expression_start_and_end(line: &str, loose_start: u32, loose_end: u32) -> (u32, u32) {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    // First find the full expression under the cursor.
    let mut found: Option<(usize, usize, u32)> = None;
    let mut i = 0;
    while i < n {
        let mut j = i;
        loop {
            if j < n && !breaks(chars[j]) {
                j += 1;
            } else if j + 1 < n && chars[j] == '-' && chars[j + 1] == '>' {
                j += 2;
            } else {
                break;
            }
        }
        if j == i {
            i += 1;
            continue;
        }
        let start = utf16_len(&chars[..i]) + 1;
        let end = start + utf16_len(&chars[i..j]);
        if start <= loose_start && end >= loose_end {
            found = Some((i, j, start));
            break;
        }
        i = j;
    }
    let Some((i, j, mut start)) = found else {
        return (0, 0);
    };
    let mut matching: &[char] = &chars[i..j];
    // Spread syntax: `...foo` is `foo`.
    if matching.len() > 3 && matching[..3] == ['.', '.', '.'] {
        matching = &matching[3..];
        start += 3;
    }
    // Non-word characters after the cursor cut the expression: `a.b.c.d` hovered on `b` is `a.b`.
    let mut k = 0;
    while k < matching.len() {
        if !word(matching[k]) {
            k += 1;
            continue;
        }
        let mut e = k;
        while e < matching.len() && word(matching[e]) {
            e += 1;
        }
        let index = utf16_len(&matching[..k]);
        let sub_end = index + 1 + start + utf16_len(&matching[k..e]);
        if sub_end >= loose_end {
            matching = &matching[..e];
            break;
        }
        k = e;
    }
    (start, start + utf16_len(matching) - 1)
}

/// The expression a hover at the 1-based `column` of `line` evaluates
/// (`getEvaluatableExpressionAtPosition` without a provider). `None` where there is no
/// expression: the language hover shows instead.
pub fn expression_at(line: &str, column: u32) -> Option<HoverExpression> {
    let (start, end) = exact_expression_start_and_end(line, column, column);
    if start == 0 {
        return None;
    }
    let units: Vec<u16> = line.encode_utf16().collect();
    let from = (start - 1) as usize;
    let to = (end as usize).min(units.len());
    let expression = String::from_utf16_lossy(units.get(from..to)?);
    if expression.is_empty() {
        return None;
    }
    let len = (to - from) as u32;
    Some(HoverExpression {
        expression,
        start_column: start,
        end_column: start + len,
    })
}

/// The names a hover looks up in the frame's scopes when the adapter cannot evaluate for hovers:
/// the expression split at `.`, each part trimmed, empty parts dropped.
pub fn lookup_names(expression: &str) -> Vec<String> {
    expression
        .split('.')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// What a step of a [`HoverSearch`] asks next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchStep {
    /// Request the variables of `reference` for the part at `depth`.
    Variables { reference: i64, depth: usize },
}

/// `findExpressionInStackFrame` for one hover: the scopes of the frame that are not expensive are
/// searched for `names`, a part at a time, each part matching exactly one child.
#[derive(Debug, Clone, PartialEq)]
pub struct HoverSearch {
    pub session: String,
    names: Vec<String>,
    waiting: usize,
    found: Vec<VariableInfo>,
}

/// How a search ended: the variable to show, or why there is none.
pub type SearchResult = Result<VariableInfo, String>;

/// Why the hover has nothing to show; the window falls back to the language hover.
pub const NOT_AVAILABLE: &str = "not available";

impl HoverSearch {
    pub fn new(session: String, expression: &str) -> Self {
        Self {
            session,
            names: lookup_names(expression),
            waiting: 0,
            found: Vec::new(),
        }
    }

    /// The frame's scopes came: `(variablesReference, expensive)` each. Returns the requests to
    /// send, and the end of the search when there is nothing to ask.
    pub fn scopes(&mut self, scopes: &[(i64, bool)]) -> (Vec<SearchStep>, Option<SearchResult>) {
        let steps: Vec<SearchStep> = if self.names.is_empty() {
            Vec::new()
        } else {
            scopes
                .iter()
                .filter(|(reference, expensive)| !expensive && *reference > 0)
                .map(|(reference, _)| SearchStep::Variables {
                    reference: *reference,
                    depth: 0,
                })
                .collect()
        };
        self.waiting += steps.len();
        let done = (self.waiting == 0).then(|| self.result());
        (steps, done)
    }

    /// The variables asked at `depth` came (empty for a failed request).
    pub fn variables(
        &mut self,
        depth: usize,
        variables: &[VariableInfo],
    ) -> (Vec<SearchStep>, Option<SearchResult>) {
        self.waiting = self.waiting.saturating_sub(1);
        let mut steps = Vec::new();
        if let Some(name) = self.names.get(depth) {
            let mut matching = variables.iter().filter(|v| &v.name == name);
            if let (Some(v), None) = (matching.next(), matching.next()) {
                if depth + 1 == self.names.len() {
                    self.found.push(v.clone());
                } else if v.variables_reference > 0 {
                    steps.push(SearchStep::Variables {
                        reference: v.variables_reference,
                        depth: depth + 1,
                    });
                }
            }
        }
        self.waiting += steps.len();
        let done = (self.waiting == 0).then(|| self.result());
        (steps, done)
    }

    /// Shown only when every scope that has the expression has the same value.
    fn result(&self) -> SearchResult {
        match self.found.first() {
            Some(first) if self.found.iter().all(|v| v.value == first.value) => Ok(first.clone()),
            _ => Err(NOT_AVAILABLE.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cases of Code-OSS's `debugUtils.test.ts` (`getExactExpressionStartAndEnd`).
    #[test]
    fn fr_l31_42_hover_expression_follows_code_oss_cases() {
        let cases: &[(&str, u32, u32, (u32, u32))] = &[
            ("foo", 1, 2, (1, 3)),
            ("!foo", 2, 3, (2, 4)),
            ("foo", 1, 3, (1, 3)),
            ("foo", 1, 4, (1, 3)),
            ("this.name = \"John\"", 1, 10, (1, 9)),
            ("this.name = \"John\"", 6, 10, (1, 9)),
            ("this->address = \"Main street\"", 6, 10, (1, 13)),
            ("var t = a.b.c.d.name", 16, 20, (9, 20)),
            ("MyClass::StaticProp", 10, 20, (1, 19)),
            ("largeNumber = myVar?.prop", 21, 25, (15, 25)),
            ("var t = a.b.c.d.name", 11, 12, (9, 11)),
            ("var t = a.b;c.d.name", 16, 20, (13, 20)),
            ("var t = a.b.c-d.name", 16, 20, (15, 20)),
            ("var a\u{f8}\u{f1}\u{e9}\u{e5}\u{6587} = a.b.c-d.name", 5, 5, (5, 10)),
            (
                "a\u{f8}\u{f1}\u{e9}\u{e5}\u{6587}.a\u{f8}\u{f1}\u{e9}\u{e5}\u{6587}.a\u{f8}\u{f1}\u{e9}\u{e5}\u{6587}",
                9,
                9,
                (1, 13),
            ),
            ("[...bar]", 5, 7, (5, 7)),
            ("...variable", 5, 5, (4, 11)),
        ];
        for (line, start, end, want) in cases {
            assert_eq!(
                exact_expression_start_and_end(line, *start, *end),
                *want,
                "{line:?} {start}..{end}"
            );
        }
        assert_eq!(exact_expression_start_and_end("   ", 2, 2), (0, 0));
    }

    #[test]
    fn fr_l31_42_hover_expression_at_a_column_gives_the_range_or_nothing() {
        assert_eq!(
            expression_at("    total = order.items.count", 21),
            Some(HoverExpression {
                expression: "order.items".into(),
                start_column: 13,
                end_column: 24,
            })
        );
        assert_eq!(
            expression_at("x = 1", 1).map(|e| e.expression),
            Some("x".into())
        );
        // Over the spaces and the `=` there is nothing to evaluate.
        assert_eq!(expression_at("x  =  1", 4), None);
        assert_eq!(expression_at("", 1), None);
    }

    fn var(name: &str, value: &str, reference: i64) -> VariableInfo {
        VariableInfo {
            name: name.into(),
            value: value.into(),
            type_name: None,
            variables_reference: reference,
            evaluate_name: None,
            read_only: false,
        }
    }

    #[test]
    fn fr_l31_42_hover_search_walks_the_cheap_scopes_part_by_part() {
        let mut search = HoverSearch::new("s".into(), "order . items");
        let (steps, done) = search.scopes(&[(10, false), (11, true), (0, false), (12, false)]);
        assert_eq!(
            steps,
            vec![
                SearchStep::Variables { reference: 10, depth: 0 },
                SearchStep::Variables { reference: 12, depth: 0 },
            ],
            "the expensive scope and the one without variables are skipped"
        );
        assert_eq!(done, None);
        let (steps, done) = search.variables(0, &[var("order", "Order", 20), var("x", "1", 0)]);
        assert_eq!(steps, vec![SearchStep::Variables { reference: 20, depth: 1 }]);
        assert_eq!(done, None);
        // The other scope has no `order`.
        let (steps, done) = search.variables(0, &[var("y", "2", 0)]);
        assert!(steps.is_empty());
        assert_eq!(done, None);
        let (steps, done) = search.variables(1, &[var("items", "[3]", 30)]);
        assert!(steps.is_empty());
        assert_eq!(done, Some(Ok(var("items", "[3]", 30))));
    }

    #[test]
    fn fr_l31_42_hover_search_shows_nothing_when_scopes_disagree_or_names_repeat() {
        let mut search = HoverSearch::new("s".into(), "x");
        search.scopes(&[(1, false), (2, false)]);
        search.variables(0, &[var("x", "1", 0)]);
        let (_, done) = search.variables(0, &[var("x", "2", 0)]);
        assert_eq!(done, Some(Err(NOT_AVAILABLE.into())));

        let mut search = HoverSearch::new("s".into(), "x");
        search.scopes(&[(1, false)]);
        let (_, done) = search.variables(0, &[var("x", "1", 0), var("x", "1", 0)]);
        assert_eq!(done, Some(Err(NOT_AVAILABLE.into())), "two children of one name match none");

        let mut search = HoverSearch::new("s".into(), "x");
        let (steps, done) = search.scopes(&[(1, true)]);
        assert!(steps.is_empty());
        assert_eq!(done, Some(Err(NOT_AVAILABLE.into())), "only expensive scopes");
    }
}
