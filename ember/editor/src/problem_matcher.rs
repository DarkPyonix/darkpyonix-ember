//! Problem matchers of tasks (FR-L31.42, #445), as Code-OSS has them.
//!
//! Two files of Code-OSS (`src/vs/workbench/contrib/tasks/common/`) are followed here:
//!
//! - `problemMatcher.ts`: the patterns and matchers Code-OSS ships ([`Registry::builtin`]), the
//!   ones extensions contribute in `problemPatterns` and `problemMatchers`
//!   ([`Registry::with_extensions`]), the parser of a definition with its defaults
//!   ([`Registry::parse_matcher`]), the line matchers and `getResource`.
//! - `problemCollectors.ts`: the start-stop collector of a task that exits and the watching
//!   collector of a background task ([`ProblemCollector`]), which run the lines of a task's
//!   output through the matchers.
//!
//! The regular expressions are JavaScript ones in Code-OSS; Oniguruma compiles them here, as the
//! search does ([`crate::search`]). Nothing here does I/O: a collector returns what Code-OSS tells
//! the marker service ([`CollectorEvent`]), and the window hands that to the session's problems.
//!
//! `fileLocation` `search` (`searchForFileLocation`) looks through a list of the workspace's files
//! the window hands the matcher ([`ProblemMatcher::search_files`]): each `include` folder depth
//! first, a folder's own files before its subfolders, an `exclude` folder skipped with everything
//! below it, and the first file whose path ends with the name wins. Without the list, or when
//! nothing matches, the name is taken as absolute, as Code-OSS does when its search finds nothing.
//!
//! What differs, and why: `fileLocation` `autoDetect` checks no file, because the collector has no
//! file system; a relative name is joined to the prefix and an absolute one is kept, which is
//! what Code-OSS's check finds when the file is there. A folder's entries are searched in name
//! order, where Code-OSS takes the order the file system lists them in. The variables an
//! `endsPattern` captures are not kept, because no task variable reads them yet.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use ember_editor_conn::exthost::MarkerData;
use onig::{Regex, RegexOptions, Syntax};
use serde_json::{Map, Value};

// ---- regular expressions ----------------------------------------------------------------------

/// A compiled regular expression with the source it came from.
#[derive(Clone)]
pub struct Re {
    source: String,
    regex: Arc<Regex>,
}

impl std::fmt::Debug for Re {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "/{}/", self.source)
    }
}

impl PartialEq for Re {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Re {
    /// Fails with Code-OSS's message (`ProblemPatternParser.invalidRegexp`).
    pub fn new(source: &str) -> Result<Self, String> {
        Regex::with_options(source, RegexOptions::REGEX_OPTION_CAPTURE_GROUP, Syntax::default())
            .map(|regex| Self { source: source.to_owned(), regex: Arc::new(regex) })
            .map_err(|_| format!("Error: The string {source} is not a valid regular expression.\n"))
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// `RegExp.exec` without the `g` flag: the first match in `line`, every group in order, `None`
    /// for a group that took no part (JavaScript's `undefined`).
    pub fn exec(&self, line: &str) -> Option<Vec<Option<String>>> {
        let captures = self.regex.captures(line)?;
        Some((0..captures.len()).map(|i| captures.at(i).map(str::to_owned)).collect())
    }
}

fn builtin_re(source: &str) -> Re {
    Re::new(source).unwrap_or_else(|e| panic!("built-in problem pattern: {e}"))
}

// ---- the model --------------------------------------------------------------------------------

/// `ProblemLocationKind`: a pattern finds a location (file and line) or a whole file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationKind {
    File,
    Location,
}

impl LocationKind {
    fn from_name(value: &str) -> Option<Self> {
        match value.to_lowercase().as_str() {
            "file" => Some(Self::File),
            "location" => Some(Self::Location),
            _ => None,
        }
    }
}

/// `IProblemPattern`: the regular expression and the group of each part of a problem.
#[derive(Debug, Clone, PartialEq)]
pub struct ProblemPattern {
    pub regexp: Re,
    pub kind: Option<LocationKind>,
    pub file: Option<usize>,
    pub message: Option<usize>,
    pub location: Option<usize>,
    pub line: Option<usize>,
    pub character: Option<usize>,
    pub end_line: Option<usize>,
    pub end_character: Option<usize>,
    pub code: Option<usize>,
    pub severity: Option<usize>,
    /// Only on the last pattern of a multi-line matcher: it matches again line after line.
    pub loops: bool,
}

impl ProblemPattern {
    fn bare(regexp: Re) -> Self {
        Self {
            regexp,
            kind: None,
            file: None,
            message: None,
            location: None,
            line: None,
            character: None,
            end_line: None,
            end_character: None,
            code: None,
            severity: None,
            loops: false,
        }
    }
}

/// `Types.SingleOrMany<IProblemPattern>`: one pattern, or the lines of a multi-line one.
#[derive(Debug, Clone, PartialEq)]
pub enum PatternSet {
    Single(ProblemPattern),
    Multi(Vec<ProblemPattern>),
}

impl PatternSet {
    pub fn patterns(&self) -> &[ProblemPattern] {
        match self {
            Self::Single(p) => std::slice::from_ref(p),
            Self::Multi(v) => v,
        }
    }
}

/// `base/common/severity.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskSeverity {
    Ignore,
    Info,
    Warning,
    Error,
}

impl TaskSeverity {
    /// `Severity.fromValue`: `error`, `warning` or `warn`, `info`, any case; anything else ignores.
    pub fn from_value(value: &str) -> Self {
        if value.eq_ignore_ascii_case("error") {
            Self::Error
        } else if value.eq_ignore_ascii_case("warning") || value.eq_ignore_ascii_case("warn") {
            Self::Warning
        } else if value.eq_ignore_ascii_case("info") {
            Self::Info
        } else {
            Self::Ignore
        }
    }

    /// `MarkerSeverity.fromSeverity`: 8 error, 4 warning, 2 info, 1 hint.
    pub fn marker(self) -> u8 {
        match self {
            Self::Error => 8,
            Self::Warning => 4,
            Self::Info => 2,
            Self::Ignore => 1,
        }
    }
}

/// `FileLocationKind`: how a file name a pattern found becomes a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileLocation {
    Absolute,
    Relative,
    AutoDetect,
    Search,
}

impl FileLocation {
    fn from_name(value: &str) -> Option<Self> {
        match value.to_lowercase().as_str() {
            "absolute" => Some(Self::Absolute),
            "relative" => Some(Self::Relative),
            "autodetect" => Some(Self::AutoDetect),
            "search" => Some(Self::Search),
            _ => None,
        }
    }
}

/// `filePrefix`: the folder of a relative location, or what a search looks through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePrefix {
    None,
    Path(String),
    Search { include: Vec<String>, exclude: Vec<String> },
}

/// `ApplyToKind`: which documents a problem may be reported on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyTo {
    AllDocuments,
    OpenDocuments,
    ClosedDocuments,
}

impl ApplyTo {
    fn from_name(value: &str) -> Option<Self> {
        match value.to_lowercase().as_str() {
            "alldocuments" => Some(Self::AllDocuments),
            "opendocuments" => Some(Self::OpenDocuments),
            "closeddocuments" => Some(Self::ClosedDocuments),
            _ => None,
        }
    }

    /// `mergeApplyTo`: two owners' kinds together.
    fn merge(self, other: Self) -> Self {
        if self == other || self == Self::AllDocuments {
            self
        } else {
            Self::AllDocuments
        }
    }
}

