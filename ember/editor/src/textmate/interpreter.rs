//! The TextMate grammar interpreter: compiles [`RawGrammar`]s into rules and tokenizes a line at a time.
//!
//! It follows vscode-textmate (the engine VS Code runs), rule for rule: `match`, `begin`/`end`,
//! `begin`/`while`, `captures` (with nested `patterns` and `$n` in scope names), `include` (`#repo`,
//! `$self`, `$base`, another grammar's scope, `scope#repo`), end patterns that use the begin match's
//! captures (`\1`), `applyEndPatternLast`, the `\A` and `\G` anchors, and the guards against a grammar
//! that pushes or pops without advancing. Regular expressions run on Oniguruma, the engine the
//! grammars are written for.
//!
//! Injections: the root grammar's `injections` (a scope selector per rule), and the grammars that name the root
//! in `injectTo` and carry an `injectionSelector`, add rules that compete with the rules of the scope they are
//! injected into, as in vscode-textmate. As there, only the root grammar's own `injections` count, not those of
//! the grammars it includes.
//!
//! A line goes in without its line break; the interpreter appends the `\n` grammars expect and removes
//! it from the result. Offsets in the result are byte offsets into the line.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use onig::{MatchParam, Regex, RegexOptions, Region, SearchOptions, Syntax};

use super::grammar::{RawGrammar, RawRule};
use super::selector::{Matcher, Selector};

/// A line longer than this many bytes is not tokenized: it gets one token with the scopes in force and the
/// state is left as it was. Minified files have lines of megabytes, and a search is linear in the line for
/// every pattern. (VS Code's `editor.maxTokenizationLineLength` is 20000 as well.)
pub const MAX_LINE_BYTES: usize = 20_000;
/// The most rules open at once. A grammar that nests without end (a `begin` that matches again inside its
/// own region) stops here instead of growing the stack for the whole file.
pub const MAX_STACK_DEPTH: usize = 256;
/// The most rules one compiled grammar may hold.
pub const MAX_COMPILED_RULES: usize = 200_000;
/// Backtracking steps one regular expression search may take before Oniguruma gives up (the search then
/// finds nothing). Its default is ten million; a pattern such as `(a+)+$` on a long line would take
/// seconds at that.
const REGEX_RETRY_LIMIT: u32 = 500_000;
/// Oniguruma's match stack, in elements: bounds the memory a single search may use.
const REGEX_STACK_LIMIT: u32 = 1_000_000;

/// One search with the limits above. A search that hits a limit (or fails for any other reason) finds
/// nothing: the `onig` crate's own `search_with_options` panics on that, which a grammar must not be able
/// to make the editor do.
fn search(regex: &Regex, text: &str, from: usize, region: &mut Region) -> Option<usize> {
    let mut param = MatchParam::default();
    param.set_retry_limit_in_match(REGEX_RETRY_LIMIT);
    param.set_match_stack_limit(REGEX_STACK_LIMIT);
    regex.search_with_param(text, from, text.len(), SearchOptions::SEARCH_OPTION_NONE, Some(region), param).ok().flatten()
}

/// One scope name (`source.ts`, `string.quoted.double`), shared between tokens.
pub type Scope = Arc<str>;
type RuleId = usize;

/// A compiled regular expression. `\A` and `\G` only match where the grammar allows them, which
/// depends on where the search starts, so up to four variants of the source are compiled, each on
/// first use.
struct Pattern {
    source: String,
    uses_a: bool,
    uses_g: bool,
    variants: [OnceLock<Option<Regex>>; 4],
}

impl Pattern {
    fn new(source: String) -> Self {
        let (uses_a, uses_g) = anchors_used(&source);
        Self { source, uses_a, uses_g, variants: Default::default() }
    }

    fn regex(&self, allow_a: bool, allow_g: bool) -> Option<&Regex> {
        let a = allow_a || !self.uses_a;
        let g = allow_g || !self.uses_g;
        let slot = &self.variants[usize::from(a) | (usize::from(g) << 1)];
        slot.get_or_init(|| {
            let src = if a && g { std::borrow::Cow::Borrowed(self.source.as_str()) } else { std::borrow::Cow::Owned(disable_anchors(&self.source, a, g)) };
            Regex::with_options(&src, RegexOptions::REGEX_OPTION_CAPTURE_GROUP, Syntax::default()).ok()
        })
        .as_ref()
    }
}

fn anchors_used(source: &str) -> (bool, bool) {
    let (mut a, mut g) = (false, false);
    let mut chars = source.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('A') => a = true,
                Some('G') => g = true,
                _ => {}
            }
        }
    }
    (a, g)
}

