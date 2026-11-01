//! Terminal UI helpers: the interactive match picker and display formatting.

use std::io::IsTerminal;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use console::style;
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Input, Select};
use indicatif::{ProgressBar, ProgressStyle};
use mrn_core::pipeline::{Choice, ChoiceRequest, Chooser};

/// True when both stdin and stderr are terminals (prompts are possible).
pub fn interactive_terminal() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

pub fn spinner() -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    if interactive_terminal() {
        pb.enable_steady_tick(Duration::from_millis(100));
    } else {
        pb.set_draw_target(indicatif::ProgressDrawTarget::hidden());
    }
    pb
}

pub fn progress_bar(len: u64) -> ProgressBar {
    let pb = ProgressBar::new(len);
    pb.set_style(
        ProgressStyle::with_template("{bar:30.cyan/blue} {pos}/{len} {wide_msg}")
            .unwrap()
            .progress_chars("━╸─"),
    );
    if !interactive_terminal() {
        pb.set_draw_target(indicatif::ProgressDrawTarget::hidden());
    }
    pb
}

/// Asks the user to pick among uncertain matches.
pub struct PromptChooser {
    pub progress: ProgressBar,
}

impl Chooser for PromptChooser {
    fn choose(&mut self, req: &ChoiceRequest<'_>) -> Choice {
        self.progress.suspend(|| prompt(req))
    }
}

fn prompt(req: &ChoiceRequest<'_>) -> Choice {
    let theme = ColorfulTheme::default();
    let year = req.year.map(|y| format!(" ({y})")).unwrap_or_default();
    eprintln!();
    eprintln!(
        "{} No confident {} match for {}{}",
        style("?").yellow().bold(),
        req.kind,
        style(format!("\"{}\"", req.query)).bold(),
        year
    );
    let shown = req.files.len().min(3);
    for f in &req.files[..shown] {
        eprintln!("    {}", style(file_name(f)).dim());
    }
    if req.files.len() > shown {
        eprintln!("    {}", style(format!("… and {} more", req.files.len() - shown)).dim());
    }

    let mut items: Vec<String> = req
        .candidates
        .iter()
        .take(8)
        .map(|s| {
            let c = &s.candidate;
            format!(
                "{:<45} {:>4}%  {}:{}",
                truncate(&c.label(), 45),
                (s.score * 100.0).round(),
                c.source(),
                c.id
            )
        })
        .collect();
    let search_idx = items.len();
    items.push("Search with a different title…".into());
    items.push("Skip these files".into());

    let selection = Select::with_theme(&theme)
        .with_prompt("Pick a match")
        .items(&items)
        .default(0)
        .interact_opt();
    match selection {
        Ok(Some(i)) if i < search_idx => Choice::Pick(i),
        Ok(Some(i)) if i == search_idx => {
            match Input::<String>::with_theme(&theme)
                .with_prompt("Title to search for")
                .default(req.query.to_string())
                .interact_text()
            {
                Ok(q) if !q.trim().is_empty() => Choice::Search(q.trim().to_string()),
                _ => Choice::Skip,
            }
        }
        _ => Choice::Skip,
    }
}

pub fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// Shorten to `max` characters with an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Show `p` relative to `base` when it is inside it.
pub fn relative(p: &Path, base: &Path) -> String {
    p.strip_prefix(base)
        .map(|r| r.display().to_string())
        .unwrap_or_else(|_| p.display().to_string())
}

/// "5 min ago", "3 days ago".
pub fn ago(unix_secs: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(unix_secs);
    let d = (now - unix_secs).max(0);
    match d {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", d / 60),
        3600..86400 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86400),
    }
}

pub fn warn(msg: &str) {
    eprintln!("{} {msg}", style("warning:").yellow().bold());
}