/// `IWatchingPattern`: the begin or the end of a background task's pass.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchingPattern {
    pub regexp: Re,
    /// The group of a file the pass is about; Code-OSS makes it 1 when a definition has none, and
    /// leaves it out for the old `watchedTaskBeginsRegExp`.
    pub file: Option<usize>,
}

/// `IWatchingMatcher`: `background` (or the older `watching`) of a definition.
#[derive(Debug, Clone, PartialEq)]
pub struct Watching {
    pub active_on_start: bool,
    pub begins: WatchingPattern,
    pub ends: WatchingPattern,
}

/// `ProblemMatcher` (with `INamedProblemMatcher`'s name and label).
#[derive(Debug, Clone, PartialEq)]
pub struct ProblemMatcher {
    pub name: Option<String>,
    pub label: Option<String>,
    pub owner: String,
    pub source: Option<String>,
    pub apply_to: ApplyTo,
    pub file_location: FileLocation,
    pub file_prefix: FilePrefix,
    pub pattern: PatternSet,
    pub severity: Option<TaskSeverity>,
    pub watching: Option<Watching>,
    pub deprecated: bool,
    /// The workspace's files, for `fileLocation` `search`; `None` until the window has listed
    /// them ([`ProblemCollector::set_search_files`]).
    pub search_files: Option<Arc<Vec<String>>>,
}

impl ProblemMatcher {
    /// The variables of the prefix resolved (`terminalTaskSystem._resolveMatchers`).
    pub fn resolve_prefix(&mut self, resolve: impl Fn(&str) -> String) {
        match &mut self.file_prefix {
            FilePrefix::None => {}
            FilePrefix::Path(p) => *p = resolve(p),
            FilePrefix::Search { include, exclude } => {
                for p in include.iter_mut().chain(exclude.iter_mut()) {
                    *p = resolve(p);
                }
            }
        }
    }

    /// `getResource`: the path a file name of a problem stands for, or `None` where Code-OSS fails
    /// (a relative location with no prefix).
    pub fn resource(&self, filename: &str) -> Option<String> {
        let prefix = match &self.file_prefix {
            FilePrefix::Path(p) => Some(p.as_str()),
            _ => None,
        };
        let full = match self.file_location {
            FileLocation::Absolute => filename.to_owned(),
            FileLocation::Search => match (&self.file_prefix, &self.search_files) {
                (FilePrefix::Search { include, exclude }, Some(files)) => {
                    search_file_location(filename, include, exclude, files)
                        .unwrap_or_else(|| filename.to_owned())
                }
                _ => filename.to_owned(),
            },
            FileLocation::Relative => format!("{}/{filename}", prefix?),
            FileLocation::AutoDetect => match prefix {
                Some(p) if !is_absolute(filename) => format!("{p}/{filename}"),
                _ => filename.to_owned(),
            },
        };
        let full = normalize(&full).replace('\\', "/");
        Some(if full.starts_with('/') { full } else { format!("/{full}") })
    }
}

/// `searchForFileLocation`: the first of `files` below an `include` folder whose path ends with
/// `filename`, in the order Code-OSS's walk meets them: the include folders in turn, and in each
/// folder its files (by name) before its subfolders (by name, each searched whole before the
/// next). A folder that is one of `exclude` is not entered.
pub fn search_file_location(filename: &str, include: &[String], exclude: &[String], files: &[String]) -> Option<String> {
    let trim = |p: &str| {
        let t = p.trim_end_matches('/');
        if t.is_empty() { "/".to_owned() } else { t.to_owned() }
    };
    let exclude: Vec<String> = exclude.iter().map(|e| trim(e)).collect();
    for dir in include {
        let dir = trim(dir);
        if exclude.contains(&dir) {
            continue;
        }
        let base = if dir == "/" { String::new() } else { dir.clone() };
        // The walk order as a key: at each level a file (0) comes before a folder (1), then names.
        let mut hits: Vec<_> = files
            .iter()
            .filter_map(|path| {
                let rest = path.strip_prefix(&base)?.strip_prefix('/')?;
                let segments: Vec<&str> = rest.split('/').collect();
                let (name, folders) = segments.split_last()?;
                // A file inside an excluded folder is never reached.
                let mut folder = base.clone();
                for f in folders {
                    folder.push('/');
                    folder.push_str(f);
                    if exclude.contains(&folder) {
                        return None;
                    }
                }
                if !path.ends_with(filename) {
                    return None;
                }
                let mut key: Vec<(u8, &str)> = folders.iter().map(|f| (1, *f)).collect();
                key.push((0, *name));
                Some((key, path))
            })
            .collect();
        hits.sort();
        if let Some((_, path)) = hits.into_iter().next() {
            return Some(path.clone());
        }
    }
    None
}

fn is_absolute(path: &str) -> bool {
    let b = path.as_bytes();
    path.starts_with('/') || path.starts_with('\\') || (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\'))
}

/// `path.normalize` (POSIX): `.` and `..` resolved, repeated slashes folded.
pub fn normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            s => parts.push(s),
        }
    }
    let mut out = parts.join("/");
    if out.is_empty() && !absolute {
        out.push('.');
    }
    if trailing && !out.is_empty() {
        out.push('/');
    }
    if absolute {
        format!("/{out}")
    } else {
        out
    }
}

// ---- line matching ----------------------------------------------------------------------------

/// A problem a matcher found: its owner, the path of its file, and the marker.
#[derive(Debug, Clone, PartialEq)]
pub struct ProblemMatch {
    pub owner: String,
    pub path: String,
    pub marker: MarkerData,
    pub apply_to: ApplyTo,
}

/// `IProblemData`: what the groups gave so far.
#[derive(Debug, Clone, Default)]
struct ProblemData {
    kind: Option<LocationKind>,
    file: Option<String>,
    location: Option<String>,
    line: Option<String>,
    character: Option<String>,
    end_line: Option<String>,
    end_character: Option<String>,
    message: Option<String>,
    severity: Option<String>,
    code: Option<String>,
}

/// `Strings.trim`: spaces only.
fn trim_spaces(s: &str) -> &str {
    s.trim_matches(' ')
}

fn fill(slot: &mut Option<String>, index: Option<usize>, groups: &[Option<String>], trim: bool) {
    if slot.is_some() {
        return;
    }
    let Some(i) = index.filter(|i| *i < groups.len()) else { return };
    if let Some(v) = &groups[i] {
        *slot = Some(if trim { trim_spaces(v) } else { v }.to_owned());
    }
}

/// `appendProperty`: a later line of a multi-line problem adds to the message.
fn append(slot: &mut Option<String>, index: Option<usize>, groups: &[Option<String>], trim: bool) {
    if slot.is_none() {
        fill(slot, index, groups, trim);
        return;
    }
    let Some(i) = index.filter(|i| *i < groups.len()) else { return };
    if let (Some(current), Some(v)) = (slot.as_mut(), &groups[i]) {
        current.push('\n');
        current.push_str(if trim { trim_spaces(v) } else { v });
    }
}

fn fill_problem_data(data: &mut ProblemData, p: &ProblemPattern, groups: &[Option<String>]) {
    fill(&mut data.file, p.file, groups, true);
    append(&mut data.message, p.message, groups, true);
    fill(&mut data.code, p.code, groups, true);
    fill(&mut data.severity, p.severity, groups, true);
    fill(&mut data.location, p.location, groups, true);
    fill(&mut data.line, p.line, groups, false);
    fill(&mut data.character, p.character, groups, false);
    fill(&mut data.end_line, p.end_line, groups, false);
    fill(&mut data.end_character, p.end_character, groups, false);
}

