//! Syntax highlighting inside the session (FR-E6): which grammar a document needs, loading it, and the
//! background work that keeps a document's coloured runs up to date.
//!
//! Two halves:
//!
//! * [`Highlights`] lives in the session core, which is pure: it decides, from the language of a document
//!   and the grammars the installed extensions contribute, which grammar files to read, and turns the
//!   document's life (opened, edited, scrolled, closed) into [`HighlightJob`]s. It never tokenizes.
//! * [`Worker`] does the tokenizing. It runs on a thread of its own (a first pass over a long file takes a
//!   second) and keeps, per document, a [`DocumentTokens`]; jobs are processed in order, and a batch of
//!   edits to one document gives one answer.
//!
//! The answer to a job is a [`HighlightOutput`]: the runs of the lines around the visible ones, with the
//! document version they were written against. The widget moves runs along with what the reader typed
//! since, so an answer that arrives a frame late still lands on the right text.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::coords::WidgetRange;
use crate::textmate::{parse_grammar, DocRun, DocumentTokens, EditError, Grammar, GrammarCatalog, GrammarSet};

/// How many lines around the visible ones carry colours.
pub const WINDOW_MARGIN: u32 = 150;
/// The most grammar files one language may pull in (the language's own and what it includes).
pub const MAX_GRAMMARS_PER_LANGUAGE: usize = 40;

/// One unit of work for the [`Worker`].
#[derive(Debug, Clone, PartialEq)]
pub enum HighlightJob {
    /// Compile the grammar and tokenize the document from the start.
    Build {
        key: String,
        seq: u64,
        root_scope: String,
        grammars: Vec<(String, String, Arc<str>)>,
        /// Scopes of the grammars injected into the root (from their `injectTo`); their text is in `grammars`.
        injectors: Vec<String>,
        text: String,
        version: u32,
        window: (u32, u32),
    },
    /// The document changed.
    Edit { key: String, seq: u64, range: WidgetRange, text: String, version: u32 },
    /// The visible lines moved: answer with the runs of this window of lines (first, last inclusive).
    Window { key: String, seq: u64, window: (u32, u32) },
    /// The document is closed.
    Drop { key: String },
}

/// What the [`Worker`] found.
#[derive(Debug, Clone, PartialEq)]
pub struct HighlightOutput {
    pub key: String,
    pub seq: u64,
    /// The document version the runs are written against.
    pub version: u32,
    /// The lines the runs cover (first, last inclusive); lines outside it carry no colour.
    pub window: (u32, u32),
    pub runs: Vec<DocRun>,
    pub problem: Option<Problem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The grammar or the document could not be used; there are no colours for it.
    Off(String),
    /// The worker's copy of the document no longer matches the session's: build it again.
    Rebuild,
}

// ---- the worker ---------------------------------------------------------------------------------------

struct WorkDoc {
    seq: u64,
    tokens: DocumentTokens,
    version: u32,
    /// The lines the answers cover (first, last inclusive).
    window: (u32, u32),
}

/// Tokenizes documents. Pure: jobs in, outputs out.
#[derive(Default)]
pub struct Worker {
    grammars: HashMap<String, Arc<Grammar>>,
    docs: HashMap<String, WorkDoc>,
}

impl HighlightJob {
    /// The document and build a job belongs to (a drop has none).
    pub fn target(&self) -> Option<(&str, u64)> {
        match self {
            HighlightJob::Build { key, seq, .. } | HighlightJob::Edit { key, seq, .. } | HighlightJob::Window { key, seq, .. } => Some((key, *seq)),
            HighlightJob::Drop { .. } => None,
        }
    }
}

impl HighlightOutput {
    /// The answer that says "build this document again".
    pub fn rebuild(key: &str, seq: u64) -> Self {
        Self { key: key.to_owned(), seq, version: 0, window: (0, 0), runs: Vec::new(), problem: Some(Problem::Rebuild) }
    }
}

impl Worker {
    pub fn new() -> Self {
        Self::default()
    }

    fn grammar_for(&mut self, root: &str, sources: &[(String, String, Arc<str>)], injectors: &[String]) -> Result<Arc<Grammar>, String> {
        // A language compiled once serves every document of it: the sources are not parsed again.
        if let Some(g) = self.grammars.get(root) {
            return Ok(g.clone());
        }
        let mut set = GrammarSet::new();
        for (scope, path, text) in sources {
            match parse_grammar(path, text) {
                Ok(g) => set.add(g),
                Err(e) => tracing::warn!("ember-editor: grammar {scope} ({path}) not usable: {e}"),
            }
        }
        for injector in injectors {
            set.add_injector(root, injector);
        }
        if !set.contains(root) {
            return Err(format!("the grammar {root} could not be read"));
        }
        let grammar = Grammar::new(&set, root).ok_or_else(|| format!("the grammar {root} could not be compiled"))?;
        for problem in grammar.problems() {
            tracing::debug!("ember-editor: grammar {root}: {problem}");
        }
        let grammar = Arc::new(grammar);
        self.grammars.insert(root.to_owned(), grammar.clone());
        Ok(grammar)
    }

