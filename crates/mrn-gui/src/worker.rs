//! Background jobs. A rename job runs on its own thread with its own tokio
//! runtime, because [`Chooser::choose`] blocks until the user answers and must
//! not block the GUI's executor.
//!
//! The job reports through an [`Event`] stream and takes [`Command`]s back.
//! Dropping the [`Job`] cancels it: pending questions are answered with
//! "skip" and nothing is applied.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;

use anyhow::{Result, bail};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use mrn_core::MediaKind;
use mrn_core::config::Config;
use mrn_core::history::History;
use mrn_core::ops::{Action, Conflict};
use mrn_core::pipeline::{Choice, ChoiceRequest, Chooser, Engine, ExecEvent, Options};
use mrn_core::preview::Preview;
use mrn_core::scan::scan;

/// What to rename and how. Fields left `None` fall back to the config.
#[derive(Debug, Clone)]
pub struct Request {
    pub paths: Vec<PathBuf>,
    pub recursive: bool,
    pub kind: Option<MediaKind>,
    pub format: Option<String>,
    pub output: Option<PathBuf>,
    pub action: Action,
    pub conflict: Conflict,
    pub offline: bool,
    pub nfo: bool,
    pub artwork: bool,
    pub comicinfo: bool,
    pub clean: bool,
}

impl Request {
    /// An equivalent `mrn` command line, recorded in the history.
    fn command_line(&self) -> String {
        let mut args = vec!["mrn-gui".to_string(), "rename".into()];
        args.extend(self.paths.iter().map(|p| p.display().to_string()));
        if let Some(o) = &self.output {
            args.extend(["-o".into(), o.display().to_string()]);
        }
        if let Some(k) = self.kind {
            args.extend(["-t".into(), k.to_string()]);
        }
        if let Some(f) = &self.format {
            args.extend(["-f".into(), f.clone()]);
        }
        args.extend(["-a".into(), self.action.to_string()]);
        for (on, flag) in [
            (self.offline, "--offline"),
            (self.nfo, "--nfo"),
            (self.artwork, "--artwork"),
            (self.comicinfo, "--comicinfo"),
            (self.clean, "--clean"),
            (!self.recursive, "--no-recursive"),
        ] {
            if on {
                args.push(flag.into());
            }
        }
        args.iter()
            .map(|a| {
                if a.contains(|c: char| c.is_whitespace() || "'\"$`\\".contains(c)) {
                    format!("'{}'", a.replace('\'', r"'\''"))
                } else {
                    a.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// An uncertain match the user has to resolve.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub kind: MediaKind,
    pub query: String,
    pub year: Option<u16>,
    pub files: Vec<String>,
    pub candidates: Vec<CandidateView>,
}

#[derive(Debug, Clone)]
pub struct CandidateView {
    pub label: String,
    pub id: String,
    pub score: f64,
    pub overview: Option<String>,
}

#[derive(Debug, Clone)]
pub enum LogKind {
    Failed,
    Warning,
    Extra,
}

#[derive(Debug, Clone)]
pub struct Finished {
    pub batch: i64,
    pub done: usize,
    pub failed: usize,
    pub extras: usize,
    pub done_verb: &'static str,
}

#[derive(Debug, Clone)]
pub enum Event {
    Status(String),
    Warning(String),
    Choose(Arc<Prompt>),
    Preview(Arc<Preview>),
    Progress { done: usize, total: usize, current: String },
    Log(LogKind, String),
    Finished(Finished),
    Error(String),
}

pub enum Command {
    Choice(Choice),
    Apply,
}

/// A running job. Drop it to cancel.
pub struct Job {
    commands: mpsc::Sender<Command>,
}

impl Job {
    pub fn send(&self, c: Command) {
        let _ = self.commands.send(c);
    }
}

/// Start analyzing `request`. The job stops after the preview until it gets
/// [`Command::Apply`].
pub fn start(request: Request) -> (Job, UnboundedReceiver<Event>) {
    let (events, rx_events) = unbounded();
    let (commands, rx_commands) = mpsc::channel();
    std::thread::Builder::new()
        .name("mrn-job".into())
        .spawn(move || {
            let result = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(anyhow::Error::from)
                .and_then(|rt| rt.block_on(run(request, &events, &rx_commands)));
            if let Err(e) = result {
                let _ = events.unbounded_send(Event::Error(format!("{e:#}")));
            }
        })
        .expect("spawning a thread");
    (Job { commands }, rx_events)
}

async fn run(req: Request, events: &UnboundedSender<Event>, commands: &mpsc::Receiver<Command>) -> Result<()> {
    let send = |e: Event| {
        let _ = events.unbounded_send(e);
    };
    let config = Config::load(None)?;
    let mut opts = Options::from_config(&config);
    opts.kind = req.kind;
    opts.format = req.format.clone();
    opts.output = req.output.clone();
    opts.action = req.action;
    opts.conflict = req.conflict;
    opts.offline = req.offline;
    opts.extras.nfo = req.nfo;
    opts.extras.artwork = req.artwork;
    opts.extras.comicinfo = req.comicinfo;
    opts.clean = req.clean;
    opts.interactive = true;

    send(Event::Status("Scanning…".into()));
    let scanned = scan(&req.paths, req.recursive)?;
    for f in &scanned.ignored {
        send(Event::Warning(format!("not a media file, ignored: {}", f.display())));
    }
    if scanned.files.is_empty() {
        bail!("no media files found");
    }

    let engine = Engine::new(config, true, false)?;
    let mut chooser = GuiChooser { events, commands };
    let status = |m: &str| send(Event::Status(m.to_string()));
    let analysis = engine
        .analyze(scanned.files, &opts, &mut chooser, &status)
        .await?;
    let plan = engine.plan(&analysis, &opts);
    let preview = Preview::build(&analysis, &plan);
    let (_, done_verb) = opts.action.verbs(preview.in_place);
    for w in &analysis.warnings {
        send(Event::Warning(w.clone()));
    }
    send(Event::Preview(Arc::new(preview)));

    // Wait for the go-ahead; a dropped job ends here.
    loop {
        match commands.recv() {
            Ok(Command::Apply) => break,
            Ok(Command::Choice(_)) => continue,
            Err(_) => return Ok(()),
        }
    }

    let history = History::open_default()?;
    let total: usize = plan
        .iter()
        .filter(|p| p.status.is_runnable())
        .map(|p| 1 + p.sidecars.len())
        .sum();
    let mut done = 0;
    let report = engine
        .execute(&analysis, &plan, &opts, &history, &req.command_line(), &mut |ev| match ev {
            ExecEvent::Done { op } => {
                done += 1;
                send(Event::Progress {
                    done,
                    total,
                    current: op
                        .to
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                });
            }
            ExecEvent::Failed { op, error } => {
                done += 1;
                send(Event::Log(LogKind::Failed, format!("{}: {error}", op.from.display())));
            }
            ExecEvent::Extra { path } => send(Event::Log(LogKind::Extra, path.display().to_string())),
            ExecEvent::Warning(w) => send(Event::Log(LogKind::Warning, w)),
        })
        .await?;
    send(Event::Finished(Finished {
        batch: report.batch,
        done: report.done,
        failed: report.failed,
        extras: report.extras,
        done_verb,
    }));
    Ok(())
}

struct GuiChooser<'a> {
    events: &'a UnboundedSender<Event>,
    commands: &'a mpsc::Receiver<Command>,
}

impl Chooser for GuiChooser<'_> {
    fn choose(&mut self, req: &ChoiceRequest<'_>) -> Choice {
        let prompt = Prompt {
            kind: req.kind,
            query: req.query.to_string(),
            year: req.year,
            files: req
                .files
                .iter()
                .map(|f| {
                    f.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| f.display().to_string())
                })
                .collect(),
            candidates: req
                .candidates
                .iter()
                .take(10)
                .map(|s| CandidateView {
                    label: s.candidate.label(),
                    id: format!("{}:{}", s.candidate.source(), s.candidate.id),
                    score: s.score,
                    overview: s.candidate.overview.clone(),
                })
                .collect(),
        };
        if self.events.unbounded_send(Event::Choose(Arc::new(prompt))).is_err() {
            return Choice::Skip;
        }
        loop {
            match self.commands.recv() {
                Ok(Command::Choice(c)) => return c,
                Ok(Command::Apply) => continue,
                Err(_) => return Choice::Skip,
            }
        }
    }
}

/// Run blocking work (history, undo, config) off the GUI thread.
pub async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.expect("worker thread panicked")
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn next(rx: &mut UnboundedReceiver<Event>) -> Event {
        futures::executor::block_on(rx.next()).expect("worker ended without finishing")
    }

