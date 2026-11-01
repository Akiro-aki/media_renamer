//! Turn metadata into a destination path using templates.

pub mod presets;
pub mod sanitize;
pub mod template;

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};

use crate::MediaKind;
use crate::config::Config;
pub use template::{MissingField, Template, TemplateError};

/// Longest file stem we produce, leaving room for sidecar suffixes such as
/// `.en.forced.srt` within the usual 255-byte name limit.
const MAX_STEM_BYTES: usize = 230;
const MAX_DIR_BYTES: usize = 255;

/// A value a template field can take.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    Number(f64),
    Numbers(Vec<f64>),
}

/// Named values available to templates.
#[derive(Debug, Clone, Default)]
pub struct Fields(BTreeMap<String, Value>);

impl Fields {
    pub fn set(&mut self, name: &str, value: Value) {
        self.0.insert(name.to_string(), value);
    }

    pub fn set_text(&mut self, name: &str, value: Option<impl Into<String>>) {
        if let Some(v) = value {
            let v = v.into();
            if !v.trim().is_empty() {
                self.set(name, Value::Text(v));
            }
        }
    }

    pub fn set_num(&mut self, name: &str, value: Option<f64>) {
        if let Some(v) = value {
            self.set(name, Value::Number(v));
        }
    }

    pub fn set_nums(&mut self, name: &str, values: &[f64]) {
        if !values.is_empty() {
            self.set(name, Value::Numbers(values.to_vec()));
        }
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.0.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.0.iter()
    }
}

/// The templates to try, in order, for one media kind.
#[derive(Debug, Clone)]
pub struct Formatter {
    templates: Vec<Template>,
    windows_safe: bool,
}

impl Formatter {
    /// Build from a `--format` value (preset name or template), the config's
    /// per-kind template, or the configured preset.
    pub fn for_kind(kind: MediaKind, format_arg: Option<&str>, config: &Config) -> Result<Formatter> {
        let sources: Vec<String> = match format_arg {
            Some(f) if presets::PRESETS.contains(&f) => preset_templates(f, kind)?,
            Some(f) => vec![f.to_string()],
            None => match config.format.get(kind) {
                Some(t) => vec![t.clone()],
                None => preset_templates(&config.general.preset, kind)?,
            },
        };
        let templates = sources
            .iter()
            .map(|s| Template::parse(s).map_err(|e| anyhow!("{e}\n  in template: {s}")))
            .collect::<Result<Vec<_>>>()?;
        Ok(Formatter {
            templates,
            windows_safe: config.general.windows_safe,
        })
    }

    pub fn uses_field(&self, name: &str) -> bool {
        self.templates.iter().any(|t| t.uses_field(name))
    }

    pub fn from_templates(templates: Vec<Template>, windows_safe: bool) -> Formatter {
        Formatter {
            templates,
            windows_safe,
        }
    }

    /// Render a relative destination path (without extension). If
    /// `keep_folders` is false only the last component (the file name) is kept.
    pub fn render(&self, fields: &Fields, keep_folders: bool) -> Result<PathBuf> {
        let mut first_missing = None;
        for t in &self.templates {
            match t.render(fields, self.windows_safe) {
                Ok(s) => return self.to_path(&s, keep_folders),
                Err(MissingField(name)) => {
                    first_missing.get_or_insert(name);
                }
            }
        }
        let name = first_missing.unwrap_or_default();
        bail!(
            "missing value for {{{name}}} (template: {})",
            self.templates
                .first()
                .map(|t| t.source())
                .unwrap_or_default()
        )
    }

    fn to_path(&self, rendered: &str, keep_folders: bool) -> Result<PathBuf> {
        let parts: Vec<&str> = rendered.split('/').collect();
        let last = parts.len() - 1;
        let mut path = PathBuf::new();
        for (i, part) in parts.iter().enumerate() {
            if !keep_folders && i != last {
                continue;
            }
            let max = if i == last { MAX_STEM_BYTES } else { MAX_DIR_BYTES };
            let clean = sanitize::component(part, self.windows_safe, max);
            if !clean.is_empty() {
                path.push(clean);
            }
        }
        if path.as_os_str().is_empty() || rendered.ends_with('/') {
            bail!("template produced an empty file name: '{rendered}'");
        }
        Ok(path)
    }
}

fn preset_templates(preset: &str, kind: MediaKind) -> Result<Vec<String>> {
    presets::templates(preset, kind)
        .map(|v| v.into_iter().map(String::from).collect())
        .ok_or_else(|| {
            anyhow!(
                "unknown preset '{preset}' (available: {})",
                presets::PRESETS.join(", ")
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_and_strips_folders() {
        let cfg = Config::default();
        let f = Formatter::for_kind(MediaKind::Tv, None, &cfg).unwrap();
        let mut fields = Fields::default();
        fields.set_text("n", Some("The Daily Show"));
        fields.set_text("d", Some("2024-01-02"));
        let full = f.render(&fields, true).unwrap();
        assert_eq!(full, PathBuf::from("The Daily Show/The Daily Show - 2024-01-02"));
        let flat = f.render(&fields, false).unwrap();
        assert_eq!(flat, PathBuf::from("The Daily Show - 2024-01-02"));
    }

    #[test]
    fn custom_format_and_errors() {
        let cfg = Config::default();
        assert!(Formatter::for_kind(MediaKind::Tv, Some("nope-preset {x}"), &cfg).is_err());
        let f = Formatter::for_kind(MediaKind::Movie, Some("{n} {vf}"), &cfg).unwrap();
        let mut fields = Fields::default();
        fields.set_text("n", Some("Dune"));
        let err = f.render(&fields, true).unwrap_err().to_string();
        assert!(err.contains("{vf}"), "{err}");
    }
}