/// The source with `\A` and `\G` replaced by a character that never occurs, where they must not match.
fn disable_anchors(source: &str, allow_a: bool, allow_g: bool) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('A') if !allow_a => out.push('\u{FFFF}'),
            Some('G') if !allow_g => out.push('\u{FFFF}'),
            Some(next) => {
                out.push('\\');
                out.push(next);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// A scope name that may use the match's captures: `keyword.$1`, `${1:/downcase}`.
#[derive(Debug, Clone)]
struct Name {
    text: String,
    capturing: bool,
}

impl Name {
    fn new(text: &str) -> Self {
        Self { text: text.to_owned(), capturing: text.contains('$') }
    }
}

struct Capture {
    index: usize,
    name: Option<Name>,
    content_name: Option<Name>,
    /// A group rule holding the capture's own `patterns`: the captured text is tokenized again with them.
    retokenize: Option<RuleId>,
}

enum Kind {
    Match { pattern: Option<Pattern>, captures: Vec<Capture> },
    BeginEnd {
        begin: Option<Pattern>,
        end: Option<Pattern>,
        end_has_backrefs: bool,
        begin_captures: Vec<Capture>,
        end_captures: Vec<Capture>,
        patterns: Vec<RuleId>,
        apply_end_last: bool,
    },
    BeginWhile {
        begin: Option<Pattern>,
        while_: Option<Pattern>,
        while_has_backrefs: bool,
        begin_captures: Vec<Capture>,
        while_captures: Vec<Capture>,
        patterns: Vec<RuleId>,
    },
    /// Only `patterns` (or an `include` that resolved to nothing): a list of rules.
    Group { patterns: Vec<RuleId> },
}

struct Rule {
    name: Option<Name>,
    content_name: Option<Name>,
    kind: Kind,
    /// The match and begin rules reachable through groups, in order (computed on first use).
    flat: OnceLock<Vec<RuleId>>,
}

/// Grammars by scope name: the root grammar and every grammar it may include.
#[derive(Debug, Clone, Default)]
pub struct GrammarSet {
    grammars: HashMap<String, RawGrammar>,
    /// Grammars injected into a scope (what their `injectTo` says), by that scope.
    injectors: HashMap<String, Vec<String>>,
}

impl GrammarSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, grammar: RawGrammar) {
        self.grammars.insert(grammar.scope_name.clone(), grammar);
    }

    /// Declares that the grammar `injector` (its `injectionSelector` and its patterns) is injected into `target`.
    pub fn add_injector(&mut self, target: &str, injector: &str) {
        let list = self.injectors.entry(target.to_owned()).or_default();
        if !list.iter().any(|s| s == injector) {
            list.push(injector.to_owned());
        }
    }

    pub fn contains(&self, scope: &str) -> bool {
        self.grammars.contains_key(scope)
    }
}

/// A rule that competes with the rules of the scopes its selector matches.
struct Injection {
    matcher: Matcher,
    /// `-1` for `L:` (wins a tie with the grammar's own rules), `1` for `R:`, else 0.
    priority: i8,
    rule: RuleId,
}

/// The most injections one grammar may have (selector alternatives count one each).
pub const MAX_INJECTIONS: usize = 256;

/// A grammar ready to tokenize.
pub struct Grammar {
    rules: Vec<Rule>,
    injections: Vec<Injection>,
    root: RuleId,
    root_scope: Scope,
    /// End and while patterns resolved against a begin match, by source.
    dynamic: Mutex<HashMap<String, Arc<Pattern>>>,
    problems: Vec<String>,
}

/// One token of a line: a byte range and the scopes it carries, outermost first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub scopes: Vec<Scope>,
}

/// The result of tokenizing one line, and where the next line starts.
#[derive(Debug, Clone)]
pub struct LineTokens {
    pub tokens: Vec<Token>,
    pub end_state: StateStack,
    /// Highlighting stopped on this line: it was longer than [`MAX_LINE_BYTES`] (one token, the state
    /// unchanged), or [`MAX_STACK_DEPTH`] rules were open and another would have been entered (the rest of
    /// the line keeps the scopes in force).
    pub skipped: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    rule: RuleId,
    name_scopes: Vec<Scope>,
    content_scopes: Vec<Scope>,
    /// The end (or while) pattern after its back references were filled in, when it has any.
    end: Option<String>,
    /// Where on this line the rule was entered (`None` at the start of a line).
    enter_pos: Option<usize>,
    /// Where `\G` matches for the rule's patterns (`None` at the start of a line).
    anchor_pos: Option<usize>,
    /// The begin match reached the end of its line.
    begin_captured_eol: bool,
}

/// The tokenizer's state between lines: the stack of rules entered and not yet left. Two lines that end
/// in equal states tokenize the lines after them alike, which is what incremental tokenizing relies on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateStack {
    frames: Vec<Frame>,
}

impl StateStack {
    /// How many rules are open.
    pub fn depth(&self) -> usize {
        self.frames.len()
    }
}

const END_RULE: RuleId = usize::MAX;

struct Found {
    rule: RuleId,
    caps: Vec<Option<(usize, usize)>>,
}

struct Injected {
    found: Found,
    /// The injection was declared `L:`.
    priority_match: bool,
}

// ---- compiling ----------------------------------------------------------------------------------------

struct Builder<'a> {
    set: &'a GrammarSet,
    rules: Vec<Option<Rule>>,
    roots: HashMap<String, RuleId>,
    repo: HashMap<(String, String), RuleId>,
    /// Rules compiled from a rule-local `repository` entry, by the entry's address, so an entry that
    /// includes itself ends instead of compiling forever.
    locals: HashMap<*const RawRule, RuleId>,
    problems: Vec<String>,
    base_scope: String,
    too_many: bool,
}

struct Ctx<'a> {
    grammar: &'a str,
    locals: Vec<&'a BTreeMap<String, RawRule>>,
}

impl<'a> Builder<'a> {
    fn alloc(&mut self) -> RuleId {
        if self.rules.len() >= MAX_COMPILED_RULES {
            self.too_many = true;
        }
        self.rules.push(None);
        self.rules.len() - 1
    }

    fn put(&mut self, id: RuleId, rule: Rule) {
        self.rules[id] = Some(rule);
    }

    fn group(&mut self, patterns: Vec<RuleId>) -> RuleId {
        let id = self.alloc();
        self.put(id, Rule { name: None, content_name: None, kind: Kind::Group { patterns }, flat: OnceLock::new() });
        id
    }