/// `parseInt`: the leading integer. JavaScript's `NaN` is 0 here, which the marker service's
/// clean-up turns into the same line and column `NaN` gets.
fn parse_int(s: &str) -> i64 {
    let t = s.trim_start();
    let (negative, digits) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let n: String = digits.chars().take_while(char::is_ascii_digit).collect();
    let v = n.parse::<i64>().unwrap_or(0);
    if negative {
        -v
    } else {
        v
    }
}

fn non_empty(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.is_empty())
}

/// (start line, start column, end line, end column), before the marker service cleans them.
type Location = (i64, i64, i64, i64);

/// `createLocation`.
fn create_location(line: i64, column: Option<i64>, end_line: Option<i64>, end_column: Option<i64>) -> Location {
    match (column, end_column) {
        (Some(c), Some(ec)) => (line, c, end_line.filter(|l| *l != 0).unwrap_or(line), ec),
        (Some(c), None) => (line, c, line, c),
        _ => (line, 1, line, i64::from(i32::MAX)),
    }
}

/// `parseLocationInfo`: `line`, `line,column` or `line,column,endLine,endColumn`.
fn parse_location_info(value: &str) -> Option<Location> {
    if !value.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    let parts: Vec<&str> = value.split(',').collect();
    let line = parse_int(parts[0]);
    let column = (parts.len() > 1).then(|| parse_int(parts[1]));
    Some(if parts.len() > 3 {
        create_location(line, column, Some(parse_int(parts[2])), Some(parse_int(parts[3])))
    } else {
        create_location(line, column, None, None)
    })
}

fn location_of(data: &ProblemData) -> Option<Location> {
    if data.kind == Some(LocationKind::File) {
        return Some(create_location(0, Some(0), Some(0), Some(0)));
    }
    if let Some(location) = non_empty(&data.location) {
        return parse_location_info(location);
    }
    let line = non_empty(&data.line)?;
    Some(create_location(
        parse_int(line),
        non_empty(&data.character).map(parse_int),
        non_empty(&data.end_line).map(parse_int),
        non_empty(&data.end_character).map(parse_int),
    ))
}

fn severity_of(data: &ProblemData, matcher: &ProblemMatcher) -> u8 {
    let mut result = None;
    if let Some(value) = non_empty(&data.severity) {
        let mut s = TaskSeverity::from_value(value);
        if s == TaskSeverity::Ignore {
            s = match value {
                "E" => TaskSeverity::Error,
                "W" => TaskSeverity::Warning,
                "I" => TaskSeverity::Info,
                v if v.eq_ignore_ascii_case("hint") || v.eq_ignore_ascii_case("note") => TaskSeverity::Info,
                _ => TaskSeverity::Ignore,
            };
        }
        result = Some(s);
    }
    let s = match result {
        None | Some(TaskSeverity::Ignore) => match matcher.severity {
            None | Some(TaskSeverity::Ignore) => TaskSeverity::Error,
            Some(s) => s,
        },
        Some(s) => s,
    };
    s.marker()
}

fn clamp_u32(v: i64) -> u32 {
    u32::try_from(v.max(0)).unwrap_or(u32::MAX)
}

/// `getMarkerMatch`, with the clean-up `markerService` does to every marker it is given.
fn marker_match(matcher: &ProblemMatcher, data: &ProblemData) -> Option<ProblemMatch> {
    let location = location_of(data)?;
    let file = non_empty(&data.file)?;
    let message = non_empty(&data.message)?;
    let path = matcher.resource(file)?;
    let (line, column, end_line, end_column) = location;
    let start_line = if line > 0 { line } else { 1 };
    let start_column = if column > 0 { column } else { 1 };
    let end_line = if end_line >= start_line { end_line } else { start_line };
    let end_column = if end_column > 0 { end_column } else { start_column };
    Some(ProblemMatch {
        owner: matcher.owner.clone(),
        path,
        apply_to: matcher.apply_to,
        marker: MarkerData {
            code: data.code.clone().map(Value::String),
            severity: severity_of(data, matcher),
            message: message.to_owned(),
            source: matcher.source.clone(),
            start_line_number: clamp_u32(start_line),
            start_column: clamp_u32(start_column),
            end_line_number: clamp_u32(end_line),
            end_column: clamp_u32(end_column),
            model_version_id: None,
            related_information: None,
            tags: None,
        },
    })
}

/// `SingleLineMatcher` and `MultiLineMatcher`.
struct LineMatcher {
    matcher: Arc<ProblemMatcher>,
    /// What a looping multi-line matcher found before its last line.
    data: Option<ProblemData>,
}

impl LineMatcher {
    fn match_length(&self) -> usize {
        self.matcher.pattern.patterns().len().max(1)
    }

    fn handle(&mut self, lines: &[String], start: usize) -> (Option<ProblemMatch>, bool) {
        let matcher = self.matcher.clone();
        match &matcher.pattern {
            PatternSet::Single(p) => {
                let mut data = ProblemData { kind: p.kind, ..Default::default() };
                let Some(groups) = p.regexp.exec(&lines[start]) else { return (None, false) };
                fill_problem_data(&mut data, p, &groups);
                if data.kind == Some(LocationKind::Location)
                    && non_empty(&data.location).is_none()
                    && non_empty(&data.line).is_none()
                    && non_empty(&data.file).is_some()
                {
                    data.kind = Some(LocationKind::File);
                }
                (marker_match(&matcher, &data), false)
            }
            PatternSet::Multi(patterns) => {
                let Some(last) = patterns.last() else { return (None, false) };
                let mut data = ProblemData { kind: patterns[0].kind, ..Default::default() };
                for (i, p) in patterns.iter().enumerate() {
                    let Some(groups) = lines.get(i + start).and_then(|l| p.regexp.exec(l)) else {
                        return (None, false);
                    };
                    // Only the last pattern can loop: what came before is kept for its next lines.
                    if p.loops && i == patterns.len() - 1 {
                        self.data = Some(data.clone());
                    }
                    fill_problem_data(&mut data, p, &groups);
                }
                if !last.loops {
                    self.data = None;
                }
                (marker_match(&matcher, &data), last.loops)
            }
        }
    }

    fn next(&mut self, line: &str) -> Option<ProblemMatch> {
        let matcher = self.matcher.clone();
        let last = matcher.pattern.patterns().last()?;
        let Some(groups) = last.regexp.exec(line) else {
            self.data = None;
            return None;
        };
        let mut data = self.data.clone()?;
        fill_problem_data(&mut data, last, &groups);
        marker_match(&matcher, &data)
    }
}

// ---- collectors -------------------------------------------------------------------------------

/// What a collector tells the marker service, and the state changes of a background task.
#[derive(Debug, Clone, PartialEq)]
pub enum CollectorEvent {
    /// Every marker of `owner` on `path` is now `markers` (`markerService.changeOne`).
    Markers { owner: String, path: String, markers: Vec<MarkerData> },
    /// The markers of `owner` on these paths are removed (`markerService.remove`).
    Remove { owner: String, paths: Vec<String> },
    /// A background pass began (`BackgroundProcessingBegins`): the task is active.
    Begins,
    /// A background pass ended (`BackgroundProcessingEnds`): the task is inactive, which is the
    /// point a debug start waits for.
    Ends,
}