    fn out(doc: &WorkDoc, key: &str) -> HighlightOutput {
        let (first, last) = (doc.window.0 as usize, doc.window.1 as usize + 1);
        HighlightOutput { key: key.to_owned(), seq: doc.seq, version: doc.version, window: doc.window, runs: doc.tokens.runs_between(first, last), problem: None }
    }

    fn problem(key: String, seq: u64, version: u32, problem: Problem) -> HighlightOutput {
        HighlightOutput { key, seq, version, window: (0, 0), runs: Vec::new(), problem: Some(problem) }
    }

    /// Runs a batch of jobs in order. A document that several jobs touched gives one output, for the window
    /// last asked for.
    pub fn run(&mut self, jobs: Vec<HighlightJob>) -> Vec<HighlightOutput> {
        let mut outputs: Vec<HighlightOutput> = Vec::new();
        let mut touched: Vec<String> = Vec::new();
        let touch = |key: &str, touched: &mut Vec<String>| {
            if !touched.iter().any(|k| k == key) {
                touched.push(key.to_owned());
            }
        };
        for job in jobs {
            match job {
                HighlightJob::Build { key, seq, root_scope, grammars, injectors, text, version, window } => {
                    self.docs.remove(&key);
                    touched.retain(|k| *k != key);
                    let built = self
                        .grammar_for(&root_scope, &grammars, &injectors)
                        .and_then(|g| DocumentTokens::new(g, &text).map_err(|_| "the document is too large to colour".to_owned()));
                    match built {
                        Ok(tokens) => {
                            self.docs.insert(key.clone(), WorkDoc { seq, tokens, version, window });
                            touch(&key, &mut touched);
                        }
                        Err(why) => outputs.push(Self::problem(key, seq, version, Problem::Off(why))),
                    }
                }
                HighlightJob::Edit { key, seq, range, text, version } => {
                    let Some(doc) = self.docs.get_mut(&key).filter(|d| d.seq == seq) else { continue };
                    match doc.tokens.apply_edit(range, &text) {
                        Ok(_) => {
                            doc.version = version;
                            touch(&key, &mut touched);
                        }
                        Err(EditError::OutOfRange) => {
                            self.docs.remove(&key);
                            touched.retain(|k| *k != key);
                            outputs.push(Self::problem(key, seq, version, Problem::Rebuild));
                        }
                        Err(EditError::TooLarge) => {
                            self.docs.remove(&key);
                            touched.retain(|k| *k != key);
                            outputs.push(Self::problem(key, seq, version, Problem::Off("the document grew too large to colour".to_owned())));
                        }
                    }
                }
                HighlightJob::Window { key, seq, window } => {
                    if let Some(doc) = self.docs.get_mut(&key).filter(|d| d.seq == seq) {
                        doc.window = window;
                        touch(&key, &mut touched);
                    }
                }
                HighlightJob::Drop { key } => {
                    self.docs.remove(&key);
                    touched.retain(|k| *k != key);
                }
            }
        }
        for key in touched {
            if let Some(doc) = self.docs.get(&key) {
                outputs.push(Self::out(doc, &key));
            }
        }
        outputs
    }
}

/// Runs `run` on a batch and turns a panic in it into "highlighting is off" for every document the batch touched,
/// with a fresh worker afterwards: a grammar or an input that breaks the tokenizer must not take the thread, and
/// with it every later document's colours, down.
pub fn guard(
    worker: &mut Worker,
    jobs: Vec<HighlightJob>,
    run: impl FnOnce(&mut Worker, Vec<HighlightJob>) -> Vec<HighlightOutput>,
) -> Vec<HighlightOutput> {
    let mut keys: Vec<(String, u64)> = Vec::new();
    for job in &jobs {
        let (key, seq) = match job {
            HighlightJob::Build { key, seq, .. } | HighlightJob::Edit { key, seq, .. } | HighlightJob::Window { key, seq, .. } => (key, *seq),
            HighlightJob::Drop { .. } => continue,
        };
        match keys.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = seq,
            None => keys.push((key.clone(), seq)),
        }
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(worker, jobs))) {
        Ok(outputs) => outputs,
        Err(_) => {
            tracing::warn!("ember-editor: the highlighting worker panicked; colours are off for {} document(s)", keys.len());
            *worker = Worker::new();
            keys.into_iter()
                .map(|(key, seq)| HighlightOutput {
                    key,
                    seq,
                    version: 0,
                    window: (0, 0),
                    runs: Vec::new(),
                    problem: Some(Problem::Off("the highlighter stopped on an internal error".to_owned())),
                })
                .collect()
        }
    }
}

