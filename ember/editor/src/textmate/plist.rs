//! A reader for XML property lists, the older format of TextMate grammar files (`*.tmLanguage`).
//!
//! Only what a grammar file uses is read, and into the same value type the JSON reader makes, so that one
//! rule builder serves both formats: `dict`, `array`, `key`, `string`, `integer`, `real`, `true`, `false`,
//! `date` and `data` (the last two as text).
//!
//! A grammar file comes from an extension, so it is input from outside, and XML is a format that has been
//! abused (entity expansion, external entities, deep nesting). This reader is small on purpose:
//!
//! * a document type declaration is accepted only in the form Apple writes it, `<!DOCTYPE plist PUBLIC "..." "...">`,
//!   and is never fetched; one with an internal subset (`[`) is refused, so no entity can be declared;
//! * the only entities are `&amp; &lt; &gt; &quot; &apos;` and numeric references; anything else is an error;
//! * the file size, the number of elements and the nesting depth are capped;
//! * it works with an explicit stack, not recursion, and an error names a line and a kind, never the text.

use serde_json::{Map, Value};

/// The largest property list read (the same cap as a JSON grammar).
pub const MAX_PLIST_BYTES: usize = 4 * 1024 * 1024;
/// The deepest nesting of `dict` and `array`.
pub const MAX_PLIST_DEPTH: usize = 64;
/// The most elements in one document.
pub const MAX_PLIST_NODES: usize = 200_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlistError {
    #[error("property list is larger than {MAX_PLIST_BYTES} bytes")]
    TooLarge,
    #[error("property list is nested deeper than {MAX_PLIST_DEPTH} levels")]
    TooDeep,
    #[error("property list has more than {MAX_PLIST_NODES} elements")]
    TooManyNodes,
    #[error("property list declares a document type with an internal subset (not read)")]
    Dtd,
    #[error("property list uses an entity that is not one of the five predefined ones or a numeric reference (line {0})")]
    Entity(usize),
    #[error("property list is not well formed or not a property list (line {0})")]
    Syntax(usize),
}

enum Frame {
    Dict { map: Map<String, Value>, key: Option<String> },
    Array(Vec<Value>),
}

struct Reader<'a> {
    text: &'a str,
    at: usize,
    nodes: usize,
}

impl<'a> Reader<'a> {
    fn line(&self) -> usize {
        self.text.as_bytes()[..self.at.min(self.text.len())].iter().filter(|b| **b == b'\n').count() + 1
    }

    fn syntax(&self) -> PlistError {
        PlistError::Syntax(self.line())
    }

    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn skip_space(&mut self) {
        let trimmed = self.rest().trim_start_matches([' ', '\t', '\r', '\n']);
        self.at = self.text.len() - trimmed.len();
    }

    fn count(&mut self) -> Result<(), PlistError> {
        self.nodes += 1;
        if self.nodes > MAX_PLIST_NODES {
            return Err(PlistError::TooManyNodes);
        }
        Ok(())
    }

    /// Past `needle`, or a syntax error when it does not occur.
    fn skip_past(&mut self, needle: &str) -> Result<(), PlistError> {
        match self.rest().find(needle) {
            Some(i) => {
                self.at += i + needle.len();
                Ok(())
            }
            None => Err(self.syntax()),
        }
    }