    fn root_of(&mut self, scope: &str) -> Option<RuleId> {
        if let Some(id) = self.roots.get(scope) {
            return Some(*id);
        }
        let set = self.set;
        let grammar = set.grammars.get(scope)?;
        let id = self.alloc();
        self.roots.insert(scope.to_owned(), id);
        let ctx = Ctx { grammar: &grammar.scope_name, locals: Vec::new() };
        let patterns = self.patterns(&grammar.patterns, &ctx);
        self.put(id, Rule { name: None, content_name: None, kind: Kind::Group { patterns }, flat: OnceLock::new() });
        Some(id)
    }

    fn repo_rule(&mut self, grammar: &str, name: &str) -> Option<RuleId> {
        let key = (grammar.to_owned(), name.to_owned());
        if let Some(id) = self.repo.get(&key) {
            return Some(*id);
        }
        let set = self.set;
        let g = set.grammars.get(grammar)?;
        let raw = g.repository.get(name)?;
        let id = self.alloc();
        self.repo.insert(key, id);
        let ctx = Ctx { grammar: &g.scope_name, locals: Vec::new() };
        self.fill(id, raw, &ctx);
        Some(id)
    }

    fn patterns(&mut self, list: &[RawRule], ctx: &Ctx) -> Vec<RuleId> {
        let mut out = Vec::new();
        for raw in list {
            if raw.disabled {
                continue;
            }
            if let Some(id) = self.rule(raw, ctx) {
                out.push(id);
            }
        }
        out
    }

    /// The rule a raw rule stands for: an `include` stands for its target, anything else for a new rule.
    fn rule(&mut self, raw: &RawRule, ctx: &Ctx) -> Option<RuleId> {
        if self.too_many {
            return None;
        }
        if let Some(include) = &raw.include {
            return self.include(include, ctx);
        }
        let id = self.alloc();
        self.fill(id, raw, ctx);
        Some(id)
    }

    fn include(&mut self, include: &str, ctx: &Ctx) -> Option<RuleId> {
        if include == "$self" {
            return self.root_of(ctx.grammar);
        }
        if include == "$base" {
            let base = self.base_scope.clone();
            return self.root_of(&base);
        }
        if let Some(name) = include.strip_prefix('#') {
            for repo in ctx.locals.iter().rev() {
                if let Some(raw) = repo.get(name) {
                    let key: *const RawRule = raw;
                    if let Some(id) = self.locals.get(&key) {
                        return Some(*id);
                    }
                    let id = self.alloc();
                    self.locals.insert(key, id);
                    self.fill(id, raw, ctx);
                    return Some(id);
                }
            }
            return self.repo_rule(ctx.grammar, name).or_else(|| {
                self.problems.push(format!("{}: no repository entry {name}", ctx.grammar));
                None
            });
        }
        match include.split_once('#') {
            Some((scope, name)) => self.repo_rule(scope, name),
            None => self.root_of(include),
        }
    }

    fn pattern(&mut self, source: &Option<String>, what: &str, ctx: &Ctx) -> Option<Pattern> {
        let source = source.as_ref()?;
        let p = Pattern::new(source.clone());
        if p.regex(true, true).is_none() && !has_backref(source) {
            self.problems.push(format!("{}: invalid {what} pattern {}", ctx.grammar, truncate(source)));
            return None;
        }
        Some(p)
    }

    fn captures(&mut self, map: &BTreeMap<String, RawRule>, ctx: &Ctx) -> Vec<Capture> {
        let mut out = Vec::new();
        for (key, raw) in map {
            let Ok(index) = key.parse::<usize>() else { continue };
            let retokenize = if raw.patterns.is_empty() {
                None
            } else {
                let patterns = self.patterns(&raw.patterns, ctx);
                Some(self.group(patterns))
            };
            out.push(Capture { index, name: raw.name.as_deref().map(Name::new), content_name: raw.content_name.as_deref().map(Name::new), retokenize });
        }
        out.sort_by_key(|c| c.index);
        out
    }

    fn fill(&mut self, id: RuleId, raw: &RawRule, outer: &Ctx) {
        let mut locals = outer.locals.clone();
        if !raw.repository.is_empty() {
            locals.push(&raw.repository);
        }
        let ctx = Ctx { grammar: outer.grammar, locals };
        let name = raw.name.as_deref().map(Name::new);
        let content_name = raw.content_name.as_deref().map(Name::new);
        let kind = if raw.match_.is_some() {
            let pattern = self.pattern(&raw.match_, "match", &ctx);
            let source = raw.captures.clone();
            Kind::Match { pattern, captures: self.captures(&source, &ctx) }
        } else if raw.begin.is_some() && raw.while_.is_some() && raw.end.is_none() {
            let begin = self.pattern(&raw.begin, "begin", &ctx);
            let while_ = raw.while_.as_ref().map(|s| Pattern::new(s.clone()));
            let begin_caps = if raw.begin_captures.is_empty() { &raw.captures } else { &raw.begin_captures };
            let while_caps = if raw.while_captures.is_empty() { &raw.captures } else { &raw.while_captures };
            let (b, w) = (begin_caps.clone(), while_caps.clone());
            Kind::BeginWhile {
                begin,
                while_has_backrefs: raw.while_.as_deref().is_some_and(has_backref),
                while_,
                begin_captures: self.captures(&b, &ctx),
                while_captures: self.captures(&w, &ctx),
                patterns: self.patterns(&raw.patterns, &ctx),
            }
        } else if raw.begin.is_some() {
            let begin = self.pattern(&raw.begin, "begin", &ctx);
            let end_source = raw.end.clone().unwrap_or_else(|| "\u{FFFF}".to_owned());
            let end = Some(Pattern::new(end_source.clone()));
            let begin_caps = if raw.begin_captures.is_empty() { &raw.captures } else { &raw.begin_captures };
            let end_caps = if raw.end_captures.is_empty() { &raw.captures } else { &raw.end_captures };
            let (b, e) = (begin_caps.clone(), end_caps.clone());
            Kind::BeginEnd {
                begin,
                end,
                end_has_backrefs: has_backref(&end_source),
                begin_captures: self.captures(&b, &ctx),
                end_captures: self.captures(&e, &ctx),
                patterns: self.patterns(&raw.patterns, &ctx),
                apply_end_last: raw.apply_end_pattern_last,
            }
        } else {
            Kind::Group { patterns: self.patterns(&raw.patterns, &ctx) }
        };
        self.put(id, Rule { name, content_name, kind, flat: OnceLock::new() });
    }
}

