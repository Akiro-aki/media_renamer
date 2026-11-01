//! `mrn`: a FileBot-style media renamer for Linux.

mod manage;
mod rename;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand};
use mrn_core::MediaKind;
use mrn_core::ops::{Action, Conflict};
use mrn_core::providers::ForcedId;

#[derive(Parser)]
#[command(
    name = "mrn",
    version,
    about = "Rename and organize movies, TV, anime, comics and manga using online metadata",
    after_help = "Examples:\n  \
        mrn rename ~/Downloads -o ~/Media -n       preview organizing a folder\n  \
        mrn rename show.s01e02.mkv                 rename in place\n  \
        mrn rename -t anime --id anilist:154587 .  force a match\n  \
        mrn undo                                   revert the last batch"
)]
struct Cli {
    /// More output (-v info, -vv debug).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Config file (default: ~/.config/mrn/config.toml).
    #[arg(long, global = true, env = "MRN_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Match files against online databases and rename/move them.
    #[command(visible_alias = "r")]
    Rename(RenameArgs),
    /// Show what mrn reads from a file name and what it would match.
    Info(InfoArgs),
    /// List past batches, or the operations of one batch.
    History {
        /// Batch to show in detail.
        batch: Option<i64>,
        /// How many batches to list.
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// Revert a batch (default: the most recent one).
    Undo {
        batch: Option<i64>,
        /// Undo even if files changed since they were renamed.
        #[arg(long)]
        force: bool,
        /// Don't ask for confirmation.
        #[arg(short, long)]
        yes: bool,
    },
    /// Manage the configuration file.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Manage the metadata cache.
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
    /// Print a shell completion script, e.g. `mrn completions fish > ~/.config/fish/completions/mrn.fish`.
    Completions {
        /// bash, zsh, fish, elvish or powershell.
        shell: clap_complete::Shell,
    },
    /// Print the man page (roff), e.g. `mrn man | man -l -`.
    Man,
    /// Open the graphical interface (mrn-gui), optionally with files or folders.
    Gui { paths: Vec<PathBuf> },
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Write a commented starter config.
    Init {
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Print the effective configuration (API keys masked).
    Show,
    /// Print the config file path.
    Path,
}

#[derive(Subcommand)]
enum CacheAction {
    /// Delete all cached provider responses.
    Clear,
}

#[derive(Args)]
pub struct MatchArgs {
    /// Media type instead of auto-detection.
    #[arg(short = 't', long = "type", value_parser = parse_kind)]
    pub kind: Option<MediaKind>,
    /// Search with this title instead of the one in the file name.
    #[arg(short, long)]
    pub query: Option<String>,
    /// Use this entry instead of searching, e.g. tmdb:1399, anilist:154587.
    #[arg(long, value_parser = parse_forced)]
    pub id: Option<ForcedId>,
    /// No lookups: name files from what is in the file name.
    #[arg(long)]
    pub offline: bool,
    /// Ignore cached metadata and fetch fresh.
    #[arg(long)]
    pub refresh: bool,
}

#[derive(Args)]
pub struct RenameArgs {
    /// Files or folders (folders are searched recursively).
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,

    #[command(flatten)]
    pub matching: MatchArgs,

    /// Preset (jellyfin, plex, kodi, anime, kavita, komga) or template.
    #[arg(short, long)]
    pub format: Option<String>,
    /// Library root. Without it, files are renamed in place.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// move, copy, hardlink or symlink.
    #[arg(short, long, value_parser = parse_action)]
    pub action: Option<Action>,
    /// When a target exists: skip, override or index.
    #[arg(long, value_parser = parse_conflict)]
    pub conflict: Option<Conflict>,
    /// Minimum confidence (0-1) to accept a match without asking.
    #[arg(long)]
    pub threshold: Option<f64>,

    /// Write .nfo files.
    #[arg(long)]
    pub nfo: bool,
    /// Download posters, fanart and covers.
    #[arg(long)]
    pub artwork: bool,
    /// Embed ComicInfo.xml into .cbz files.
    #[arg(long)]
    pub comicinfo: bool,
    /// Turn off extras enabled in the config.
    #[arg(long, conflicts_with_all = ["nfo", "artwork", "comicinfo"])]
    pub no_extras: bool,

    /// Remove source folders left empty after moving.
    #[arg(long)]
    pub clean: bool,
    /// Don't descend into subfolders.
    #[arg(long)]
    pub no_recursive: bool,
    /// Show what would happen without touching any file.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Apply without asking for confirmation.
    #[arg(short, long)]
    pub yes: bool,
    /// Never prompt; skip uncertain matches.
    #[arg(long)]
    pub non_interactive: bool,
}

#[derive(Args)]
pub struct InfoArgs {
    pub file: PathBuf,
    #[command(flatten)]
    pub matching: MatchArgs,
}

fn parse_kind(s: &str) -> Result<MediaKind, String> {
    s.parse()
}
fn parse_forced(s: &str) -> Result<ForcedId, String> {
    s.parse()
}
fn parse_action(s: &str) -> Result<Action, String> {
    s.parse()
}
fn parse_conflict(s: &str) -> Result<Conflict, String> {
    s.parse()
}

fn init_logging(verbose: u8) {
    let level = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug",
    };
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| format!("mrn_core={level},mrn={level}"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .init();
}

/// Replace this process with `mrn-gui`: the one next to this binary (cargo
/// install, AppImage), else the one on `PATH`.
fn launch_gui(paths: &[PathBuf]) -> anyhow::Result<ExitCode> {
    use std::os::unix::process::CommandExt;
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("mrn-gui")))
        .filter(|p| p.is_file());
    let program = sibling.unwrap_or_else(|| PathBuf::from("mrn-gui"));
    let err = std::process::Command::new(&program).args(paths).exec();
    anyhow::bail!(
        "could not start {}: {err} (install it with `cargo install --path crates/mrn-gui`)",
        program.display()
    )
}

/// Write to stdout; a closed pipe (`mrn man | head`) is not an error.
fn print_quietly(bytes: &[u8]) -> anyhow::Result<ExitCode> {
    use std::io::Write;
    match std::io::stdout().lock().write_all(bytes) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e.into()),
        _ => Ok(ExitCode::SUCCESS),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    let result = match cli.command {
        Command::Rename(args) => rename::run(args, cli.config.as_deref()).await,
        Command::Info(args) => rename::info(args, cli.config.as_deref()).await,
        Command::History { batch, limit } => manage::history(batch, limit),
        Command::Undo { batch, force, yes } => manage::undo(batch, force, yes),
        Command::Config { action } => match action {
            ConfigAction::Init { force } => manage::config_init(cli.config.as_deref(), force),
            ConfigAction::Show => manage::config_show(cli.config.as_deref()),
            ConfigAction::Path => manage::config_path(cli.config.as_deref()),
        },
        Command::Cache {
            action: CacheAction::Clear,
        } => manage::cache_clear(),
        Command::Completions { shell } => {
            let mut out = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "mrn", &mut out);
            print_quietly(&out)
        }
        Command::Gui { paths } => launch_gui(&paths),
        Command::Man => {
            let mut out = Vec::new();
            clap_mangen::Man::new(Cli::command())
                .render(&mut out)
                .map_err(anyhow::Error::from)
                .and_then(|()| print_quietly(&out))
        }
    };

    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{} {e:#}", console::style("error:").red().bold());
            ExitCode::FAILURE
        }
    }
}