    /// The name of the tag at `<` (or `</`), the position after it, and whether it closes and self-closes. Attributes
    /// are skipped (quotes respected) up to the `>`.
    fn tag(&mut self) -> Result<(bool, String, bool), PlistError> {
        let bytes = self.text.as_bytes();
        debug_assert_eq!(bytes[self.at], b'<');
        self.at += 1;
        let closing = bytes.get(self.at) == Some(&b'/');
        if closing {
            self.at += 1;
        }
        let start = self.at;
        while self.at < bytes.len() && (bytes[self.at].is_ascii_alphanumeric() || bytes[self.at] == b'_') {
            self.at += 1;
        }
        if self.at == start {
            return Err(self.syntax());
        }
        let name = self.text[start..self.at].to_owned();
        let mut quote: Option<u8> = None;
        let mut self_closing = false;
        loop {
            let Some(&b) = bytes.get(self.at) else { return Err(self.syntax()) };
            self.at += 1;
            match (quote, b) {
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, b'"') | (None, b'\'') => quote = Some(b),
                (None, b'>') => break,
                (None, b'/') if bytes.get(self.at) == Some(&b'>') => {
                    self_closing = true;
                    self.at += 1;
                    break;
                }
                (None, b'<') => return Err(self.syntax()),
                _ => {}
            }
        }
        if closing && self_closing {
            return Err(self.syntax());
        }
        Ok((closing, name, self_closing))
    }

    /// The decoded text of an element up to its closing tag `</name>`.
    fn text_of(&mut self, name: &str) -> Result<String, PlistError> {
        let mut out = String::new();
        loop {
            let rest = self.rest();
            let Some(c) = rest.chars().next() else { return Err(self.syntax()) };
            if rest.starts_with("<![CDATA[") {
                self.at += "<![CDATA[".len();
                let end = self.rest().find("]]>").ok_or_else(|| self.syntax())?;
                out.push_str(&self.rest()[..end]);
                self.at += end + 3;
            } else if c == '<' {
                let (closing, found, self_closing) = self.tag()?;
                if !closing || self_closing || found != name {
                    return Err(self.syntax());
                }
                return Ok(out);
            } else if c == '&' {
                let semi = rest.find(';').filter(|i| *i <= 12).ok_or(PlistError::Entity(self.line()))?;
                let entity = &rest[1..semi];
                let ch = match entity {
                    "amp" => '&',
                    "lt" => '<',
                    "gt" => '>',
                    "quot" => '"',
                    "apos" => '\'',
                    _ => {
                        let number = if let Some(hex) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
                            u32::from_str_radix(hex, 16).ok()
                        } else if let Some(dec) = entity.strip_prefix('#') {
                            dec.parse::<u32>().ok()
                        } else {
                            None
                        };
                        number.and_then(char::from_u32).filter(|c| *c != '\0').ok_or(PlistError::Entity(self.line()))?
                    }
                };
                out.push(ch);
                self.at += semi + 1;
            } else {
                out.push(c);
                self.at += c.len_utf8();
            }
        }
    }
}

/// Where a finished value goes: into the open dict (under the key just read), the open array, or the document.
fn put(stack: &mut [Frame], root: &mut Option<Value>, value: Value, line: impl FnOnce() -> usize) -> Result<(), PlistError> {
    match stack.last_mut() {
        Some(Frame::Dict { map, key }) => {
            let k = key.take().ok_or_else(|| PlistError::Syntax(line()))?;
            map.insert(k, value);
        }
        Some(Frame::Array(items)) => items.push(value),
        None => {
            if root.is_some() {
                return Err(PlistError::Syntax(line()));
            }
            *root = Some(value);
        }
    }
    Ok(())
}