fn has_backref(source: &str) -> bool {
    let mut chars = source.chars();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.next().is_some_and(|d| d.is_ascii_digit()) {
            return true;
        }
    }
    false
}

fn truncate(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(80).collect()
}

impl Grammar {
    /// Compiles the grammar whose scope is `root` (with everything it includes from `set`). `None` when
    /// `set` has no such grammar. Patterns that Oniguruma rejects are left out and listed in
    /// [`Grammar::problems`].
    pub fn new(set: &GrammarSet, root: &str) -> Option<Self> {
        let mut builder = Builder {
            set,
            rules: Vec::new(),
            roots: HashMap::new(),
            repo: HashMap::new(),
            locals: HashMap::new(),
            problems: Vec::new(),
            base_scope: root.to_owned(),
            too_many: false,
        };
        let root_id = builder.root_of(root)?;
        let mut injections: Vec<Injection> = Vec::new();
        {
            let root_grammar = set.grammars.get(root)?;
            let ctx = Ctx { grammar: &root_grammar.scope_name, locals: Vec::new() };
            // The root grammar's own `injections`: a selector for each rule.
            for (selector, raw) in &root_grammar.injections {
                let Some(parsed) = Selector::parse(selector) else {
                    builder.problems.push(format!("{root}: injection selector over the limits ({})", truncate(selector)));
                    continue;
                };
                let Some(rule) = builder.rule(raw, &ctx) else { continue };
                for (matcher, priority) in parsed.alternatives {
                    if injections.len() < MAX_INJECTIONS {
                        injections.push(Injection { matcher, priority, rule });
                    }
                }
            }
            // Grammars injected into the root: their `injectionSelector` and their patterns.
            for injector in set.injectors.get(root).into_iter().flatten() {
                let Some(grammar) = set.grammars.get(injector) else { continue };
                let Some(selector) = grammar.injection_selector.as_deref() else { continue };
                let Some(parsed) = Selector::parse(selector) else {
                    builder.problems.push(format!("{injector}: injection selector over the limits ({})", truncate(selector)));
                    continue;
                };
                let Some(rule) = builder.root_of(injector) else { continue };
                for (matcher, priority) in parsed.alternatives {
                    if injections.len() < MAX_INJECTIONS {
                        injections.push(Injection { matcher, priority, rule });
                    }
                }
            }
        }
        // L: before the grammar's own rules, R: after (a stable sort keeps the declared order within a priority).
        injections.sort_by_key(|i| i.priority);
        if builder.too_many {
            builder.problems.push(format!("{root}: more than {MAX_COMPILED_RULES} rules; the rest were left out"));
        }
        let rules = builder
            .rules
            .into_iter()
            .map(|r| r.unwrap_or(Rule { name: None, content_name: None, kind: Kind::Group { patterns: Vec::new() }, flat: OnceLock::new() }))
            .collect();
        Some(Self { rules, injections, root: root_id, root_scope: Arc::from(root), dynamic: Mutex::new(HashMap::new()), problems: builder.problems })
    }