/// `IMarkerData.makeKeyOptionalMessage(marker, false)`: two markers with this key are one.
type MarkerKey = (Option<String>, Option<String>, u8, u32, u32, u32, u32);

fn marker_key(m: &MarkerData) -> MarkerKey {
    let code = match &m.code {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Object(o)) => o.get("value").map(|v| v.as_str().map(str::to_owned).unwrap_or_else(|| v.to_string())),
        _ => None,
    };
    (m.source.clone().filter(|s| !s.is_empty()), code, m.severity, m.start_line_number, m.start_column, m.end_line_number, m.end_column)
}

/// `StartStopProblemCollector` (a task that exits) or `WatchingProblemCollector` (a background
/// task), in one type because they share `AbstractProblemCollector`.
pub struct ProblemCollector {
    watching: bool,
    line_matchers: BTreeMap<usize, Vec<LineMatcher>>,
    active: Option<(usize, usize)>,
    buffer_length: usize,
    buffer: Vec<String>,
    number_of_matches: usize,
    max_severity: Option<u8>,
    apply_to_by_owner: BTreeMap<String, ApplyTo>,
    resources_to_clean: BTreeMap<String, BTreeSet<String>>,
    markers: BTreeMap<String, BTreeMap<String, Vec<(MarkerKey, MarkerData)>>>,
    delivered: HashMap<String, HashMap<String, usize>>,
    /// The paths each owner has markers on, as far as task problems go: what `markerService.read`
    /// answers in Code-OSS.
    known: BTreeMap<String, BTreeSet<String>>,
    open: HashSet<String>,
    current: Option<(String, String)>,
    backgrounds: Vec<Arc<ProblemMatcher>>,
    active_backgrounds: BTreeSet<usize>,
    events: Vec<CollectorEvent>,
}

impl ProblemCollector {
    /// A collector for `matchers`: the watching one for a background task, else the start-stop
    /// one. `known` is where each owner has task problems already, so a run can clean them.
    pub fn new(matchers: Vec<ProblemMatcher>, background: bool, known: BTreeMap<String, BTreeSet<String>>) -> Self {
        let matchers: Vec<Arc<ProblemMatcher>> = matchers.into_iter().map(Arc::new).collect();
        let mut line_matchers: BTreeMap<usize, Vec<LineMatcher>> = BTreeMap::new();
        let mut buffer_length = 1;
        let mut apply_to_by_owner: BTreeMap<String, ApplyTo> = BTreeMap::new();
        for m in &matchers {
            let lm = LineMatcher { matcher: m.clone(), data: None };
            buffer_length = buffer_length.max(lm.match_length());
            line_matchers.entry(lm.match_length()).or_default().push(lm);
            let merged = match apply_to_by_owner.get(&m.owner) {
                Some(current) => current.merge(m.apply_to),
                None => m.apply_to,
            };
            apply_to_by_owner.insert(m.owner.clone(), merged);
        }
        let backgrounds = if background { matchers.iter().filter(|m| m.watching.is_some()).cloned().collect() } else { Vec::new() };
        let mut collector = Self {
            watching: background,
            line_matchers,
            active: None,
            buffer_length,
            buffer: Vec::new(),
            number_of_matches: 0,
            max_severity: None,
            apply_to_by_owner,
            resources_to_clean: BTreeMap::new(),
            markers: BTreeMap::new(),
            delivered: HashMap::new(),
            known,
            open: HashSet::new(),
            current: None,
            backgrounds,
            active_backgrounds: BTreeSet::new(),
            events: Vec::new(),
        };
        if !background {
            // The start-stop collector cleans, at the end, what its owners reported before and
            // this run did not report again.
            let owners: Vec<String> = collector.apply_to_by_owner.keys().cloned().collect();
            for owner in owners {
                collector.record_resources_to_clean(&owner);
            }
        }
        collector
    }

    /// Whether a background task's collector has a matcher with `background` patterns
    /// (`isWatching`). One without can never tell the task is ready.
    pub fn is_watching(&self) -> bool {
        !self.backgrounds.is_empty()
    }

    pub fn number_of_matches(&self) -> usize {
        self.number_of_matches
    }

    /// The highest marker severity found since the last pass began (8 is an error).
    pub fn max_severity(&self) -> Option<u8> {
        self.max_severity
    }

    /// Whether a matcher uses `fileLocation` `search`, so the window lists the workspace's files.
    pub fn uses_search(&self) -> bool {
        self.line_matchers.values().flatten().any(|m| m.matcher.file_location == FileLocation::Search)
            || self.backgrounds.iter().any(|m| m.file_location == FileLocation::Search)
    }

    /// The workspace's files for the matchers with `fileLocation` `search`
    /// ([`search_file_location`]); a problem found after this is looked up in them.
    pub fn set_search_files(&mut self, files: Arc<Vec<String>>) {
        let set = |m: &mut Arc<ProblemMatcher>| {
            if m.file_location == FileLocation::Search {
                Arc::make_mut(m).search_files = Some(files.clone());
            }
        };
        for lm in self.line_matchers.values_mut().flatten() {
            set(&mut lm.matcher);
        }
        for m in &mut self.backgrounds {
            set(m);
        }
    }

    /// The paths of the open documents, for `applyTo`.
    pub fn set_open_documents(&mut self, paths: impl IntoIterator<Item = String>) {
        self.open = paths.into_iter().collect();
    }

    /// What happened since the last call, in order.
    pub fn take_events(&mut self) -> Vec<CollectorEvent> {
        std::mem::take(&mut self.events)
    }

    /// `aboutToStart`: a background matcher with `activeOnStart` is in a pass from the start.
    pub fn about_to_start(&mut self) {
        if !self.watching {
            return;
        }
        let backgrounds = self.backgrounds.clone();
        for (id, b) in backgrounds.iter().enumerate() {
            if b.watching.as_ref().is_some_and(|w| w.active_on_start) {
                self.active_backgrounds.insert(id);
                self.events.push(CollectorEvent::Begins);
                self.record_resources_to_clean(&b.owner);
            }
        }
    }

    /// `processLine`: one line of the task's output, without its line break.
    pub fn process_line(&mut self, line: &str) {
        if self.watching && (self.try_begin(line) || self.try_finish(line)) {
            return;
        }
        let Some(found) = self.try_find_marker(line) else { return };
        let owner = found.owner.clone();
        let path = found.path.clone();
        self.remove_resource_to_clean(&owner, &path);
        if self.should_apply(&found) {
            self.record_marker(found.marker, &owner, &path);
            let here = (owner, path);
            if self.current.as_ref() != Some(&here) {
                if let Some((o, p)) = self.current.take() {
                    self.deliver(&o, &p);
                }
                self.current = Some(here);
            }
        }
    }

    /// `forceDelivery`: a background task's problems of the file it is on are reported now. The
    /// start-stop collector reports a file only when it moves on or the task ends.
    pub fn force_delivery(&mut self) {
        if !self.watching {
            return;
        }
        if let Some((o, p)) = self.current.clone() {
            self.deliver(&o, &p);
        }
    }

    /// `done`: the task's process ended. What is left is reported, and what was not reported again
    /// is removed (for a background task, every problem of its owners).
    pub fn done(&mut self) {
        if self.watching {
            let owners: Vec<String> = self.apply_to_by_owner.keys().cloned().collect();
            for owner in owners {
                self.record_resources_to_clean(&owner);
            }
        }
        self.report_markers();
        self.clean_all_markers();
    }

