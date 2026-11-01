//! Runs every case in `fixtures/filenames.toml` and reports all mismatches at once.

use std::path::Path;

use mrn_core::detect::{detect, resolve_book_numbers};
use mrn_core::parse::parse_path;
use toml::Value;

#[test]
fn parser_corpus() {
    let text = include_str!("fixtures/filenames.toml");
    let doc: toml::Table = toml::from_str(text).expect("corpus parses");
    let cases = doc["case"].as_array().expect("[[case]] entries");
    assert!(cases.len() >= 60, "corpus shrank to {}", cases.len());

    let mut failures = Vec::new();
    for case in cases {
        let case = case.as_table().unwrap();
        let path = case["path"].as_str().unwrap();
        let mut info = parse_path(Path::new(path));
        let kind = detect(&info);
        resolve_book_numbers(&mut info, kind);

        let json = serde_json::to_value(&info).unwrap();
        for (field, expected) in case {
            if field == "path" {
                continue;
            }
            let actual: serde_json::Value = match field.as_str() {
                "kind" => serde_json::Value::String(kind.to_string()),
                "date" => info
                    .date
                    .map(|d| serde_json::Value::String(d.to_string()))
                    .unwrap_or(serde_json::Value::Null),
                f => json.get(f).cloned().unwrap_or_else(|| panic!("unknown field {f}")),
            };
            if !matches(expected, &actual) {
                failures.push(format!("{path}\n    {field}: expected {expected}, got {actual}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} parser mismatches:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

fn matches(expected: &Value, actual: &serde_json::Value) -> bool {
    match expected {
        Value::String(s) if s == "none" => actual.is_null(),
        Value::String(s) => actual.as_str() == Some(s.as_str()),
        Value::Integer(i) => actual.as_f64() == Some(*i as f64),
        Value::Float(f) => actual.as_f64() == Some(*f),
        Value::Boolean(b) => actual.as_bool() == Some(*b),
        Value::Array(items) => {
            let Some(arr) = actual.as_array() else { return false };
            arr.len() == items.len() && items.iter().zip(arr).all(|(e, a)| matches(e, a))
        }
        _ => false,
    }
}