    /// What could not be compiled, one sentence each.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    fn initial_state(&self) -> StateStack {
        StateStack {
            frames: vec![Frame {
                rule: self.root,
                name_scopes: vec![self.root_scope.clone()],
                content_scopes: vec![self.root_scope.clone()],
                end: None,
                enter_pos: None,
                anchor_pos: None,
                begin_captured_eol: false,
            }],
        }
    }

    fn flat(&self, id: RuleId) -> &[RuleId] {
        self.rules[id].flat.get_or_init(|| {
            let mut out = Vec::new();
            let mut seen = HashSet::new();
            match &self.rules[id].kind {
                Kind::Group { .. } => self.collect(id, &mut out, &mut seen),
                Kind::BeginEnd { patterns, .. } | Kind::BeginWhile { patterns, .. } => {
                    for p in patterns {
                        self.collect(*p, &mut out, &mut seen);
                    }
                }
                Kind::Match { .. } => {}
            }
            out
        })
    }

    fn collect(&self, id: RuleId, out: &mut Vec<RuleId>, seen: &mut HashSet<RuleId>) {
        if !seen.insert(id) {
            return;
        }
        match &self.rules[id].kind {
            Kind::Match { pattern, .. } => {
                if pattern.is_some() {
                    out.push(id);
                }
            }
            Kind::BeginEnd { begin, .. } | Kind::BeginWhile { begin, .. } => {
                if begin.is_some() {
                    out.push(id);
                }
            }
            Kind::Group { patterns } => {
                for p in patterns {
                    self.collect(*p, out, seen);
                }
            }
        }
    }

    /// The pattern for an end or while source that was filled in from a begin match, and whether it is shared
    /// (kept in the cache: the same `Arc` comes back for the same source, so its address stays valid). A
    /// long source, or a full cache, gives a pattern for one use.
    fn dynamic_pattern(&self, source: &str) -> (Arc<Pattern>, bool) {
        if source.len() > 1024 {
            return (Arc::new(Pattern::new(source.to_owned())), false);
        }
        let mut cache = self.dynamic.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = cache.get(source) {
            return (p.clone(), true);
        }
        if cache.len() >= 4096 {
            return (Arc::new(Pattern::new(source.to_owned())), false);
        }
        let p = Arc::new(Pattern::new(source.to_owned()));
        cache.insert(source.to_owned(), p.clone());
        (p, true)
    }

    /// Forgets the cached dynamic patterns when there are many: done between lines, never while a line is
    /// being tokenized, so an address handed out during a line stays valid for it.
    fn trim_dynamic(&self) {
        let mut cache = self.dynamic.lock().unwrap_or_else(|e| e.into_inner());
        if cache.len() >= 4096 {
            cache.clear();
        }
    }

    // ---- tokenizing -----------------------------------------------------------------------------------

    /// Tokenizes one line (without its line break) from the state the previous line ended in; `None`
    /// for the first line of a document, where `\A` matches.
    pub fn tokenize_line(&self, line: &str, prev: Option<&StateStack>) -> LineTokens {
        let mut text = String::with_capacity(line.len() + 1);
        text.push_str(line);
        text.push('\n');
        let first_line = prev.is_none();
        let mut stack = prev.cloned().unwrap_or_else(|| self.initial_state());
        if line.len() > MAX_LINE_BYTES {
            let scopes = stack.frames.last().map(|f| f.content_scopes.clone()).unwrap_or_default();
            let tokens = if line.is_empty() { Vec::new() } else { vec![Token { start: 0, end: line.len(), scopes }] };
            return LineTokens { tokens, end_state: stack, skipped: true };
        }
        for f in &mut stack.frames {
            f.enter_pos = None;
            f.anchor_pos = None;
        }
        let mut out = Out { tokens: Vec::new(), last_end: 0, stack_limit_hit: false };
        let mut guard = Guard { steps: 0, limit: 20 * (text.len() + 1) + 2000 };
        self.trim_dynamic();
        let mut cache = SearchCache::new();
        self.tokenize_string(&text, first_line, 0, &mut stack, &mut out, true, &mut guard, &mut cache);
        let out_limit = out.stack_limit_hit;
        // The appended line break is not part of the line.
        let len = line.len();
        let mut tokens: Vec<Token> = Vec::with_capacity(out.tokens.len());
        for (start, end, scopes) in out.tokens {
            let (start, end) = (start.min(len), end.min(len));
            if start < end {
                tokens.push(Token { start, end, scopes });
            }
        }
        LineTokens { tokens, end_state: stack, skipped: out_limit }
    }

    #[allow(clippy::too_many_arguments)]
    fn tokenize_string(
        &self,
        text: &str,
        mut is_first_line: bool,
        mut pos: usize,
        stack: &mut StateStack,
        out: &mut Out,
        check_while: bool,
        guard: &mut Guard,
        cache: &mut SearchCache,
    ) {
        let len = text.len();
        let mut anchor: Option<usize> = None;
        if check_while {
            let (new_pos, new_first, new_anchor) = self.check_while(text, is_first_line, pos, stack, out);
            pos = new_pos;
            is_first_line = new_first;
            anchor = new_anchor;
        }
        loop {
            guard.steps += 1;
            if guard.steps > guard.limit {
                out.produce(stack, len);
                return;
            }
            let Some(found) = self.match_rule_or_injections(text, is_first_line, pos, stack, anchor, cache) else {
                out.produce(stack, len);
                return;
            };
            let caps = &found.caps;
            let (m_start, m_end) = caps[0].expect("a match has a whole-match range");
            let advanced = m_end > pos;
            if found.rule == END_RULE {
                let top = stack.frames.last().expect("a frame").clone();
                out.produce(stack, m_start);
                let top_mut = stack.frames.last_mut().expect("a frame");
                top_mut.content_scopes = top.name_scopes.clone();
                let end_caps = match &self.rules[top.rule].kind {
                    Kind::BeginEnd { end_captures, .. } => end_captures.as_slice(),
                    _ => &[],
                };
                self.handle_captures(text, is_first_line, stack, out, end_caps, caps, guard);
                out.produce(stack, m_end);
                let popped = stack.frames.pop().expect("a frame");
                anchor = popped.anchor_pos;
                if !advanced && popped.enter_pos == Some(pos) {
                    // The grammar entered and left this rule without moving: stop on this line.
                    stack.frames.push(popped);
                    out.produce(stack, len);
                    return;
                }
            } else {
                let rule = &self.rules[found.rule];
                if stack.frames.len() >= MAX_STACK_DEPTH && !matches!(rule.kind, Kind::Match { .. }) {
                    // Too many rules open: do not open another. The rest of the line keeps the scopes in force
                    // and the line is reported as skipped, so a caller can show that highlighting stopped.
                    out.produce(stack, len);
                    out.stack_limit_hit = true;
                    return;
                }
                out.produce(stack, m_start);
                let before = stack.frames.last().expect("a frame").clone();
                let name_scopes = push_scopes(&before.content_scopes, rule.name.as_ref().map(|n| expand_name(n, text, caps)));
                stack.frames.push(Frame {
                    rule: found.rule,
                    name_scopes: name_scopes.clone(),
                    content_scopes: name_scopes.clone(),
                    end: None,
                    enter_pos: Some(pos),
                    anchor_pos: anchor,
                    begin_captured_eol: m_end == len,
                });
                match &rule.kind {
                    Kind::BeginEnd { begin_captures, end, end_has_backrefs, .. } => {
                        self.handle_captures(text, is_first_line, stack, out, begin_captures, caps, guard);
                        out.produce(stack, m_end);
                        anchor = Some(m_end);
                        let content = push_scopes(&name_scopes, rule.content_name.as_ref().map(|n| expand_name(n, text, caps)));
                        let top = stack.frames.last_mut().expect("a frame");
                        top.content_scopes = content;
                        if *end_has_backrefs {
                            if let Some(p) = end {
                                top.end = Some(resolve_backrefs(&p.source, text, caps));
                            }
                        }
                        if !advanced && has_same_rule(&stack.frames) {
                            stack.frames.pop();
                            out.produce(stack, len);
                            return;
                        }
                    }
                    Kind::BeginWhile { begin_captures, while_, while_has_backrefs, .. } => {
                        self.handle_captures(text, is_first_line, stack, out, begin_captures, caps, guard);
                        out.produce(stack, m_end);
                        anchor = Some(m_end);
                        let content = push_scopes(&name_scopes, rule.content_name.as_ref().map(|n| expand_name(n, text, caps)));
                        let top = stack.frames.last_mut().expect("a frame");
                        top.content_scopes = content;
                        if *while_has_backrefs {
                            if let Some(p) = while_ {
                                top.end = Some(resolve_backrefs(&p.source, text, caps));
                            }
                        }
                        if !advanced && has_same_rule(&stack.frames) {
                            stack.frames.pop();
                            out.produce(stack, len);
                            return;
                        }
                    }
                    Kind::Match { captures, .. } => {
                        self.handle_captures(text, is_first_line, stack, out, captures, caps, guard);
                        out.produce(stack, m_end);
                        stack.frames.pop();
                        if !advanced {
                            // A match that consumed nothing and did not move: leave the line to avoid a loop.
                            if stack.frames.len() > 1 {
                                stack.frames.pop();
                            }
                            out.produce(stack, len);
                            return;
                        }
                    }
                    Kind::Group { .. } => {}
                }
            }
            if m_end > pos {
                pos = m_end;
                is_first_line = false;
            }
        }
    }

    /// The grammar's own best match, or an injected rule's when that starts earlier (or at the same place with `L:`).
    #[allow(clippy::too_many_arguments)]
    fn match_rule_or_injections(&self, text: &str, is_first_line: bool, pos: usize, stack: &StateStack, anchor: Option<usize>, cache: &mut SearchCache) -> Option<Found> {
        let normal = self.match_rule(text, is_first_line, pos, stack, anchor, cache);
        if self.injections.is_empty() {
            return normal;
        }
        let Some(injected) = self.match_injections(text, is_first_line, pos, stack, anchor, cache) else {
            return normal;
        };
        let Some(normal) = normal else { return Some(injected.found) };
        let normal_start = normal.caps[0].map_or(usize::MAX, |c| c.0);
        let injected_start = injected.found.caps[0].map_or(usize::MAX, |c| c.0);
        if injected_start < normal_start || (injected.priority_match && injected_start == normal_start) {
            Some(injected.found)
        } else {
            Some(normal)
        }
    }

    /// The earliest match among the injections whose selector matches the scopes in force.
    #[allow(clippy::too_many_arguments)]
    fn match_injections(&self, text: &str, is_first_line: bool, pos: usize, stack: &StateStack, anchor: Option<usize>, cache: &mut SearchCache) -> Option<Injected> {
        let frame = stack.frames.last()?;
        let allow = (is_first_line && pos == 0, anchor == Some(pos));
        let mut best: Option<(usize, Injected)> = None;
        for injection in &self.injections {
            if !injection.matcher.matches(&frame.content_scopes) {
                continue;
            }
            let mut local: Option<Best> = None;
            for (order, &id) in self.flat(injection.rule).iter().enumerate() {
                let pattern = match &self.rules[id].kind {
                    Kind::Match { pattern, .. } => pattern.as_ref(),
                    Kind::BeginEnd { begin, .. } | Kind::BeginWhile { begin, .. } => begin.as_ref(),
                    Kind::Group { .. } => None,
                };
                if let Some(p) = pattern {
                    consider(cache, text, pos, allow, p, true, id, order, &mut local);
                }
            }
            let Some(found) = local else { continue };
            let Some(region) = cache.get(&found.key).and_then(|e| e.hit.as_ref()) else { continue };
            let caps: Vec<Option<(usize, usize)>> = (0..region.len()).map(|i| region.pos(i)).collect();
            let start = found.start;
            if best.as_ref().is_some_and(|(s, _)| start >= *s) {
                continue;
            }
            best = Some((start, Injected { found: Found { rule: found.rule, caps }, priority_match: injection.priority == -1 }));
            if start == pos {
                break;
            }
        }
        best.map(|(_, i)| i)
    }

    /// The best match of the rule on top of the stack at `pos`: the earliest start, the first pattern
    /// among equals. The rule's own end pattern comes first unless `applyEndPatternLast` says otherwise.
    #[allow(clippy::too_many_arguments)]
    fn match_rule(&self, text: &str, is_first_line: bool, pos: usize, stack: &StateStack, anchor: Option<usize>, cache: &mut SearchCache) -> Option<Found> {
        let frame = stack.frames.last()?;
        let rule = &self.rules[frame.rule];
        let allow = (is_first_line && pos == 0, anchor == Some(pos));
        let mut best: Option<Best> = None;
        let mut order = 0usize;
        let dynamic_end = frame.end.as_deref().map(|source| self.dynamic_pattern(source));
        let (end_pattern, end_last): (Option<(&Pattern, bool)>, bool) = match &rule.kind {
            Kind::BeginEnd { end, apply_end_last, .. } => {
                let found = match &dynamic_end {
                    Some((p, shared)) => Some((p.as_ref(), *shared)),
                    None => end.as_ref().map(|p| (p, true)),
                };
                (found, *apply_end_last)
            }
            _ => (None, false),
        };
        if let (Some((p, shared)), false) = (end_pattern, end_last) {
            consider(cache, text, pos, allow, p, shared, END_RULE, order, &mut best);
            order += 1;
        }
        for &id in self.flat(frame.rule) {
            let pattern = match &self.rules[id].kind {
                Kind::Match { pattern, .. } => pattern.as_ref(),
                Kind::BeginEnd { begin, .. } | Kind::BeginWhile { begin, .. } => begin.as_ref(),
                Kind::Group { .. } => None,
            };
            if let Some(p) = pattern {
                consider(cache, text, pos, allow, p, true, id, order, &mut best);
            }
            order += 1;
        }
        if let (Some((p, shared)), true) = (end_pattern, end_last) {
            consider(cache, text, pos, allow, p, shared, END_RULE, order, &mut best);
        }
        let best = best?;
        let region = cache.get(&best.key).and_then(|e| e.hit.as_ref())?;
        Some(Found { rule: best.rule, caps: (0..region.len()).map(|i| region.pos(i)).collect() })
    }

    /// `begin`/`while` rules on the stack must match at the start of each line, from the outermost in; the
    /// first that does not match ends itself and everything inside it.
    fn check_while(&self, text: &str, mut is_first_line: bool, mut pos: usize, stack: &mut StateStack, out: &mut Out) -> (usize, bool, Option<usize>) {
        let mut anchor: Option<usize> = None;
        if stack.frames.last().is_some_and(|f| f.begin_captured_eol) {
            anchor = Some(0);
        }
        let while_frames: Vec<usize> = stack
            .frames
            .iter()
            .enumerate()
            .filter(|(_, f)| matches!(self.rules[f.rule].kind, Kind::BeginWhile { .. }))
            .map(|(i, _)| i)
            .collect();
        for index in while_frames {
            let frame = stack.frames[index].clone();
            let Kind::BeginWhile { while_, while_captures, .. } = &self.rules[frame.rule].kind else { continue };
            let pattern = match (&frame.end, while_) {
                (Some(source), _) => Some(self.dynamic_pattern(source).0),
                (None, Some(p)) => Some(self.dynamic_pattern(&p.source).0),
                _ => None,
            };
            let allow_a = is_first_line && pos == 0;
            let allow_g = anchor == Some(pos);
            let mut region = Region::new();
            let matched = pattern
                .as_ref()
                .and_then(|p| p.regex(allow_a, allow_g))
                .and_then(|re| search(re, text, pos, &mut region))
                .is_some();
            if matched {
                let caps: Vec<Option<(usize, usize)>> = (0..region.len()).map(|i| region.pos(i)).collect();
                let (m_start, m_end) = caps[0].expect("whole match");
                let mut view = StateStack { frames: stack.frames[..=index].to_vec() };
                out.produce(&view, m_start);
                let mut guard = Guard { steps: 0, limit: usize::MAX };
                self.handle_captures(text, is_first_line, &mut view, out, while_captures, &caps, &mut guard);
                out.produce(&view, m_end);
                anchor = Some(m_end);
                if m_end > pos {
                    pos = m_end;
                    is_first_line = false;
                }
            } else {
                stack.frames.truncate(index);
                break;
            }
        }
        (pos, is_first_line, anchor)
    }

    /// Scopes the pieces of a match that have captures, and tokenizes again the ones that have patterns.
    #[allow(clippy::too_many_arguments)]
    fn handle_captures(
        &self,
        text: &str,
        is_first_line: bool,
        stack: &mut StateStack,
        out: &mut Out,
        captures: &[Capture],
        caps: &[Option<(usize, usize)>],
        guard: &mut Guard,
    ) {
        if captures.is_empty() {
            return;
        }
        let max_end = caps[0].map(|c| c.1).unwrap_or(0);
        let mut local: Vec<(Vec<Scope>, usize)> = Vec::new();
        for capture in captures {
            let Some(Some((start, end))) = caps.get(capture.index).copied() else { continue };
            if start == end {
                continue;
            }
            if start > max_end {
                break;
            }
            while let Some((scopes, end_at)) = local.last().cloned() {
                if end_at <= start {
                    out.produce_scopes(&scopes, end_at);
                    local.pop();
                } else {
                    break;
                }
            }
            match local.last() {
                Some((scopes, _)) => out.produce_scopes(scopes, start),
                None => out.produce(stack, start),
            }
            if let Some(group) = capture.retokenize {
                let base = stack.frames.last().expect("a frame").content_scopes.clone();
                let name_scopes = push_scopes(&base, capture.name.as_ref().map(|n| expand_name(n, text, caps)));
                let content = push_scopes(&name_scopes, capture.content_name.as_ref().map(|n| expand_name(n, text, caps)));
                let mut inner = stack.clone();
                inner.frames.push(Frame {
                    rule: group,
                    name_scopes,
                    content_scopes: content,
                    end: None,
                    enter_pos: Some(start),
                    anchor_pos: None,
                    begin_captured_eol: false,
                });
                // A shorter text is a different search: the results kept for the whole line do not apply to it.
                let mut sub_cache = SearchCache::new();
                self.tokenize_string(&text[..end], is_first_line && start == 0, start, &mut inner, out, false, guard, &mut sub_cache);
                continue;
            }
            if let Some(name) = &capture.name {
                let base = match local.last() {
                    Some((scopes, _)) => scopes.clone(),
                    None => stack.frames.last().expect("a frame").content_scopes.clone(),
                };
                local.push((push_scopes(&base, Some(expand_name(name, text, caps))), end));
            }
        }
        while let Some((scopes, end_at)) = local.pop() {
            out.produce_scopes(&scopes, end_at);
        }
    }
}