/// Reads an XML property list into a JSON value (`dict` as an object, `array` as an array, numbers as numbers,
/// `date` and `data` as text).
pub fn parse_plist(text: &str) -> Result<Value, PlistError> {
    if text.len() > MAX_PLIST_BYTES {
        return Err(PlistError::TooLarge);
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut r = Reader { text, at: 0, nodes: 0 };
    let mut stack: Vec<Frame> = Vec::new();
    let mut root: Option<Value> = None;
    let mut in_plist = false;
    let mut closed_plist = false;
    loop {
        r.skip_space();
        if r.at >= text.len() {
            break;
        }
        if text.as_bytes()[r.at] != b'<' {
            return Err(r.syntax());
        }
        let rest = r.rest();
        if rest.starts_with("<?") {
            r.skip_past("?>")?;
            continue;
        }
        if rest.starts_with("<!--") {
            r.skip_past("-->")?;
            continue;
        }
        if rest.starts_with("<!DOCTYPE") || rest.starts_with("<!doctype") {
            // Only the external form, and only before the plist: no internal subset can declare an entity.
            let close = rest.find('>').ok_or_else(|| r.syntax())?;
            let declaration = &rest[..close];
            if declaration.contains('[') || in_plist || !declaration[9..].trim_start().starts_with("plist") {
                return Err(PlistError::Dtd);
            }
            r.at += close + 1;
            continue;
        }
        if rest.starts_with("<!") {
            return Err(PlistError::Dtd);
        }
        let tag_at = r.at;
        let (closing, name, self_closing) = r.tag()?;
        let line = || r.text.as_bytes()[..tag_at.min(r.text.len())].iter().filter(|b| **b == b'\n').count() + 1;
        r.count()?;
        if closing {
            match name.as_str() {
                "plist" if in_plist && !closed_plist && stack.is_empty() => closed_plist = true,
                "dict" => match stack.pop() {
                    Some(Frame::Dict { map, key: None }) => put(&mut stack, &mut root, Value::Object(map), line)?,
                    _ => return Err(PlistError::Syntax(line())),
                },
                "array" => match stack.pop() {
                    Some(Frame::Array(items)) => put(&mut stack, &mut root, Value::Array(items), line)?,
                    _ => return Err(PlistError::Syntax(line())),
                },
                _ => return Err(PlistError::Syntax(line())),
            }
            continue;
        }
        if name == "plist" {
            if in_plist || self_closing {
                return Err(PlistError::Syntax(line()));
            }
            in_plist = true;
            continue;
        }
        if !in_plist || closed_plist {
            return Err(PlistError::Syntax(line()));
        }
        match name.as_str() {
            "dict" | "array" => {
                if self_closing {
                    let empty = if name == "dict" { Value::Object(Map::new()) } else { Value::Array(Vec::new()) };
                    put(&mut stack, &mut root, empty, line)?;
                } else {
                    if stack.len() >= MAX_PLIST_DEPTH {
                        return Err(PlistError::TooDeep);
                    }
                    stack.push(if name == "dict" { Frame::Dict { map: Map::new(), key: None } } else { Frame::Array(Vec::new()) });
                }
            }
            "key" => {
                let key = if self_closing { String::new() } else { r.text_of("key")? };
                match stack.last_mut() {
                    Some(Frame::Dict { key: pending @ None, .. }) => *pending = Some(key),
                    _ => return Err(PlistError::Syntax(line())),
                }
            }
            "string" | "date" | "data" => {
                let value = if self_closing { String::new() } else { r.text_of(&name)? };
                put(&mut stack, &mut root, Value::String(value), line)?;
            }
            "integer" => {
                let body = if self_closing { String::new() } else { r.text_of("integer")? };
                let n: i64 = body.trim().parse().map_err(|_| PlistError::Syntax(line()))?;
                put(&mut stack, &mut root, Value::from(n), line)?;
            }
            "real" => {
                let body = if self_closing { String::new() } else { r.text_of("real")? };
                let n: f64 = body.trim().parse().map_err(|_| PlistError::Syntax(line()))?;
                let number = serde_json::Number::from_f64(n).ok_or_else(|| PlistError::Syntax(line()))?;
                put(&mut stack, &mut root, Value::Number(number), line)?;
            }
            "true" | "false" => {
                if !self_closing {
                    // `<true></true>` is the same value written out.
                    let (closing, found, _) = {
                        r.skip_space();
                        if text.as_bytes().get(r.at) != Some(&b'<') {
                            return Err(PlistError::Syntax(line()));
                        }
                        r.tag()?
                    };
                    if !closing || found != name {
                        return Err(PlistError::Syntax(line()));
                    }
                }
                put(&mut stack, &mut root, Value::Bool(name == "true"), line)?;
            }
            _ => return Err(PlistError::Syntax(line())),
        }
    }
    if !stack.is_empty() || !closed_plist {
        return Err(r.syntax());
    }
    root.ok_or_else(|| r.syntax())
}
