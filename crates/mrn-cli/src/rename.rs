//! `mrn rename` and `mrn info`.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Result, bail};
use console::style;
use dialoguer::Confirm;
use dialoguer::theme::ColorfulTheme;
use mrn_core::config::{Config, Extras, expand_tilde};
use mrn_core::history::History;
use mrn_core::ops::PlanItem;
use mrn_core::pipeline::{Engine, ExecEvent, Options, SkipUncertain};
use mrn_core::preview::{Preview, RowState};
use mrn_core::scan::scan;

use crate::ui::{self, PromptChooser, file_name, warn};
use crate::{InfoArgs, MatchArgs, RenameArgs};

fn apply_match_args(opts: &mut Options, m: &MatchArgs) {
    opts.kind = m.kind;
    opts.query = m.query.clone();
    opts.forced_id = m.id.clone();
    opts.offline = m.offline;
}

pub async fn run(args: RenameArgs, config_path: Option<&Path>) -> Result<ExitCode> {
    let config = Config::load(config_path)?;
    let mut opts = Options::from_config(&config);
    apply_match_args(&mut opts, &args.matching);
    opts.format = args.format.clone();
    opts.output = args.output.as_deref().map(expand_tilde);
    if let Some(a) = args.action {
        opts.action = a;
    }
    if let Some(c) = args.conflict {
        opts.conflict = c;
    }
    if let Some(t) = args.threshold {
        if !(0.0..=1.0).contains(&t) {
            bail!("--threshold must be between 0 and 1");
        }
        opts.threshold = t;
    }
    if args.no_extras {
        opts.extras = Extras::default();
    } else {
        opts.extras.nfo |= args.nfo;
        opts.extras.artwork |= args.artwork;
        opts.extras.comicinfo |= args.comicinfo;
    }
    opts.clean = args.clean;
    opts.interactive = !args.non_interactive && ui::interactive_terminal();

    let scanned = scan(&args.paths, !args.no_recursive)?;
    for f in &scanned.ignored {
        warn(&format!("not a media file, ignored: {}", f.display()));
    }
    if scanned.files.is_empty() {
        bail!("no media files found");
    }

    let engine = Engine::new(config, true, args.matching.refresh)?;
    let spinner = ui::spinner();
    let mut chooser = PromptChooser {
        progress: spinner.clone(),
    };
    let status = |m: &str| spinner.set_message(m.to_string());
    let analysis = engine
        .analyze(scanned.files, &opts, &mut chooser, &status)
        .await?;
    spinner.finish_and_clear();

    let plan = engine.plan(&analysis, &opts);
    let preview = Preview::build(&analysis, &plan);
    let (verb, done_verb) = opts.action.verbs(preview.in_place);
    print_preview(&preview, analysis.items.len());
    for w in &analysis.warnings {
        warn(w);
    }

    let runnable: Vec<&PlanItem> = plan.iter().filter(|p| p.status.is_runnable()).collect();
    if runnable.is_empty() {
        println!("Nothing to do.");
        return Ok(if preview.unmatched > 0 { ExitCode::FAILURE } else { ExitCode::SUCCESS });
    }
    if args.dry_run {
        println!("{}", style("Dry run: no files were changed.").dim());
        return Ok(ExitCode::SUCCESS);
    }
    if !args.yes {
        if !ui::interactive_terminal() {
            bail!("not asking for confirmation in a non-interactive session; pass --yes to apply");
        }
        let ok = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt(format!("{verb} {} file(s)?", runnable.len()))
            .default(true)
            .interact_opt()?
            .unwrap_or(false);
        if !ok {
            println!("Cancelled.");
            return Ok(ExitCode::SUCCESS);
        }
    }

    let history = History::open_default()?;
    let total: usize = runnable.iter().map(|p| 1 + p.sidecars.len()).sum();
    let pb = ui::progress_bar(total as u64);
    let command_line = shell_join(std::iter::once("mrn".to_string()).chain(std::env::args().skip(1)));
    let report = engine
        .execute(&analysis, &plan, &opts, &history, &command_line, &mut |ev| match ev {
            ExecEvent::Done { op } => {
                pb.inc(1);
                pb.set_message(file_name(&op.to));
            }
            ExecEvent::Failed { op, error } => {
                pb.inc(1);
                pb.println(format!(
                    "{} {}: {error}",
                    style("failed").red().bold(),
                    op.from.display()
                ));
            }
            ExecEvent::Extra { path } => {
                pb.println(format!("{} {}", style("+").green(), style(path.display()).dim()));
            }
            ExecEvent::Warning(w) => {
                pb.println(format!("{} {w}", style("warning:").yellow().bold()));
            }
        })
        .await?;
    pb.finish_and_clear();

    let extras = if report.extras > 0 {
        format!(", wrote {} extra file(s)", report.extras)
    } else {
        String::new()
    };
    println!(
        "{} {} file(s){extras}. Undo with {}",
        style(done_verb).green().bold(),
        report.done,
        style(format!("mrn undo {}", report.batch)).cyan()
    );
    if report.failed > 0 {
        eprintln!("{} {} operation(s) failed", style("error:").red().bold(), report.failed);
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

/// Print what will happen to every file.
fn print_preview(preview: &Preview, total: usize) {
    for row in &preview.rows {
        println!("  {}", style(&row.from).dim());
        let to = row.to.as_deref().unwrap_or_default();
        match &row.state {
            RowState::Ready | RowState::Overwrite => {
                let overwrite = if row.state == RowState::Overwrite {
                    style(" (overwrites)").red().to_string()
                } else {
                    String::new()
                };
                let extra = match row.sidecars {
                    0 => String::new(),
                    n => format!("  (+{n} sidecar{})", if n == 1 { "" } else { "s" }),
                };
                let score = match (&row.matched_id, row.score) {
                    (Some(id), Some(s)) => format!("  [{id} {:.0}%]", s * 100.0),
                    _ => "  [from file name]".into(),
                };
                println!(
                    "  {} {}{overwrite}{}{}",
                    style("→").cyan(),
                    style(to).green(),
                    style(extra).dim(),
                    style(score).dim()
                );
            }
            RowState::Skipped(reason) => {
                println!("  {} {}", style("–").yellow(), style(format!("skipped: {reason}")).yellow());
            }
            RowState::Unmatched(reason) => println!("  {} {}", style("✗").red(), style(reason).red()),
        }
    }
    println!();
    println!("{total} file(s): {}", preview.summary());
}

/// Quote arguments containing spaces for the history log.
fn shell_join(args: impl Iterator<Item = String>) -> String {
    args.map(|a| {
        if a.is_empty() || a.contains(|c: char| c.is_whitespace() || "'\"$`\\".contains(c)) {
            format!("'{}'", a.replace('\'', r"'\''"))
        } else {
            a
        }
    })
    .collect::<Vec<_>>()
    .join(" ")
}

pub async fn info(args: InfoArgs, config_path: Option<&Path>) -> Result<ExitCode> {
    let config = Config::load(config_path)?;
    let mut opts = Options::from_config(&config);
    apply_match_args(&mut opts, &args.matching);
    opts.interactive = false;

    let scanned = scan(std::slice::from_ref(&args.file), false)?;
    if scanned.files.is_empty() {
        bail!("{} is not a media file", args.file.display());
    }
    let engine = Engine::new(config, true, args.matching.refresh)?;
    let analysis = engine
        .analyze(scanned.files, &opts, &mut SkipUncertain, &|_| {})
        .await?;
    let item = &analysis.items[0];

    let label = |s: &str| style(format!("{s:>13}")).bold();
    println!("{} {}", label("File"), item.file.path.display());
    let detected = if opts.kind.is_some() { "forced" } else { "detected" };
    println!("{} {} ({detected})", label("Type"), item.kind);

    println!("{}", label("Parsed"));
    if let Ok(serde_json::Value::Object(map)) = serde_json::to_value(&item.info) {
        for (k, v) in map {
            let shown = match &v {
                serde_json::Value::Null | serde_json::Value::Bool(false) => continue,
                serde_json::Value::Array(a) if a.is_empty() => continue,
                serde_json::Value::String(s) if s.is_empty() => continue,
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Object(_) if k == "date" => item
                    .info
                    .date
                    .map(|d| d.to_string())
                    .unwrap_or_default(),
                other => other.to_string(),
            };
            println!("{:>13} {}", style(k).dim(), shown);
        }
    }

    if !opts.offline && opts.forced_id.is_none() {
        let query = opts.query.clone().or_else(|| item.info.title.clone());
        if let Some(q) = query {
            match engine.candidates(item.kind, &q, item.info.year).await {
                Ok(c) if !c.is_empty() => {
                    println!("{}", label("Candidates"));
                    for s in c.iter().take(5) {
                        println!(
                            "{:>13} {}  {}",
                            style(format!("{:.0}%", s.score * 100.0)).dim(),
                            s.candidate.label(),
                            style(format!("{}:{}", s.candidate.source(), s.candidate.id)).dim()
                        );
                    }
                }
                Ok(_) => println!("{} none", label("Candidates")),
                Err(e) => println!("{} {e:#}", label("Candidates")),
            }
        }
    }

    if let Some(m) = &item.matched {
        println!(
            "{} {} {}",
            label("Match"),
            m.candidate.label(),
            style(format!("{}:{} ({:.0}%)", m.candidate.source(), m.candidate.id, m.score * 100.0)).dim()
        );
        for e in &item.episodes {
            println!(
                "{:>13} S{:02}E{:02}{} {}",
                "",
                e.season,
                e.episode,
                e.absolute.map(|a| format!(" (#{a})")).unwrap_or_default(),
                e.title.as_deref().unwrap_or("")
            );
        }
    }
    match &item.target {
        Ok(t) => println!("{} {}", label("Target"), style(t.display()).green()),
        Err(e) => println!("{} {}", label("Target"), style(e).red()),
    }
    for (p, _) in &item.file.sidecars {
        println!("{} {}", label("Sidecar"), p.display());
    }
    for w in &analysis.warnings {
        warn(w);
    }
    Ok(ExitCode::SUCCESS)
}