struct Guard {
    steps: usize,
    limit: usize,
}

/// A search result kept for the rest of the line: the position it was made from and what it found (`None`:
/// nothing from there on). Searching again from a later position finds the same match as long as the match
/// starts at or after it, so most patterns are searched once per line instead of once per token.
struct CacheEntry {
    from: usize,
    hit: Option<Region>,
}

/// By the address of the compiled regular expression.
type SearchCache = HashMap<usize, CacheEntry>;

struct Best {
    start: usize,
    order: usize,
    rule: RuleId,
    key: usize,
}

/// Runs one candidate pattern (or reuses its cached result) and keeps it if it beats `best`.
#[allow(clippy::too_many_arguments)]
fn consider(
    cache: &mut SearchCache,
    text: &str,
    pos: usize,
    (allow_a, allow_g): (bool, bool),
    pattern: &Pattern,
    shared: bool,
    rule: RuleId,
    order: usize,
    best: &mut Option<Best>,
) {
    let Some(regex) = pattern.regex(allow_a, allow_g) else { return };
    let key = regex as *const Regex as usize;
    // `\G` matches where the search starts, so a result for it is only good for that start.
    let cacheable = shared && !(pattern.uses_g && allow_g);
    let valid = cacheable
        && cache
            .get(&key)
            .is_some_and(|e| e.from <= pos && e.hit.as_ref().is_none_or(|r| r.pos(0).is_some_and(|(start, _)| start >= pos)));
    if !valid {
        let mut region = Region::new();
        let hit = search(regex, text, pos, &mut region).map(|_| region);
        cache.insert(key, CacheEntry { from: pos, hit });
    }
    if let Some(CacheEntry { hit: Some(region), .. }) = cache.get(&key) {
        if let Some((start, _)) = region.pos(0) {
            let better = match best {
                Some(b) => start < b.start || (start == b.start && order < b.order),
                None => true,
            };
            if better {
                *best = Some(Best { start, order, rule, key });
            }
        }
    }
}

