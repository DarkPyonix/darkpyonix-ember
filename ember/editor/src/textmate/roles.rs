//! From TextMate scopes to the colour roles the editor widget draws.
//!
//! The widget takes runs painted with a code colour role (keyword, string, comment, ...) and resolves
//! each role in the design system, light and dark. A token carries a stack of scopes, outermost first;
//! the role comes from the innermost scope that has one, the way a VS Code theme picks the most specific
//! rule. A scope that no role covers leaves the text in the default colour: `source.*`, `text.*`, and the
//! `meta.*` scopes, of which only three carry a role (`meta.preprocessor` is a macro, `meta.object-literal.key`
//! a property) because their grammars have nothing more specific to give. Every `variable.*` scope that is
//! not a property, a parameter or a language variable gets the plain variable role.

/// The roles. They mirror the widget's `Syntax*` colour roles one to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SyntaxRole {
    Keyword,
    String,
    Comment,
    Number,
    Constant,
    Type,
    Function,
    Variable,
    Property,
    Operator,
    Punctuation,
    Tag,
    Attribute,
    Escape,
    Macro,
}

/// Scope prefixes and their roles, most specific first inside each family: the first prefix that matches
/// a scope (at a `.` boundary) decides it.
const RULES: &[(&str, Option<SyntaxRole>)] = &[
    ("comment", Some(SyntaxRole::Comment)),
    ("string.regexp", Some(SyntaxRole::String)),
    ("string", Some(SyntaxRole::String)),
    ("constant.numeric", Some(SyntaxRole::Number)),
    ("constant.character.escape", Some(SyntaxRole::Escape)),
    ("constant.character", Some(SyntaxRole::Constant)),
    ("constant.language", Some(SyntaxRole::Constant)),
    ("constant", Some(SyntaxRole::Constant)),
    ("keyword.operator", Some(SyntaxRole::Operator)),
    ("keyword", Some(SyntaxRole::Keyword)),
    ("storage", Some(SyntaxRole::Keyword)),
    ("entity.name.function.macro", Some(SyntaxRole::Macro)),
    ("support.macro", Some(SyntaxRole::Macro)),
    ("meta.preprocessor", Some(SyntaxRole::Macro)),
    ("entity.name.function", Some(SyntaxRole::Function)),
    ("support.function", Some(SyntaxRole::Function)),
    ("entity.name.type", Some(SyntaxRole::Type)),
    ("entity.name.class", Some(SyntaxRole::Type)),
    ("entity.other.inherited-class", Some(SyntaxRole::Type)),
    ("support.type.property-name", Some(SyntaxRole::Property)),
    ("support.type", Some(SyntaxRole::Type)),
    ("support.class", Some(SyntaxRole::Type)),
    ("entity.name.tag", Some(SyntaxRole::Tag)),
    ("entity.other.attribute-name", Some(SyntaxRole::Attribute)),
    ("punctuation.definition.tag", Some(SyntaxRole::Punctuation)),
    ("variable.language", Some(SyntaxRole::Keyword)),
    ("variable.parameter", Some(SyntaxRole::Variable)),
    ("variable.other.property", Some(SyntaxRole::Property)),
    ("variable.other.object.property", Some(SyntaxRole::Property)),
    ("support.variable.property", Some(SyntaxRole::Property)),
    ("meta.object-literal.key", Some(SyntaxRole::Property)),
    ("variable", Some(SyntaxRole::Variable)),
    ("markup.heading", Some(SyntaxRole::Keyword)),
    ("markup.inline.raw", Some(SyntaxRole::String)),
    ("markup.fenced_code", Some(SyntaxRole::String)),
    ("markup.raw", Some(SyntaxRole::String)),
    ("markup.underline.link", Some(SyntaxRole::Constant)),
    ("markup.quote", Some(SyntaxRole::Comment)),
    ("markup.list", Some(SyntaxRole::Punctuation)),
    ("invalid", None),
];

fn matches_prefix(scope: &str, prefix: &str) -> bool {
    scope == prefix || (scope.starts_with(prefix) && scope.as_bytes().get(prefix.len()) == Some(&b'.'))
}

fn role_of(scope: &str) -> Option<SyntaxRole> {
    RULES.iter().find(|(prefix, _)| matches_prefix(scope, prefix)).and_then(|(_, role)| *role)
}

/// The role of a token with these scopes (outermost first): the innermost scope that has one.
pub fn role_for_scopes<S: AsRef<str>>(scopes: &[S]) -> Option<SyntaxRole> {
    scopes.iter().rev().find_map(|s| role_of(s.as_ref()))
}
