//! `mrn history`, `mrn undo`, `mrn config`, `mrn cache`.

use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{ContentArrangement, Table};
use console::style;
use dialoguer::Confirm;
use dialoguer::theme::ColorfulTheme;
use mrn_core::cache::Cache;
use mrn_core::config::Config;
use mrn_core::history::{History, OpKind};

use crate::ui::{self, warn};

pub fn history(batch: Option<i64>, limit: usize) -> Result<ExitCode> {
    let h = History::open_default()?;
    match batch {
        None => {
            let batches = h.batches(limit)?;
            if batches.is_empty() {
                println!("No history yet.");
                return Ok(ExitCode::SUCCESS);
            }
            let mut t = Table::new();
            t.load_style(UTF8_FULL_CONDENSED)
                .set_content_arrangement(ContentArrangement::Dynamic)
                .set_header(["#", "When", "Action", "Files", "Status", "Command"]);
            for b in batches {
                let files = if b.failed > 0 {
                    format!("{} ({} failed)", b.done, b.failed)
                } else {
                    b.done.to_string()
                };
                t.add_row([
                    b.id.to_string(),
                    mrn_core::preview::ago(b.created),
                    b.action,
                    files,
                    b.status,
                    ui::truncate(&b.args, 60),
                ]);
            }
            println!("{t}");
        }
        Some(id) => {
            let ops = h.ops(id)?;
            if ops.is_empty() {
                bail!("no operations recorded for batch #{id}");
            }
            for op in ops {
                let status = match op.status.as_str() {
                    "done" => style(op.status.clone()).green(),
                    "undone" => style(op.status.clone()).dim(),
                    _ => style(op.status.clone()).red(),
                };
                match (&op.from, op.kind) {
                    (Some(from), _) => println!(
                        "[{status}] {} {}\n         → {}",
                        op.kind.as_str(),
                        from.display(),
                        op.to.display()
                    ),
                    (None, _) => println!("[{status}] {} {}", op.kind.as_str(), op.to.display()),
                }
                if let Some(e) = op.error {
                    println!("         {}", style(e).red());
                }
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub fn undo(batch: Option<i64>, force: bool, yes: bool) -> Result<ExitCode> {
    let h = History::open_default()?;
    let id = match batch {
        Some(id) => id,
        None => match h.last_undoable()? {
            Some(id) => id,
            None => {
                println!("Nothing to undo.");
                return Ok(ExitCode::SUCCESS);
            }
        },
    };
    let ops = h.ops(id)?;
    let pending: Vec<_> = ops.iter().filter(|o| o.status == "done").collect();
    if pending.is_empty() {
        println!("Batch #{id} has nothing left to undo.");
        return Ok(ExitCode::SUCCESS);
    }
    let count = |f: fn(&OpKind) -> bool| pending.iter().filter(|o| f(&o.kind)).count();
    let restores = count(|k| *k == OpKind::Move);
    let embeds = count(|k| *k == OpKind::Embed);
    let covers = count(|k| *k == OpKind::Cover);
    let removes = pending.len() - restores - embeds - covers;
    let replaced = pending.iter().filter(|o| o.backup.is_some()).count();
    let mut summary = Vec::new();
    if restores > 0 {
        summary.push(format!("move {restores} file(s) back"));
    }
    if removes > 0 {
        summary.push(format!("delete {removes} created file(s)/link(s)"));
    }
    if replaced > 0 {
        summary.push(format!("restore {replaced} replaced file(s) from the trash"));
    }
    if embeds > 0 {
        summary.push(format!("revert ComicInfo.xml in {embeds} archive(s)"));
    }
    if covers > 0 {
        summary.push(format!("remove the cover added to {covers} archive(s)"));
    }
    println!("Undo batch #{id}: {}", summary.join(", "));

    if !yes {
        if !ui::interactive_terminal() {
            bail!("not asking for confirmation in a non-interactive session; pass --yes");
        }
        let ok = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("Proceed?")
            .default(true)
            .interact_opt()?
            .unwrap_or(false);
        if !ok {
            println!("Cancelled.");
            return Ok(ExitCode::SUCCESS);
        }
    }

    let report = h.undo(id, force)?;
    for (to, from) in &report.restored {
        println!("{} {}", style("restored").green(), from.display());
        tracing::debug!("from {}", to.display());
    }
    for p in &report.removed {
        println!("{} {}", style("removed").green(), p.display());
    }
    for w in &report.warnings {
        warn(w);
    }
    for (p, reason) in &report.skipped {
        eprintln!("{} {}: {reason}", style("kept").yellow(), p.display());
    }
    if report.skipped.is_empty() {
        println!("Batch #{id} undone.");
        Ok(ExitCode::SUCCESS)
    } else {
        eprintln!(
            "{} file(s) were left in place. Run `mrn undo {id} --force` to revert them anyway.",
            report.skipped.len()
        );
        Ok(ExitCode::FAILURE)
    }
}

fn config_file(path: Option<&Path>) -> Result<std::path::PathBuf> {
    match path {
        Some(p) => Ok(p.to_path_buf()),
        None => Config::default_path(),
    }
}

pub fn config_init(path: Option<&Path>, force: bool) -> Result<ExitCode> {
    let path = config_file(path)?;
    if path.exists() && !force {
        bail!("{} already exists (use --force to overwrite)", path.display());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, Config::template()).with_context(|| format!("writing {}", path.display()))?;
    println!("Wrote {}", path.display());
    println!("Add your TMDB API key there (or set TMDB_API_KEY) to match movies and TV.");
    Ok(ExitCode::SUCCESS)
}

pub fn config_show(path: Option<&Path>) -> Result<ExitCode> {
    let mut config = Config::load(path)?;
    let mask = |k: &mut Option<String>| {
        if k.is_some() {
            *k = Some("********".into());
        }
    };
    mask(&mut config.api.tmdb);
    mask(&mut config.api.comicvine);
    mask(&mut config.api.tvdb);
    mask(&mut config.api.tvdb_pin);
    println!("# {}", config_file(path)?.display());
    print!("{}", toml::to_string_pretty(&config)?);
    Ok(ExitCode::SUCCESS)
}

pub fn config_path(path: Option<&Path>) -> Result<ExitCode> {
    println!("{}", config_file(path)?.display());
    Ok(ExitCode::SUCCESS)
}

pub fn cache_clear() -> Result<ExitCode> {
    let path = Cache::default_path()?;
    if !path.exists() {
        println!("Cache is empty.");
        return Ok(ExitCode::SUCCESS);
    }
    let n = Cache::open(&path)?.clear()?;
    println!("Removed {n} cached response(s).");
    Ok(ExitCode::SUCCESS)
}