struct Out {
    tokens: Vec<(usize, usize, Vec<Scope>)>,
    last_end: usize,
    stack_limit_hit: bool,
}

impl Out {
    fn produce(&mut self, stack: &StateStack, end: usize) {
        let scopes = stack.frames.last().map(|f| f.content_scopes.clone()).unwrap_or_default();
        self.produce_scopes(&scopes, end);
    }

    fn produce_scopes(&mut self, scopes: &[Scope], end: usize) {
        if self.last_end >= end {
            return;
        }
        self.tokens.push((self.last_end, end, scopes.to_vec()));
        self.last_end = end;
    }
}

/// Whether the frame just pushed repeats a rule already open at the same position: entering it again
/// without having moved would never end.
fn has_same_rule(frames: &[Frame]) -> bool {
    let Some((pushed, below)) = frames.split_last() else { return false };
    for f in below.iter().rev() {
        if f.enter_pos != pushed.enter_pos {
            break;
        }
        if f.rule == pushed.rule {
            return true;
        }
    }
    false
}

fn push_scopes(base: &[Scope], name: Option<String>) -> Vec<Scope> {
    let mut out = base.to_vec();
    if let Some(name) = name {
        for part in name.split_whitespace() {
            out.push(Arc::from(part));
        }
    }
    out
}

