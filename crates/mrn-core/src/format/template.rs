//! A small naming template language.
//!
//! - `{field}` / `{field:spec}` inserts a value. Specs: `02` (zero pad),
//!   `upper`, `lower`, `first` (first letter, for A-Z folders).
//! - `[ ... ]` is optional: rendered only if every field inside has a value.
//! - `\x` escapes any character, e.g. `\[tmdbid-{tmdbid}\]`.
//! - `/` separates folders.

use std::fmt;

use super::{Fields, Value, sanitize};

pub const KNOWN_FIELDS: &[&str] = &[
    "n", "y", "id", "tmdbid", "anilistid", "malid", "mangabakaid", "source", "kind", "s", "e",
    "abs", "s00e00", "sxe", "t", "d", "vol", "ch", "issue", "vf", "vs", "vc", "ac", "group", "crc",
    "orig", "ot", "publisher",
];

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Text(String),
    Field { name: String, spec: Spec },
    Optional(Vec<Node>),
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
enum Spec {
    #[default]
    None,
    Pad(usize),
    Upper,
    Lower,
    First,
}

/// A parsed template.
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    source: String,
    nodes: Vec<Node>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    Syntax(String),
    UnknownField(String),
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateError::Syntax(m) => write!(f, "template syntax error: {m}"),
            TemplateError::UnknownField(n) => write!(
                f,
                "unknown template field {{{n}}} (known: {})",
                KNOWN_FIELDS.join(", ")
            ),
        }
    }
}

impl std::error::Error for TemplateError {}

/// A field a template needed but the file had no value for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingField(pub String);

impl Template {
    pub fn parse(source: &str) -> Result<Template, TemplateError> {
        let chars: Vec<char> = source.chars().collect();
        let mut pos = 0;
        let nodes = parse_seq(&chars, &mut pos, false)?;
        Ok(Template {
            source: source.to_string(),
            nodes,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether the template references `name` anywhere.
    pub fn uses_field(&self, name: &str) -> bool {
        fn walk(nodes: &[Node], name: &str) -> bool {
            nodes.iter().any(|n| match n {
                Node::Field { name: f, .. } => f == name,
                Node::Optional(inner) => walk(inner, name),
                Node::Text(_) => false,
            })
        }
        walk(&self.nodes, name)
    }

    /// Render to a `/`-separated relative path (no extension).
    pub fn render(&self, fields: &Fields, windows_safe: bool) -> Result<String, MissingField> {
        render_seq(&self.nodes, fields, windows_safe)
    }
}

fn parse_seq(chars: &[char], pos: &mut usize, in_optional: bool) -> Result<Vec<Node>, TemplateError> {
    let mut nodes = Vec::new();
    let mut text = String::new();
    while *pos < chars.len() {
        let c = chars[*pos];
        match c {
            '\\' => {
                *pos += 1;
                let next = chars
                    .get(*pos)
                    .ok_or_else(|| TemplateError::Syntax("trailing '\\'".into()))?;
                text.push(*next);
                *pos += 1;
            }
            '{' => {
                flush(&mut text, &mut nodes);
                *pos += 1;
                let start = *pos;
                while *pos < chars.len() && chars[*pos] != '}' {
                    *pos += 1;
                }
                if *pos >= chars.len() {
                    return Err(TemplateError::Syntax("unclosed '{'".into()));
                }
                let inner: String = chars[start..*pos].iter().collect();
                *pos += 1;
                nodes.push(parse_field(&inner)?);
            }
            '}' => return Err(TemplateError::Syntax("unexpected '}'".into())),
            '[' => {
                flush(&mut text, &mut nodes);
                *pos += 1;
                let inner = parse_seq(chars, pos, true)?;
                nodes.push(Node::Optional(inner));
            }
            ']' => {
                if !in_optional {
                    return Err(TemplateError::Syntax("unexpected ']' (escape it as \\])".into()));
                }
                flush(&mut text, &mut nodes);
                *pos += 1;
                return Ok(nodes);
            }
            _ => {
                text.push(c);
                *pos += 1;
            }
        }
    }
    if in_optional {
        return Err(TemplateError::Syntax("unclosed '['".into()));
    }
    flush(&mut text, &mut nodes);
    Ok(nodes)
}

fn flush(text: &mut String, nodes: &mut Vec<Node>) {
    if !text.is_empty() {
        nodes.push(Node::Text(std::mem::take(text)));
    }
}

fn parse_field(inner: &str) -> Result<Node, TemplateError> {
    let (name, spec) = match inner.split_once(':') {
        Some((n, s)) => (n.trim(), Some(s.trim())),
        None => (inner.trim(), None),
    };
    if !KNOWN_FIELDS.contains(&name) {
        return Err(TemplateError::UnknownField(name.to_string()));
    }
    let spec = match spec {
        None => Spec::None,
        Some("upper") => Spec::Upper,
        Some("lower") => Spec::Lower,
        Some("first") => Spec::First,
        Some(s) if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) => {
            Spec::Pad(s.parse().unwrap_or(0))
        }
        Some(s) => {
            return Err(TemplateError::Syntax(format!(
                "unknown spec '{s}' in {{{inner}}} (use a pad width like 02, or upper/lower/first)"
            )));
        }
    };
    Ok(Node::Field {
        name: name.to_string(),
        spec,
    })
}

fn render_seq(nodes: &[Node], fields: &Fields, windows_safe: bool) -> Result<String, MissingField> {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Field { name, spec } => {
                let value = fields
                    .get(name)
                    .ok_or_else(|| MissingField(name.clone()))?;
                let text = apply_spec(value, *spec);
                if text.is_empty() {
                    return Err(MissingField(name.clone()));
                }
                out.push_str(&sanitize::value(&text, windows_safe));
            }
            Node::Optional(inner) => {
                if let Ok(s) = render_seq(inner, fields, windows_safe) {
                    out.push_str(&s);
                }
            }
        }
    }
    Ok(out)
}