// ---- the session side ---------------------------------------------------------------------------------

/// What [`Highlights`] asks the session to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Read this grammar file (the answer comes back through [`Highlights::grammar_read`]).
    Read { scope: String, path: String },
    Job(HighlightJob),
    /// Highlighting is off for the document (reason in a sentence, for the log).
    Off { key: String, why: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    /// Waiting for grammar files.
    Loading,
    /// The worker has (or is building) the document.
    Active,
    Off,
}

struct DocState {
    root: String,
    seq: u64,
    phase: Phase,
    /// The window the worker last answered for or was asked for.
    window: (u32, u32),
}

/// The part of highlighting that lives in the session core.
#[derive(Default)]
pub struct Highlights {
    catalog: GrammarCatalog,
    texts: HashMap<String, Arc<str>>,
    refs: HashMap<String, Vec<String>>,
    requested: HashSet<String>,
    failed: HashSet<String>,
    docs: HashMap<String, DocState>,
    next_seq: u64,
}

impl Highlights {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_catalog(&mut self, catalog: GrammarCatalog) {
        self.catalog = catalog;
    }

    pub fn has_grammar_for(&self, language: &str) -> bool {
        self.catalog.for_language(language).is_some()
    }

    /// The grammar files `root` needs (itself and what it includes), whether each is loaded, and the ones
    /// still to request, up to [`MAX_GRAMMARS_PER_LANGUAGE`].
    fn closure(&self, root: &str) -> (Vec<String>, Vec<String>) {
        let mut order: Vec<String> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        let mut queue = vec![root.to_owned()];
        // The grammars injected into the root are loaded with it (after what it includes, in a fixed order).
        queue.splice(0..0, self.catalog.injections_into(root).iter().rev().map(|e| e.scope_name.clone()));
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(scope) = queue.pop() {
            if order.len() + missing.len() >= MAX_GRAMMARS_PER_LANGUAGE || !seen.insert(scope.clone()) {
                continue;
            }
            if self.catalog.by_scope(&scope).is_none() || self.failed.contains(&scope) {
                continue;
            }
            match self.refs.get(&scope) {
                Some(refs) => {
                    order.push(scope);
                    queue.extend(refs.iter().rev().cloned());
                }
                None => missing.push(scope),
            }
        }
        (order, missing)
    }

    /// Requests whatever `root` still needs; returns the reads and whether it is ready.
    fn need(&mut self, root: &str, actions: &mut Vec<Action>) -> bool {
        let (_, missing) = self.closure(root);
        for scope in &missing {
            if self.requested.insert(scope.clone()) {
                if let Some(entry) = self.catalog.by_scope(scope) {
                    actions.push(Action::Read { scope: scope.clone(), path: entry.path.clone() });
                }
            }
        }
        // Ready when nothing is missing, or when everything missing was already asked for and failed.
        missing.is_empty()
    }

    /// A document was opened or reset. Returns the reads it needs and whether its grammars are all there
    /// already (then [`Highlights::build_job`] gives the work to do).
    pub fn open(&mut self, key: &str, language: &str) -> (Vec<Action>, bool) {
        self.docs.remove(key);
        let mut actions = Vec::new();
        let Some(root) = self.catalog.for_language(language).map(|e| e.scope_name.clone()) else {
            return (actions, false);
        };
        // A grammar that failed to read is tried again when a document of a language opens: the server may have
        // been busy, and one failure should not leave a language uncoloured for the whole session.
        for scope in self.failed.drain() {
            self.requested.remove(&scope);
        }
        self.next_seq += 1;
        let ready = self.need(&root, &mut actions);
        let phase = if ready { Phase::Active } else { Phase::Loading };
        self.docs.insert(key.to_owned(), DocState { root, seq: self.next_seq, phase, window: (0, WINDOW_MARGIN * 2) });
        (actions, ready)
    }