    fn try_find_marker(&mut self, line: &str) -> Option<ProblemMatch> {
        if let Some((length, index)) = self.active {
            let next = self.line_matchers.get_mut(&length).and_then(|v| v.get_mut(index)).and_then(|m| m.next(line));
            if let Some(found) = next {
                self.capture(&found);
                return Some(found);
            }
            self.buffer.clear();
            self.active = None;
        }
        if self.buffer.len() < self.buffer_length {
            self.buffer.push(line.to_owned());
        } else {
            self.buffer.remove(0);
            self.buffer.push(line.to_owned());
        }
        let found = self.try_matchers();
        if found.is_some() {
            self.buffer.clear();
        }
        found
    }

    fn try_matchers(&mut self) -> Option<ProblemMatch> {
        self.active = None;
        let length = self.buffer.len();
        let mut found = None;
        'outer: for start in 0..length {
            let want = length - start;
            let Some(candidates) = self.line_matchers.get_mut(&want) else { continue };
            for (index, lm) in candidates.iter_mut().enumerate() {
                let (m, more) = lm.handle(&self.buffer, start);
                if let Some(m) = m {
                    if more {
                        self.active = Some((want, index));
                    }
                    found = Some(m);
                    break 'outer;
                }
            }
        }
        if let Some(m) = &found {
            self.capture(m);
        }
        found
    }

    fn capture(&mut self, m: &ProblemMatch) {
        self.number_of_matches += 1;
        if self.max_severity.is_none_or(|s| m.marker.severity > s) {
            self.max_severity = Some(m.marker.severity);
        }
    }

    fn should_apply(&self, m: &ProblemMatch) -> bool {
        match m.apply_to {
            ApplyTo::AllDocuments => true,
            ApplyTo::OpenDocuments => self.open.contains(&m.path),
            ApplyTo::ClosedDocuments => !self.open.contains(&m.path),
        }
    }

    fn record_marker(&mut self, marker: MarkerData, owner: &str, path: &str) {
        let key = marker_key(&marker);
        let list = self.markers.entry(owner.to_owned()).or_default().entry(path.to_owned()).or_default();
        if !list.iter().any(|(k, _)| *k == key) {
            list.push((key, marker));
        }
    }

    fn deliver(&mut self, owner: &str, path: &str) {
        let Some(list) = self.markers.get(owner).and_then(|m| m.get(path)) else { return };
        let delivered = self.delivered.entry(owner.to_owned()).or_default();
        if delivered.get(path) == Some(&list.len()) {
            return;
        }
        delivered.insert(path.to_owned(), list.len());
        let markers: Vec<MarkerData> = list.iter().map(|(_, m)| m.clone()).collect();
        self.known.entry(owner.to_owned()).or_default().insert(path.to_owned());
        self.events.push(CollectorEvent::Markers { owner: owner.to_owned(), path: path.to_owned(), markers });
    }

    fn report_markers(&mut self) {
        let all: Vec<(String, String)> =
            self.markers.iter().flat_map(|(o, by_path)| by_path.keys().map(move |p| (o.clone(), p.clone()))).collect();
        for (o, p) in all {
            self.deliver(&o, &p);
        }
    }

    fn record_resources_to_clean(&mut self, owner: &str) {
        let paths = self.known.get(owner).cloned().unwrap_or_default();
        self.resources_to_clean.entry(owner.to_owned()).or_default().extend(paths);
    }

    fn record_resource_to_clean(&mut self, owner: &str, path: String) {
        self.resources_to_clean.entry(owner.to_owned()).or_default().insert(path);
    }

    fn remove_resource_to_clean(&mut self, owner: &str, path: &str) {
        if let Some(set) = self.resources_to_clean.get_mut(owner) {
            set.remove(path);
        }
    }

    fn clean_all_markers(&mut self) {
        let all = std::mem::take(&mut self.resources_to_clean);
        for (owner, paths) in all {
            self.clean(&owner, paths);
        }
    }

    fn clean_markers(&mut self, owner: &str) {
        if let Some(paths) = self.resources_to_clean.remove(owner) {
            self.clean(owner, paths);
        }
    }

    fn clean(&mut self, owner: &str, paths: BTreeSet<String>) {
        let Some(apply_to) = self.apply_to_by_owner.get(owner).copied() else { return };
        let paths: Vec<String> = paths
            .into_iter()
            .filter(|p| match apply_to {
                ApplyTo::AllDocuments => true,
                ApplyTo::OpenDocuments => self.open.contains(p),
                ApplyTo::ClosedDocuments => !self.open.contains(p),
            })
            .collect();
        if paths.is_empty() {
            return;
        }
        if let Some(known) = self.known.get_mut(owner) {
            for p in &paths {
                known.remove(p);
            }
        }
        self.events.push(CollectorEvent::Remove { owner: owner.to_owned(), paths });
    }

    fn clean_marker_caches(&mut self) {
        self.number_of_matches = 0;
        self.max_severity = None;
        self.markers.clear();
        self.delivered.clear();
    }

    fn reset_current_resource(&mut self) {
        if let Some((o, p)) = self.current.take() {
            self.deliver(&o, &p);
        }
    }

    fn try_begin(&mut self, line: &str) -> bool {
        let mut result = false;
        let backgrounds = self.backgrounds.clone();
        for (id, b) in backgrounds.iter().enumerate() {
            let Some(w) = &b.watching else { continue };
            let Some(groups) = w.begins.regexp.exec(line) else { continue };
            if self.active_backgrounds.contains(&id) {
                continue;
            }
            self.active_backgrounds.insert(id);
            result = true;
            self.events.push(CollectorEvent::Begins);
            self.clean_marker_caches();
            self.reset_current_resource();
            let file = w.begins.file.and_then(|i| groups.get(i).cloned().flatten()).filter(|f| !f.is_empty());
            match file.and_then(|f| b.resource(&f)) {
                Some(path) => self.record_resource_to_clean(&b.owner, path),
                None => self.record_resources_to_clean(&b.owner),
            }
        }
        result
    }

    fn try_finish(&mut self, line: &str) -> bool {
        let mut result = false;
        let backgrounds = self.backgrounds.clone();
        for (id, b) in backgrounds.iter().enumerate() {
            let Some(w) = &b.watching else { continue };
            if w.ends.regexp.exec(line).is_none() {
                continue;
            }
            if self.active_backgrounds.remove(&id) {
                self.reset_current_resource();
                self.events.push(CollectorEvent::Ends);
                result = true;
                self.clean_markers(&b.owner);
                self.clean_marker_caches();
            }
        }
        result
    }
}

// ---- the registry and the parser -------------------------------------------------------------

/// The named patterns and matchers (`ProblemPatternRegistry`, `ProblemMatcherRegistry`).
#[derive(Debug, Clone, Default)]
pub struct Registry {
    patterns: BTreeMap<String, PatternSet>,
    matchers: BTreeMap<String, ProblemMatcher>,
}

fn obj_usize(o: &Map<String, Value>, key: &str) -> Option<usize> {
    o.get(key).and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok())
}

fn non_empty_str<'a>(o: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    o.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

impl Registry {
    /// The patterns and matchers Code-OSS registers itself (`fillDefaults` of both registries).
    pub fn builtin() -> Self {
        let mut r = Self::default();
        r.fill_default_patterns();
        r.fill_default_matchers();
        r
    }

