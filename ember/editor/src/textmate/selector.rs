//! Scope selectors, the language of `injectionSelector` and of the keys of a grammar's `injections`.
//!
//! The syntax is vscode-textmate's matcher (a subset of the theme selector syntax):
//!
//! * `a.b` matches a scope that is `a.b` or starts with `a.b.`;
//! * identifiers separated by spaces are a path: `a b` needs a scope matching `a` and, later in the stack, one
//!   matching `b`;
//! * `-x` is "not x", `x | y` and `x, y` are "either", `( ... )` groups, and what stands side by side
//!   (`text.html -comment`) must all hold;
//! * a leading `L:` or `R:` on a comma separated alternative gives it a priority: `L:` wins a tie against the
//!   grammar's own rules, `R:` loses it.
//!
//! A selector comes from a grammar, which comes from an extension, so it is input from outside: its length,
//! its tokens and its nesting are capped, and one over a cap is rejected whole.

use super::interpreter::Scope;

/// Longest selector, in characters.
pub const MAX_SELECTOR_CHARS: usize = 1024;
/// Most tokens in a selector.
pub const MAX_SELECTOR_TOKENS: usize = 512;
/// Deepest nesting of parentheses.
pub const MAX_SELECTOR_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher {
    /// Every part holds (an empty list holds).
    And(Vec<Matcher>),
    /// Some part holds.
    Or(Vec<Matcher>),
    Not(Box<Matcher>),
    /// Scopes named in this order somewhere in the stack.
    Path(Vec<String>),
}

fn scope_matches(scope: &str, name: &str) -> bool {
    scope == name || (scope.len() > name.len() && scope.starts_with(name) && scope.as_bytes()[name.len()] == b'.')
}

impl Matcher {
    /// Whether a scope stack (outermost first) satisfies the matcher.
    pub fn matches(&self, scopes: &[Scope]) -> bool {
        match self {
            Matcher::And(parts) => parts.iter().all(|m| m.matches(scopes)),
            Matcher::Or(parts) => parts.iter().any(|m| m.matches(scopes)),
            Matcher::Not(inner) => !inner.matches(scopes),
            Matcher::Path(names) => {
                if scopes.len() < names.len() {
                    return false;
                }
                let mut from = 0;
                names.iter().all(|name| match scopes[from..].iter().position(|s| scope_matches(s, name)) {
                    Some(i) => {
                        from += i + 1;
                        true
                    }
                    None => false,
                })
            }
        }
    }
}

/// A parsed selector: its comma separated alternatives, each with a priority (`-1` for `L:`, `1` for `R:`, else 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    pub alternatives: Vec<(Matcher, i8)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Priority(i8),
    Ident(String),
    Punct(char),
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == ':'
}

fn tokenize(src: &str) -> Option<Vec<Token>> {
    if src.chars().count() > MAX_SELECTOR_CHARS {
        return None;
    }
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if (c == 'L' || c == 'R') && chars.get(i + 1) == Some(&':') {
            out.push(Token::Priority(if c == 'L' { -1 } else { 1 }));
            i += 2;
        } else if is_ident_start(c) {
            let start = i;
            i += 1;
            while i < chars.len() && (is_ident_start(chars[i]) || chars[i] == '-') {
                i += 1;
            }
            out.push(Token::Ident(chars[start..i].iter().collect()));
        } else if matches!(c, ',' | '|' | '-' | '(' | ')') {
            out.push(Token::Punct(c));
            i += 1;
        } else {
            // A character the syntax has no use for: skipped, as vscode-textmate's tokenizer does.
            i += 1;
        }
        if out.len() > MAX_SELECTOR_TOKENS {
            return None;
        }
    }
    Some(out)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn operand(&mut self, depth: usize) -> Option<Option<Matcher>> {
        if depth > MAX_SELECTOR_DEPTH {
            return None;
        }
        match self.peek().cloned() {
            Some(Token::Punct('-')) => {
                self.at += 1;
                let inner = self.operand(depth + 1)?;
                // "-" with nothing to negate matches nothing, as in vscode-textmate.
                Some(Some(match inner {
                    Some(m) => Matcher::Not(Box::new(m)),
                    None => Matcher::Or(Vec::new()),
                }))
            }
            Some(Token::Punct('(')) => {
                self.at += 1;
                let inner = self.inner(depth + 1)?;
                if self.peek() == Some(&Token::Punct(')')) {
                    self.at += 1;
                }
                Some(Some(inner))
            }
            Some(Token::Ident(_)) => {
                let mut names = Vec::new();
                while let Some(Token::Ident(name)) = self.peek().cloned() {
                    names.push(name);
                    self.at += 1;
                }
                Some(Some(Matcher::Path(names)))
            }
            _ => Some(None),
        }
    }

    fn conjunction(&mut self, depth: usize) -> Option<Matcher> {
        let mut parts = Vec::new();
        while let Some(m) = self.operand(depth)? {
            parts.push(m);
        }
        Some(Matcher::And(parts))
    }

    fn inner(&mut self, depth: usize) -> Option<Matcher> {
        let mut parts = vec![self.conjunction(depth)?];
        while matches!(self.peek(), Some(Token::Punct('|')) | Some(Token::Punct(','))) {
            while matches!(self.peek(), Some(Token::Punct('|')) | Some(Token::Punct(','))) {
                self.at += 1;
            }
            parts.push(self.conjunction(depth)?);
        }
        Some(Matcher::Or(parts))
    }
}

impl Selector {
    /// `None` for a selector over a cap (see the module documentation); anything else parses, as in
    /// vscode-textmate, where a stray token only ends an alternative.
    pub fn parse(src: &str) -> Option<Self> {
        let tokens = tokenize(src)?;
        let mut p = Parser { tokens, at: 0 };
        let mut alternatives = Vec::new();
        loop {
            let mut priority = 0i8;
            if let Some(Token::Priority(v)) = p.peek() {
                priority = *v;
                p.at += 1;
            }
            let matcher = p.conjunction(0)?;
            alternatives.push((matcher, priority));
            if p.peek() == Some(&Token::Punct(',')) {
                p.at += 1;
            } else {
                break;
            }
        }
        Some(Self { alternatives })
    }
}
