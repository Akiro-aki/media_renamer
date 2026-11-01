//! Make rendered names safe as path components.

/// Clean a single field value before it is inserted into a template.
/// Field values can never introduce folder separators.
pub fn value(s: &str, windows_safe: bool) -> String {
    let s = s.replace(": ", " - ").replace([':', '/'], "-");
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            c if c.is_control() => {}
            '\\' if windows_safe => out.push('-'),
            '"' if windows_safe => out.push('\''),
            '?' | '*' | '<' | '>' | '|' if windows_safe => {}
            c => out.push(c),
        }
    }
    out
}

/// Clean one path component of a rendered template.
pub fn component(s: &str, windows_safe: bool, max_bytes: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut c = collapsed.trim().to_string();
    if windows_safe {
        c = c.trim_end_matches(['.', ' ']).to_string();
    }
    if c == "." || c == ".." {
        c = c.replace('.', "_");
    }
    truncate_bytes(&c, max_bytes).to_string()
}

/// Truncate to at most `max` bytes on a char boundary.
pub fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        assert_eq!(value("Star Wars: Episode IV", true), "Star Wars - Episode IV");
        assert_eq!(value("Fate/Zero", true), "Fate-Zero");
        assert_eq!(value("What If...?", true), "What If...");
        assert_eq!(value("What If...?", false), "What If...?");
    }

    #[test]
    fn components() {
        assert_eq!(component("  What If...  ", true, 255), "What If");
        assert_eq!(component("..", true, 255), "");
        assert_eq!(component("..", false, 255), "__");
        assert_eq!(component("ab  cd", true, 255), "ab cd");
        assert_eq!(component("ééé", false, 3), "é");
    }
}