    /// The built-ins, then what the extensions contribute: every `problemPatterns` first, as the
    /// matchers' extension point depends on the patterns', then every `problemMatchers`. A name
    /// contributed again replaces the earlier one, as the registries' `add` does.
    pub fn with_extensions(extensions: &[Value]) -> Self {
        let mut r = Self::builtin();
        for p in contributed(extensions, "problemPatterns") {
            let mut errors = Vec::new();
            let Some(name) = p.get("name").and_then(Value::as_str).map(str::to_owned) else { continue };
            if let Some(set) = parse_pattern(p, &mut errors) {
                r.patterns.insert(name, set);
            }
        }
        for m in contributed(extensions, "problemMatchers") {
            let mut errors = Vec::new();
            if let Some(matcher) = r.parse_matcher(m, &mut errors) {
                if let Some(name) = matcher.name.clone() {
                    r.matchers.insert(name, matcher);
                }
            }
        }
        r
    }

    pub fn pattern(&self, name: &str) -> Option<&PatternSet> {
        self.patterns.get(name)
    }

    pub fn matcher(&self, name: &str) -> Option<&ProblemMatcher> {
        self.matchers.get(name)
    }

    /// Every matcher name, without the `$`.
    pub fn names(&self) -> Vec<&str> {
        self.matchers.keys().map(String::as_str).collect()
    }