    fn offline_request(home: &std::path::Path) -> Request {
        Request {
            paths: vec![home.join("in")],
            recursive: true,
            kind: None,
            format: None,
            output: Some(home.join("lib")),
            action: Action::Move,
            conflict: Conflict::Skip,
            offline: true,
            nfo: false,
            artwork: false,
            comicinfo: false,
            clean: false,
        }
    }

    /// The only test in this binary, so changing the environment is safe.
    #[test]
    fn preview_apply_undo() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", home.join("config"));
            std::env::set_var("XDG_DATA_HOME", home.join("data"));
            std::env::set_var("XDG_CACHE_HOME", home.join("cache"));
            std::env::set_var("MRN_CONFIG", home.join("config/mrn/config.toml"));
        }
        std::fs::create_dir_all(home.join("in")).unwrap();
        std::fs::write(home.join("in/The.Expanse.S02E05.1080p.mkv"), b"x").unwrap();
        std::fs::write(home.join("in/notes.txt"), b"x").unwrap();

        let (job, mut rx) = start(offline_request(home));
        let preview = loop {
            match next(&mut rx) {
                Event::Preview(p) => break p,
                Event::Error(e) => panic!("{e}"),
                _ => {}
            }
        };
        assert_eq!(preview.ready, 1);
        assert_eq!(preview.rows[0].to.as_deref(), Some("The Expanse/Season 02/The Expanse - S02E05.mkv"));

        job.send(Command::Apply);
        let finished = loop {
            match next(&mut rx) {
                Event::Finished(f) => break f,
                Event::Error(e) => panic!("{e}"),
                _ => {}
            }
        };
        assert_eq!((finished.done, finished.failed, finished.done_verb), (1, 0, "Moved"));
        let target = home.join("lib/The Expanse/Season 02/The Expanse - S02E05.mkv");
        assert!(target.exists());

        let msg = crate::rename_tab::undo(finished.batch).unwrap();
        assert!(msg.contains("1 restored"), "{msg}");
        assert!(home.join("in/The.Expanse.S02E05.1080p.mkv").exists());
        assert!(!target.exists());

        // A dropped job stops after the preview without touching files.
        let (job, mut rx) = start(offline_request(home));
        while !matches!(next(&mut rx), Event::Preview(_)) {}
        drop(job);
        assert!(futures::executor::block_on(rx.next()).is_none());
        assert!(home.join("in/The.Expanse.S02E05.1080p.mkv").exists());
    }
}