fn expand_name(name: &Name, text: &str, caps: &[Option<(usize, usize)>]) -> String {
    if !name.capturing {
        return name.text.clone();
    }
    let chars: Vec<char> = name.text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            // $n or ${n:/downcase} / ${n:/upcase}
            let mut j = i + 1;
            let braced = chars.get(j) == Some(&'{');
            if braced {
                j += 1;
            }
            let digits_start = j;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > digits_start {
                let n: usize = chars[digits_start..j].iter().collect::<String>().parse().unwrap_or(usize::MAX);
                let mut transform = String::new();
                let mut ok = true;
                if braced {
                    if chars.get(j) == Some(&':') && chars.get(j + 1) == Some(&'/') {
                        let mut k = j + 2;
                        while k < chars.len() && chars[k] != '}' {
                            transform.push(chars[k]);
                            k += 1;
                        }
                        j = k + 1;
                    } else {
                        ok = false;
                    }
                }
                if ok {
                    let value = caps.get(n).copied().flatten().map(|(s, e)| &text[s..e]).unwrap_or("");
                    let value = value.trim_start_matches('.');
                    match transform.as_str() {
                        "downcase" => out.push_str(&value.to_lowercase()),
                        "upcase" => out.push_str(&value.to_uppercase()),
                        _ => out.push_str(value),
                    }
                    i = j;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// The pattern with `\1`..`\9` replaced by the text the begin match captured, escaped.
fn resolve_backrefs(source: &str, text: &str, caps: &[Option<(usize, usize)>]) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.peek().copied() {
            Some(d) if d.is_ascii_digit() => {
                chars.next();
                let n = d.to_digit(10).unwrap_or(0) as usize;
                let value = caps.get(n).copied().flatten().map(|(s, e)| &text[s..e]).unwrap_or("");
                for ch in value.chars() {
                    if matches!(ch, '-' | '\\' | '^' | '$' | '*' | '+' | '?' | '.' | '(' | ')' | '|' | '[' | ']' | '{' | '}') {
                        out.push('\\');
                    }
                    out.push(ch);
                }
            }
            Some(other) => {
                chars.next();
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}