    /// The build job for a document that is ready, from its current text and version.
    pub fn build_job(&mut self, key: &str, text: String, version: u32) -> Option<HighlightJob> {
        let doc = self.docs.get(key)?;
        if doc.phase != Phase::Active {
            return None;
        }
        let (order, _) = self.closure(&doc.root);
        let grammars: Vec<(String, String, Arc<str>)> = order
            .iter()
            .filter_map(|scope| {
                let path = self.catalog.by_scope(scope)?.path.clone();
                Some((scope.clone(), path, self.texts.get(scope)?.clone()))
            })
            .collect();
        let injectors: Vec<String> = self.catalog.injections_into(&doc.root).iter().map(|e| e.scope_name.clone()).collect();
        Some(HighlightJob::Build { key: key.to_owned(), seq: doc.seq, root_scope: doc.root.clone(), grammars, injectors, text, version, window: doc.window })
    }

    /// A grammar file arrived (or failed). Returns the reads it leads to and the documents that are now ready
    /// to be built.
    pub fn grammar_read(&mut self, scope: &str, path: &str, result: Result<String, String>) -> (Vec<Action>, Vec<String>) {
        let mut actions = Vec::new();
        match result {
            Ok(text) => match parse_grammar(path, &text) {
                Ok(raw) => {
                    self.refs.insert(scope.to_owned(), raw.referenced_scopes());
                    self.texts.insert(scope.to_owned(), Arc::from(text));
                }
                Err(e) => {
                    tracing::warn!("ember-editor: grammar {scope} ({path}) not usable: {e}");
                    self.failed.insert(scope.to_owned());
                }
            },
            Err(e) => {
                tracing::warn!("ember-editor: grammar {scope} ({path}) not read: {e}");
                self.failed.insert(scope.to_owned());
            }
        }
        let loading: Vec<(String, String)> =
            self.docs.iter().filter(|(_, d)| d.phase == Phase::Loading).map(|(k, d)| (k.clone(), d.root.clone())).collect();
        let mut ready = Vec::new();
        for (key, root) in loading {
            if self.failed.contains(&root) {
                if let Some(d) = self.docs.get_mut(&key) {
                    d.phase = Phase::Off;
                }
                actions.push(Action::Off { key, why: format!("the grammar {root} could not be loaded") });
            } else if self.need(&root, &mut actions) {
                if let Some(d) = self.docs.get_mut(&key) {
                    d.phase = Phase::Active;
                }
                ready.push(key);
            }
        }
        (actions, ready)
    }

    /// The document changed.
    pub fn edit(&mut self, key: &str, range: WidgetRange, text: &str, version: u32) -> Option<HighlightJob> {
        let doc = self.docs.get(key)?;
        (doc.phase == Phase::Active).then(|| HighlightJob::Edit { key: key.to_owned(), seq: doc.seq, range, text: text.to_owned(), version })
    }

    /// The visible lines are `first..=last`: a job when the window that carries colours should move.
    pub fn visible(&mut self, key: &str, first: u32, last: u32) -> Option<HighlightJob> {
        let doc = self.docs.get_mut(key)?;
        if doc.phase != Phase::Active {
            return None;
        }
        let wanted = (first.saturating_sub(WINDOW_MARGIN), last.saturating_add(WINDOW_MARGIN));
        // Move when the visible lines come within a third of the margin of the window's edge.
        let slack = WINDOW_MARGIN / 3;
        let covered = doc.window.0 <= first.saturating_sub(slack) && last.saturating_add(slack) <= doc.window.1;
        if covered {
            return None;
        }
        doc.window = wanted;
        Some(HighlightJob::Window { key: key.to_owned(), seq: doc.seq, window: wanted })
    }

    /// The window the document's colours should cover now.
    pub fn window(&self, key: &str) -> Option<(u32, u32)> {
        self.docs.get(key).map(|d| d.window)
    }

    /// An answer from the worker: whether it is current (its document is open and was not rebuilt since).
    pub fn accept(&mut self, output: &HighlightOutput) -> bool {
        self.docs.get(&output.key).is_some_and(|d| d.seq == output.seq)
    }

    /// The worker could not follow the document: build it again (a new `seq`).
    pub fn rebuild(&mut self, key: &str) {
        self.next_seq += 1;
        if let Some(d) = self.docs.get_mut(key) {
            d.seq = self.next_seq;
        }
    }

    pub fn turn_off(&mut self, key: &str) {
        if let Some(d) = self.docs.get_mut(key) {
            d.phase = Phase::Off;
        }
    }

    pub fn close(&mut self, key: &str) -> Option<HighlightJob> {
        self.docs.remove(key).map(|_| HighlightJob::Drop { key: key.to_owned() })
    }
}