fn apply_spec(value: &Value, spec: Spec) -> String {
    let pad = match spec {
        Spec::Pad(w) => w,
        _ => 0,
    };
    let text = match value {
        Value::Text(s) => s.clone(),
        Value::Number(n) => format_number(*n, pad),
        Value::Numbers(ns) => ns
            .iter()
            .map(|n| format_number(*n, pad))
            .collect::<Vec<_>>()
            .join("-"),
    };
    match spec {
        Spec::Upper => text.to_uppercase(),
        Spec::Lower => text.to_lowercase(),
        Spec::First => first_letter(&text),
        _ => text,
    }
}

/// `12.0` → `012` (pad 3), `12.5` → `012.5`.
pub fn format_number(n: f64, pad: usize) -> String {
    let int = n.trunc() as i64;
    let mut s = format!("{int:0pad$}");
    let frac = n - n.trunc();
    if frac.abs() > 1e-9 {
        let f = format!("{frac:.3}");
        let f = f.trim_start_matches('0').trim_end_matches('0');
        s.push_str(f);
    }
    s
}

/// First letter for alphabetical folders, ignoring a leading article.
fn first_letter(s: &str) -> String {
    let lower = s.to_lowercase();
    let body = ["the ", "a ", "an "]
        .iter()
        .find_map(|a| lower.strip_prefix(a).map(|_| &s[a.len()..]))
        .unwrap_or(s);
    match body.chars().find(|c| c.is_alphanumeric()) {
        Some(c) if c.is_ascii_digit() => "#".into(),
        Some(c) => c.to_uppercase().collect(),
        None => "#".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> Fields {
        let mut f = Fields::default();
        f.set("n", Value::Text("The Expanse".into()));
        f.set("y", Value::Number(2015.0));
        f.set("s", Value::Number(2.0));
        f.set("e", Value::Numbers(vec![5.0, 6.0]));
        f.set("t", Value::Text("Home: Part 1".into()));
        f
    }

    #[test]
    fn renders_fields_and_padding() {
        let t = Template::parse("{n} ({y})/Season {s:02}/{n} - S{s:02}E{e:02} - {t}").unwrap();
        assert_eq!(
            t.render(&fields(), true).unwrap(),
            "The Expanse (2015)/Season 02/The Expanse - S02E05-06 - Home - Part 1"
        );
    }

    #[test]
    fn optional_sections_drop_when_missing() {
        let t = Template::parse("{n}[ - {vf}][ ({y})]").unwrap();
        assert_eq!(t.render(&fields(), true).unwrap(), "The Expanse (2015)");
    }

    #[test]
    fn required_missing_is_error() {
        let t = Template::parse("{n} {vf}").unwrap();
        assert_eq!(t.render(&fields(), true), Err(MissingField("vf".into())));
    }

    #[test]
    fn escapes_and_specs() {
        let t = Template::parse(r"{n:first}/{n:upper} \[x\]").unwrap();
        assert_eq!(t.render(&fields(), true).unwrap(), "E/THE EXPANSE [x]");
    }

    #[test]
    fn rejects_bad_templates() {
        assert!(matches!(Template::parse("{nope}"), Err(TemplateError::UnknownField(_))));
        assert!(Template::parse("{n").is_err());
        assert!(Template::parse("[{n}").is_err());
        assert!(Template::parse("{n}]").is_err());
        assert!(Template::parse("{n:zz}").is_err());
    }

    #[test]
    fn numbers() {
        assert_eq!(format_number(12.0, 3), "012");
        assert_eq!(format_number(12.5, 3), "012.5");
        assert_eq!(format_number(1100.0, 2), "1100");
    }
}