    /// The matchers of a task's `problemMatcher` (taskConfiguration's `ProblemMatcherConverter`):
    /// a `$name`, a definition, or a list of both. What cannot be used is left out and `errors`
    /// says why, in Code-OSS's words.
    pub fn resolve(&self, value: Option<&Value>, errors: &mut Vec<String>) -> Vec<ProblemMatcher> {
        let one = |v: &Value, errors: &mut Vec<String>| -> Option<ProblemMatcher> {
            match v {
                Value::String(s) => {
                    let found = s.strip_prefix('$').filter(|n| !n.is_empty()).and_then(|n| self.matchers.get(n)).cloned();
                    if found.is_none() {
                        errors.push(format!("Error: Invalid problemMatcher reference: {s}\n"));
                    }
                    found
                }
                Value::Object(_) => self.parse_matcher(v, errors),
                _ => None,
            }
        };
        match value {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(list)) => list.iter().filter_map(|v| one(v, errors)).collect(),
            Some(v) => one(v, errors).into_iter().collect(),
        }
    }

    /// `ProblemMatcherParser.parse`: a definition with Code-OSS's defaults (owner, `applyTo`
    /// `allDocuments`, `fileLocation` relative to `${workspaceFolder}`), its `base`, and its
    /// `background` (or `watching`, or the old `watchedTaskBeginsRegExp`).
    pub fn parse_matcher(&self, json: &Value, errors: &mut Vec<String>) -> Option<ProblemMatcher> {
        let Some(o) = json.as_object() else {
            errors.push(format!("Error: the description can't be converted into a problem matcher:\n{json}\n"));
            return None;
        };
        let owner_given = o.get("owner").and_then(Value::as_str).map(str::to_owned);
        let owner = owner_given.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let source = o.get("source").and_then(Value::as_str).map(str::to_owned);
        let apply_to_given = o.get("applyTo").and_then(Value::as_str);
        let apply_to = apply_to_given.and_then(ApplyTo::from_name).unwrap_or(ApplyTo::AllDocuments);
        let workspace = || "${workspaceFolder}".to_owned();
        let location_given = o.get("fileLocation");
        let (file_location, file_prefix) = match location_given {
            None => (Some(FileLocation::Relative), FilePrefix::Path(workspace())),
            Some(Value::String(s)) => match FileLocation::from_name(s) {
                Some(k @ (FileLocation::Relative | FileLocation::AutoDetect)) => (Some(k), FilePrefix::Path(workspace())),
                Some(FileLocation::Search) => (Some(FileLocation::Search), FilePrefix::Search { include: vec![workspace()], exclude: Vec::new() }),
                Some(k) => (Some(k), FilePrefix::None),
                None => (None, FilePrefix::None),
            },
            Some(Value::Array(values)) if values.iter().all(Value::is_string) => {
                let values: Vec<&str> = values.iter().filter_map(Value::as_str).collect();
                let kind = values.first().and_then(|v| FileLocation::from_name(v));
                match (values.len(), kind) {
                    (1, Some(FileLocation::Absolute)) => (Some(FileLocation::Absolute), FilePrefix::None),
                    (2, Some(k @ (FileLocation::Relative | FileLocation::AutoDetect))) if !values[1].is_empty() => {
                        (Some(k), FilePrefix::Path(values[1].to_owned()))
                    }
                    _ => (None, FilePrefix::None),
                }
            }
            Some(Value::Array(values)) => match values.first().and_then(Value::as_str).and_then(FileLocation::from_name) {
                Some(FileLocation::Search) => {
                    let list = |v: Option<&Value>| -> Vec<String> {
                        match v {
                            Some(Value::String(s)) => vec![s.clone()],
                            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
                            _ => Vec::new(),
                        }
                    };
                    let prefix = match values.get(1) {
                        Some(args) if args.is_object() => {
                            FilePrefix::Search { include: list(args.get("include")), exclude: list(args.get("exclude")) }
                        }
                        _ => FilePrefix::Search { include: vec![workspace()], exclude: Vec::new() },
                    };
                    (Some(FileLocation::Search), prefix)
                }
                _ => (None, FilePrefix::None),
            },
            Some(_) => (None, FilePrefix::None),
        };
        let pattern_given = o.get("pattern").filter(|p| !p.is_null() && *p != &Value::String(String::new()));
        let pattern = pattern_given.and_then(|p| self.create_pattern(p, errors));
        let severity = match o.get("severity") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.is_empty() => None,
            Some(v) => {
                let s = v.as_str().map(TaskSeverity::from_value).unwrap_or(TaskSeverity::Ignore);
                if s == TaskSeverity::Ignore {
                    errors.push(format!("Info: unknown severity {v}. Valid values are error, warning and info.\n"));
                    Some(TaskSeverity::Error)
                } else {
                    Some(s)
                }
            }
        };
        let mut result: Option<ProblemMatcher> = None;
        if let Some(base) = o.get("base").and_then(Value::as_str) {
            if let Some(mut m) = base.strip_prefix('$').filter(|n| !n.is_empty()).and_then(|n| self.matchers.get(n)).cloned() {
                if owner_given.is_some() {
                    m.owner = owner.clone();
                }
                if source.is_some() {
                    m.source = source.clone();
                }
                if location_given.is_some() {
                    if let Some(k) = file_location {
                        m.file_location = k;
                        m.file_prefix = file_prefix.clone();
                    }
                }
                if let Some(p) = &pattern {
                    m.pattern = p.clone();
                }
                if let Some(s) = severity {
                    m.severity = Some(s);
                }
                if apply_to_given.is_some() {
                    m.apply_to = apply_to;
                }
                m.name = None;
                m.label = None;
                m.deprecated = false;
                result = Some(m);
            }
        } else if let (Some(k), Some(p)) = (file_location, pattern.clone()) {
            result = Some(ProblemMatcher {
                name: None,
                label: None,
                owner: owner.clone(),
                source: source.clone(),
                apply_to,
                file_location: k,
                file_prefix,
                pattern: p,
                severity,
                watching: None,
                deprecated: false,
                search_files: None,
            });
        }
        let Some(mut result) = result else {
            errors.push(format!("Error: the description can't be converted into a problem matcher:\n{json}\n"));
            return None;
        };
        if let Some(name) = o.get("name").and_then(Value::as_str) {
            result.name = Some(name.to_owned());
            result.label = Some(o.get("label").and_then(Value::as_str).unwrap_or(name).to_owned());
        }
        if result.owner.is_empty() {
            errors.push(format!("Error: the description doesn't define an owner:\n{json}\n"));
            return None;
        }
        self.add_watching(o, &mut result, errors);
        Some(result)
    }

    fn create_pattern(&self, value: &Value, errors: &mut Vec<String>) -> Option<PatternSet> {
        match value {
            Value::String(name) => match name.strip_prefix('$').filter(|n| !n.is_empty()) {
                Some(n) => {
                    let found = self.patterns.get(n).cloned();
                    if found.is_none() {
                        errors.push(format!("Error: the pattern with the identifier {name} doesn't exist."));
                    }
                    found
                }
                None => {
                    errors.push(format!("Error: the pattern property {name} is not a valid pattern variable name."));
                    None
                }
            },
            v => parse_pattern(v, errors),
        }
    }

    fn add_watching(&self, o: &Map<String, Value>, m: &mut ProblemMatcher, errors: &mut Vec<String>) {
        let mut legacy = |key: &str| non_empty_str(o, key).and_then(|s| Re::new(s).map_err(|e| errors.push(e)).ok());
        let old_begins = legacy("watchedTaskBeginsRegExp");
        let old_ends = legacy("watchedTaskEndsRegExp");
        if let (Some(begins), Some(ends)) = (old_begins, old_ends) {
            m.watching = Some(Watching {
                active_on_start: false,
                begins: WatchingPattern { regexp: begins, file: None },
                ends: WatchingPattern { regexp: ends, file: None },
            });
            return;
        }
        let Some(monitor) = o.get("background").or_else(|| o.get("watching")).filter(|v| !v.is_null()) else { return };
        let begins = watching_pattern(monitor.get("beginsPattern"), errors);
        let ends = watching_pattern(monitor.get("endsPattern"), errors);
        match (begins, ends) {
            (Some(begins), Some(ends)) => {
                m.watching = Some(Watching {
                    active_on_start: monitor.get("activeOnStart").and_then(Value::as_bool).unwrap_or(false),
                    begins,
                    ends,
                });
            }
            (None, None) => {}
            _ => errors.push("A problem matcher must define both a begin pattern and an end pattern for watching.".to_owned()),
        }
    }

    fn fill_default_patterns(&mut self) {
        use LocationKind::Location;
        let single = |p: ProblemPattern| PatternSet::Single(p);
        let p = |re: &str| ProblemPattern::bare(builtin_re(re));
        let defs = [
            (
                "msCompile",
                single(ProblemPattern {
                    kind: Some(Location),
                    file: Some(1),
                    location: Some(2),
                    severity: Some(4),
                    code: Some(5),
                    message: Some(6),
                    ..p(r"^\s*(?:\s*\d+>)?(\S.*?)(?:\((\d+|\d+,\d+|\d+,\d+,\d+,\d+)\))?\s*:\s+(?:(\S+)\s+)?((?:fatal +)?error|warning|info)\s+(\w+\d+)?\s*:\s*(.*)$")
                }),
            ),
            (
                "gulp-tsc",
                single(ProblemPattern {
                    kind: Some(Location),
                    file: Some(1),
                    location: Some(2),
                    code: Some(3),
                    message: Some(4),
                    ..p(r"^([^\s].*)\((\d+|\d+,\d+|\d+,\d+,\d+,\d+)\):\s+(\d+)\s+(.*)$")
                }),
            ),
            (
                "cpp",
                single(ProblemPattern {
                    kind: Some(Location),
                    file: Some(1),
                    location: Some(2),
                    severity: Some(3),
                    code: Some(4),
                    message: Some(5),
                    ..p(r"^(\S.*)\((\d+|\d+,\d+|\d+,\d+,\d+,\d+)\):\s+(error|warning|info)\s+(C\d+)\s*:\s*(.*)$")
                }),
            ),
            (
                "csc",
                single(ProblemPattern {
                    kind: Some(Location),
                    file: Some(1),
                    location: Some(2),
                    severity: Some(3),
                    code: Some(4),
                    message: Some(5),
                    ..p(r"^(\S.*)\((\d+|\d+,\d+|\d+,\d+,\d+,\d+)\):\s+(error|warning|info)\s+(CS\d+)\s*:\s*(.*)$")
                }),
            ),
            (
                "vb",
                single(ProblemPattern {
                    kind: Some(Location),
                    file: Some(1),
                    location: Some(2),
                    severity: Some(3),
                    code: Some(4),
                    message: Some(5),
                    ..p(r"^(\S.*)\((\d+|\d+,\d+|\d+,\d+,\d+,\d+)\):\s+(error|warning|info)\s+(BC\d+)\s*:\s*(.*)$")
                }),
            ),
            (
                "lessCompile",
                single(ProblemPattern {
                    kind: Some(Location),
                    message: Some(1),
                    file: Some(2),
                    line: Some(3),
                    ..p(r"^\s*(.*) in file (.*) line no. (\d+)$")
                }),
            ),
            (
                "jshint",
                single(ProblemPattern {
                    kind: Some(Location),
                    file: Some(1),
                    line: Some(2),
                    character: Some(3),
                    message: Some(4),
                    severity: Some(5),
                    code: Some(6),
                    ..p(r"^(.*):\s+line\s+(\d+),\s+col\s+(\d+),\s(.+?)(?:\s+\((\w)(\d+)\))?$")
                }),
            ),
            (
                "jshint-stylish",
                PatternSet::Multi(vec![
                    ProblemPattern { kind: Some(Location), file: Some(1), ..p(r"^(.+)$") },
                    ProblemPattern {
                        line: Some(1),
                        character: Some(2),
                        message: Some(3),
                        severity: Some(4),
                        code: Some(5),
                        loops: true,
                        ..p(r"^\s+line\s+(\d+)\s+col\s+(\d+)\s+(.+?)(?:\s+\((\w)(\d+)\))?$")
                    },
                ]),
            ),
            (
                "eslint-compact",
                single(ProblemPattern {
                    file: Some(1),
                    kind: Some(Location),
                    line: Some(2),
                    character: Some(3),
                    severity: Some(4),
                    message: Some(5),
                    code: Some(6),
                    ..p(r"^(.+):\sline\s(\d+),\scol\s(\d+),\s(Error|Warning|Info)\s-\s(.+)\s\((.+)\)$")
                }),
            ),
            (
                "eslint-stylish",
                PatternSet::Multi(vec![
                    ProblemPattern { kind: Some(Location), file: Some(1), ..p(r"^((?:[a-zA-Z]:)*[./\\]+.*?)$") },
                    ProblemPattern {
                        line: Some(1),
                        character: Some(2),
                        severity: Some(3),
                        message: Some(4),
                        code: Some(5),
                        loops: true,
                        ..p(r"^\s+(\d+):(\d+)\s+(error|warning|info)\s+(.+?)(?:\s\s+(.*))?$")
                    },
                ]),
            ),
            (
                "go",
                single(ProblemPattern {
                    kind: Some(Location),
                    file: Some(2),
                    line: Some(4),
                    character: Some(6),
                    message: Some(7),
                    ..p(r"^([^:]*: )?((.:)?[^:]*):(\d+)(:(\d+))?: (.*)$")
                }),
            ),
        ];
        for (name, set) in defs {
            self.patterns.insert(name.to_owned(), set);
        }
    }

    fn fill_default_matchers(&mut self) {
        let workspace = || FilePrefix::Path("${workspaceFolder}".to_owned());
        // (name, label, owner, source, applyTo, fileLocation, filePrefix, severity, deprecated)
        let defs = [
            ("msCompile", "Microsoft compiler problems", "msCompile", "cpp", ApplyTo::AllDocuments, FileLocation::Absolute, FilePrefix::None, None, false),
            ("lessCompile", "Less problems", "lessCompile", "less", ApplyTo::AllDocuments, FileLocation::Absolute, FilePrefix::None, Some(TaskSeverity::Error), true),
            ("gulp-tsc", "Gulp TSC Problems", "typescript", "ts", ApplyTo::ClosedDocuments, FileLocation::Relative, workspace(), None, false),
            ("jshint", "JSHint problems", "jshint", "jshint", ApplyTo::AllDocuments, FileLocation::Absolute, FilePrefix::None, None, false),
            ("jshint-stylish", "JSHint stylish problems", "jshint", "jshint", ApplyTo::AllDocuments, FileLocation::Absolute, FilePrefix::None, None, false),
            ("eslint-compact", "ESLint compact problems", "eslint", "eslint", ApplyTo::AllDocuments, FileLocation::Absolute, workspace(), None, false),
            ("eslint-stylish", "ESLint stylish problems", "eslint", "eslint", ApplyTo::AllDocuments, FileLocation::Absolute, FilePrefix::None, None, false),
            ("go", "Go problems", "go", "go", ApplyTo::AllDocuments, FileLocation::Relative, workspace(), None, false),
        ];
        for (name, label, owner, source, apply_to, file_location, file_prefix, severity, deprecated) in defs {
            let Some(pattern) = self.patterns.get(name).cloned() else { continue };
            self.matchers.insert(
                name.to_owned(),
                ProblemMatcher {
                    name: Some(name.to_owned()),
                    label: Some(label.to_owned()),
                    owner: owner.to_owned(),
                    source: Some(source.to_owned()),
                    apply_to,
                    file_location,
                    file_prefix,
                    pattern,
                    severity,
                    watching: None,
                    deprecated,
                    search_files: None,
                },
            );
        }
    }
}

/// The entries of `contributes.<key>` of every extension, in scan order.
fn contributed<'a>(extensions: &'a [Value], key: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
    extensions.iter().filter_map(move |e| e.get("contributes")?.get(key)?.as_array()).flatten()
}

/// `createWatchingPattern`: a string or `{ regexp, file }`; `file` is 1 unless given.
fn watching_pattern(value: Option<&Value>, errors: &mut Vec<String>) -> Option<WatchingPattern> {
    let (source, file) = match value? {
        Value::Null => return None,
        Value::String(s) => (s.as_str(), None),
        Value::Object(o) => (o.get("regexp").and_then(Value::as_str).unwrap_or(""), obj_usize(o, "file")),
        _ => return None,
    };
    if source.is_empty() {
        return None;
    }
    let regexp = Re::new(source).map_err(|e| errors.push(e)).ok()?;
    Some(WatchingPattern { regexp, file: Some(file.filter(|f| *f != 0).unwrap_or(1)) })
}

/// `ProblemPatternParser.parse`: a pattern, a list of them, or a named list (`{ name, patterns }`).
fn parse_pattern(value: &Value, errors: &mut Vec<String>) -> Option<PatternSet> {
    let checked = |v: &Value| v.get("regexp").is_some_and(Value::is_string);
    if let Some(list) = value.get("patterns").and_then(Value::as_array).filter(|_| value.get("name").is_some_and(Value::is_string)) {
        if list.iter().all(checked) {
            return multi_pattern(list, errors).map(PatternSet::Multi);
        }
    }
    if let Some(list) = value.as_array() {
        if list.iter().all(checked) {
            return multi_pattern(list, errors).map(PatternSet::Multi);
        }
    } else if checked(value) {
        let mut p = single_pattern(value.as_object()?, true, errors)?;
        if p.kind.is_none() {
            p.kind = Some(LocationKind::Location);
        }
        return validate(std::slice::from_ref(&p), errors).then_some(PatternSet::Single(p));
    }
    errors.push("The problem pattern is missing a regular expression.".to_owned());
    None
}

fn multi_pattern(values: &[Value], errors: &mut Vec<String>) -> Option<Vec<ProblemPattern>> {
    let mut out = Vec::new();
    for (i, v) in values.iter().enumerate() {
        let mut p = single_pattern(v.as_object()?, false, errors)?;
        if i < values.len() - 1 && p.loops {
            p.loops = false;
            errors.push("The loop property is only supported on the last line matcher.".to_owned());
        }
        out.push(p);
    }
    if out.is_empty() {
        errors.push("The problem pattern is invalid. It must contain at least one pattern.".to_owned());
        return None;
    }
    if out[0].kind.is_none() {
        out[0].kind = Some(LocationKind::Location);
    }
    validate(&out, errors).then_some(out)
}

/// `doCreateSingleProblemPattern`. With `defaults`, a pattern gets `file` 1 and `message` 0, and
/// `line` 2 and `column` 3 when it has no `location` and is not of `kind` `file`.
fn single_pattern(o: &Map<String, Value>, defaults: bool, errors: &mut Vec<String>) -> Option<ProblemPattern> {
    let source = o.get("regexp").and_then(Value::as_str)?;
    let regexp = Re::new(source).map_err(|e| errors.push(e)).ok()?;
    let mut p = ProblemPattern::bare(regexp);
    p.kind = non_empty_str(o, "kind").and_then(LocationKind::from_name);
    p.file = obj_usize(o, "file");
    p.location = obj_usize(o, "location");
    p.line = obj_usize(o, "line");
    p.character = obj_usize(o, "column");
    p.end_line = obj_usize(o, "endLine");
    p.end_character = obj_usize(o, "endColumn");
    p.severity = obj_usize(o, "severity");
    p.code = obj_usize(o, "code");
    p.message = obj_usize(o, "message");
    p.loops = o.get("loop").and_then(Value::as_bool).unwrap_or(false);
    if defaults {
        p.file = p.file.or(Some(1));
        p.message = p.message.or(Some(0));
        if !(p.location.is_some_and(|l| l != 0) || p.kind == Some(LocationKind::File)) {
            p.line = p.line.or(Some(2));
            p.character = p.character.or(Some(3));
        }
    }
    Some(p)
}

/// `validateProblemPattern`: a file and a message somewhere, and a line or location for a pattern
/// of `kind` `location`.
fn validate(values: &[ProblemPattern], errors: &mut Vec<String>) -> bool {
    if values.is_empty() {
        errors.push("The problem pattern is invalid. It must contain at least one pattern.".to_owned());
        return false;
    }
    let kind = values[0].kind.unwrap_or(LocationKind::Location);
    let mut file = false;
    let mut message = false;
    let mut location = false;
    let mut line = false;
    for (i, p) in values.iter().enumerate() {
        if i != 0 && p.kind.is_some() {
            errors.push("The problem pattern is invalid. The kind property must be provided only in the first element".to_owned());
        }
        file |= p.file.is_some();
        message |= p.message.is_some();
        location |= p.location.is_some();
        line |= p.line.is_some();
    }
    if !(file && message) {
        errors.push("The problem pattern is invalid. It must have at least have a file and a message.".to_owned());
        return false;
    }
    if kind == LocationKind::Location && !(location || line) {
        errors.push("The problem pattern is invalid. It must either have kind: \"file\" or have a line or location match group.".to_owned());
        return false;
    }
    true
}
