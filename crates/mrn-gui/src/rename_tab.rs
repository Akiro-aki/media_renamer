//! The Rename tab: sources, options, preview, uncertain matches, apply.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{
    Column, button, checkbox, column, container, pick_list, progress_bar, row, scrollable, space, text,
    text_input,
};
use iced::{Center, Element, Fill, Task};
use mrn_core::MediaKind;
use mrn_core::config::{Config, expand_tilde};
use mrn_core::format::presets::PRESETS;
use mrn_core::history::History;
use mrn_core::ops::{Action, Conflict};
use mrn_core::pipeline::Choice;
use mrn_core::preview::{Preview, RowState};

use crate::widgets::{bad, badge, card, dim, ok, truncate, warn};
use crate::worker::{self, Command, Event, Finished, Job, LogKind, Prompt, Request};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindOpt(Option<MediaKind>);

const KINDS: [KindOpt; 6] = [
    KindOpt(None),
    KindOpt(Some(MediaKind::Movie)),
    KindOpt(Some(MediaKind::Tv)),
    KindOpt(Some(MediaKind::Anime)),
    KindOpt(Some(MediaKind::Comic)),
    KindOpt(Some(MediaKind::Manga)),
];

impl fmt::Display for KindOpt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            None => "Detect",
            Some(MediaKind::Movie) => "Movie",
            Some(MediaKind::Tv) => "TV",
            Some(MediaKind::Anime) => "Anime",
            Some(MediaKind::Comic) => "Comic",
            Some(MediaKind::Manga) => "Manga",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionOpt(Action);

const ACTIONS: [ActionOpt; 4] = [
    ActionOpt(Action::Move),
    ActionOpt(Action::Copy),
    ActionOpt(Action::Hardlink),
    ActionOpt(Action::Symlink),
];

impl fmt::Display for ActionOpt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            Action::Move => "Move / rename",
            Action::Copy => "Copy",
            Action::Hardlink => "Hard link",
            Action::Symlink => "Symbolic link",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConflictOpt(Conflict);

const CONFLICTS: [ConflictOpt; 3] = [
    ConflictOpt(Conflict::Skip),
    ConflictOpt(Conflict::Index),
    ConflictOpt(Conflict::Override),
];

impl fmt::Display for ConflictOpt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            Conflict::Skip => "Skip the file",
            Conflict::Index => "Add a number",
            Conflict::Override => "Replace it",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatOpt {
    Config,
    Preset(&'static str),
    Custom,
}

impl fmt::Display for FormatOpt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatOpt::Config => f.write_str("From config"),
            FormatOpt::Preset(p) => {
                let mut c = p.chars();
                let first = c.next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
                write!(f, "{first}{}", c.as_str())
            }
            FormatOpt::Custom => f.write_str("Custom template"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Flag {
    Recursive,
    Offline,
    Nfo,
    Artwork,
    ComicInfo,
    EmbedCover,
    Clean,
}

#[derive(Debug, Clone)]
pub enum Msg {
    AddFiles,
    AddFolder,
    Picked(Vec<PathBuf>),
    RemoveSource(usize),
    ClearSources,
    BrowseOutput,
    OutputPicked(Option<PathBuf>),
    OutputChanged(String),
    Kind(KindOpt),
    Format(FormatOpt),
    Template(String),
    Action(ActionOpt),
    Conflict(ConflictOpt),
    Toggle(Flag, bool),
    Preview,
    Apply,
    Cancel,
    StartOver,
    Worker(u64, Event),
    Pick(usize),
    SearchText(String),
    Search,
    Skip,
    Undo(i64),
    Undone(Result<String, String>),
}

enum Phase {
    Idle,
    Analyzing,
    Previewed(Arc<Preview>),
    Applying(Arc<Preview>),
    Done(Finished),
}

pub struct State {
    sources: Vec<PathBuf>,
    output: String,
    kind: KindOpt,
    format: FormatOpt,
    formats: Vec<FormatOpt>,
    template: String,
    action: ActionOpt,
    conflict: ConflictOpt,
    recursive: bool,
    offline: bool,
    nfo: bool,
    artwork: bool,
    comicinfo: bool,
    embed_cover: bool,
    clean: bool,

    phase: Phase,
    job: Option<Job>,
    job_id: u64,
    status: String,
    error: Option<String>,
    warnings: Vec<String>,
    prompt: Option<Arc<Prompt>>,
    search: String,
    progress: (usize, usize, String),
    log: Vec<(LogKind, String)>,
    undo_message: Option<Result<String, String>>,
}

impl State {
    pub fn new(sources: Vec<PathBuf>) -> State {
        let config = Config::load(None).unwrap_or_default();
        let mut formats = vec![FormatOpt::Config];
        formats.extend(PRESETS.iter().map(|p| FormatOpt::Preset(p)));
        formats.push(FormatOpt::Custom);
        State {
            sources,
            output: String::new(),
            kind: KindOpt(None),
            format: FormatOpt::Config,
            formats,
            template: String::new(),
            action: ActionOpt(config.general.action),
            conflict: ConflictOpt(config.general.conflict),
            recursive: true,
            offline: false,
            nfo: config.extras.nfo,
            artwork: config.extras.artwork,
            comicinfo: config.extras.comicinfo,
            embed_cover: config.extras.embed_cover,
            clean: false,
            phase: Phase::Idle,
            job: None,
            job_id: 0,
            status: String::new(),
            error: None,
            warnings: Vec::new(),
            prompt: None,
            search: String::new(),
            progress: (0, 0, String::new()),
            log: Vec::new(),
            undo_message: None,
        }
    }

    fn busy(&self) -> bool {
        matches!(self.phase, Phase::Analyzing | Phase::Applying(_))
    }

    /// Options changed: an existing preview no longer matches them.
    fn invalidate(&mut self) {
        if matches!(self.phase, Phase::Analyzing | Phase::Previewed(_) | Phase::Done(_)) {
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.job = None;
        self.phase = Phase::Idle;
        self.prompt = None;
        self.status.clear();
        self.error = None;
        self.warnings.clear();
        self.log.clear();
        self.undo_message = None;
    }

    fn request(&self) -> Request {
        let format = match &self.format {
            FormatOpt::Config => None,
            FormatOpt::Preset(p) => Some(p.to_string()),
            FormatOpt::Custom => Some(self.template.trim().to_string()).filter(|t| !t.is_empty()),
        };
        let output = self.output.trim();
        Request {
            paths: self.sources.clone(),
            recursive: self.recursive,
            kind: self.kind.0,
            format,
            output: (!output.is_empty()).then(|| expand_tilde(std::path::Path::new(output))),
            action: self.action.0,
            conflict: self.conflict.0,
            offline: self.offline,
            nfo: self.nfo,
            artwork: self.artwork,
            comicinfo: self.comicinfo,
            embed_cover: self.embed_cover,
            clean: self.clean,
        }
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::AddFiles => {
                return Task::perform(
                    rfd::AsyncFileDialog::new().set_title("Add media files").pick_files(),
                    |files| {
                        Msg::Picked(
                            files
                                .unwrap_or_default()
                                .iter()
                                .map(|f| f.path().to_path_buf())
                                .collect(),
                        )
                    },
                );
            }
            Msg::AddFolder => {
                return Task::perform(
                    rfd::AsyncFileDialog::new().set_title("Add folders").pick_folders(),
                    |dirs| {
                        Msg::Picked(
                            dirs.unwrap_or_default()
                                .iter()
                                .map(|f| f.path().to_path_buf())
                                .collect(),
                        )
                    },
                );
            }
            Msg::Picked(paths) => {
                if self.busy() || paths.is_empty() {
                    return Task::none();
                }
                for p in paths {
                    if !self.sources.contains(&p) {
                        self.sources.push(p);
                    }
                }
                self.invalidate();
            }
            Msg::RemoveSource(i) => {
                if i < self.sources.len() {
                    self.sources.remove(i);
                }
                self.invalidate();
            }
            Msg::ClearSources => {
                self.sources.clear();
                self.invalidate();
            }
            Msg::BrowseOutput => {
                return Task::perform(
                    rfd::AsyncFileDialog::new().set_title("Library folder").pick_folder(),
                    |dir| Msg::OutputPicked(dir.map(|d| d.path().to_path_buf())),
                );
            }
            Msg::OutputPicked(Some(p)) => {
                self.output = p.display().to_string();
                self.invalidate();
            }
            Msg::OutputPicked(None) => {}
            Msg::OutputChanged(s) => {
                self.output = s;
                self.invalidate();
            }
            Msg::Kind(k) => {
                self.kind = k;
                self.invalidate();
            }
            Msg::Format(f) => {
                self.format = f;
                self.invalidate();
            }
            Msg::Template(t) => {
                self.template = t;
                self.invalidate();
            }
            Msg::Action(a) => {
                self.action = a;
                self.invalidate();
            }
            Msg::Conflict(c) => {
                self.conflict = c;
                self.invalidate();
            }
            Msg::Toggle(flag, on) => {
                let slot = match flag {
                    Flag::Recursive => &mut self.recursive,
                    Flag::Offline => &mut self.offline,
                    Flag::Nfo => &mut self.nfo,
                    Flag::Artwork => &mut self.artwork,
                    Flag::ComicInfo => &mut self.comicinfo,
                    Flag::EmbedCover => &mut self.embed_cover,
                    Flag::Clean => &mut self.clean,
                };
                *slot = on;
                self.invalidate();
            }
            Msg::Preview => {
                if self.sources.is_empty() || self.busy() {
                    return Task::none();
                }
                self.reset();
                self.job_id += 1;
                let id = self.job_id;
                let (job, events) = worker::start(self.request());
                self.job = Some(job);
                self.phase = Phase::Analyzing;
                self.status = "Starting…".into();
                return Task::run(events, move |e| Msg::Worker(id, e));
            }
            Msg::Apply => {
                if let Phase::Previewed(p) = &self.phase
                    && let Some(job) = &self.job
                {
                    job.send(Command::Apply);
                    let total = p.rows.iter().filter(|r| r.state.is_runnable()).map(|r| 1 + r.sidecars).sum();
                    self.progress = (0, total, String::new());
                    self.phase = Phase::Applying(p.clone());
                }
            }
            Msg::Cancel => {
                if !matches!(self.phase, Phase::Applying(_)) {
                    self.reset();
                    self.status = "Cancelled.".into();
                }
            }
            Msg::StartOver => {
                self.reset();
                self.sources.clear();
            }
            Msg::Worker(id, event) => {
                if id == self.job_id {
                    self.on_event(event);
                }
            }
            Msg::Pick(i) => self.answer(Choice::Pick(i)),
            Msg::SearchText(s) => self.search = s,
            Msg::Search => {
                let q = self.search.trim().to_string();
                if !q.is_empty() {
                    self.answer(Choice::Search(q));
                }
            }
            Msg::Skip => self.answer(Choice::Skip),
            Msg::Undo(batch) => {
                self.undo_message = Some(Ok("Undoing…".into()));
                return Task::perform(
                    worker::blocking(move || undo(batch).map_err(|e| format!("{e:#}"))),
                    Msg::Undone,
                );
            }
            Msg::Undone(r) => self.undo_message = Some(r),
        }
        Task::none()
    }

    fn answer(&mut self, choice: Choice) {
        if let Some(job) = &self.job {
            job.send(Command::Choice(choice));
        }
        self.prompt = None;
        self.status = "Searching…".into();
    }

    fn on_event(&mut self, event: Event) {
        match event {
            Event::Status(s) => self.status = s,
            Event::Warning(w) => self.warnings.push(w),
            Event::Choose(p) => {
                self.search = p.query.clone();
                self.prompt = Some(p);
            }
            Event::Preview(p) => {
                self.status.clear();
                self.phase = Phase::Previewed(p);
            }
            Event::Progress { done, total, current } => self.progress = (done, total, current),
            Event::Log(kind, line) => {
                if matches!(kind, LogKind::Failed) {
                    self.progress.0 += 1;
                }
                self.log.push((kind, line));
            }
            Event::Finished(f) => {
                self.job = None;
                self.phase = Phase::Done(f);
            }
            Event::Error(e) => {
                self.job = None;
                self.prompt = None;
                self.phase = Phase::Idle;
                self.status.clear();
                self.error = Some(e);
            }
        }
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let mut page = Column::new().spacing(12);
        page = page.push(self.view_sources());
        page = page.push(self.view_options());

        let can_preview = !self.sources.is_empty() && !self.busy();
        let mut actions = row![
            button(text("Preview").size(15))
                .padding([8, 20])
                .style(button::primary)
                .on_press_maybe(can_preview.then_some(Msg::Preview)),
        ]
        .spacing(10)
        .align_y(Center);
        if matches!(self.phase, Phase::Analyzing | Phase::Previewed(_)) {
            actions = actions.push(button("Cancel").style(button::secondary).on_press(Msg::Cancel));
        }
        if !self.status.is_empty() {
            actions = actions.push(text(&self.status).size(14).style(text::secondary));
        }
        page = page.push(actions);

        if let Some(e) = &self.error {
            page = page.push(card(text(e).style(bad)));
        }
        if let Some(p) = &self.prompt {
            page = page.push(self.view_prompt(p));
        }
        for w in self.warnings.iter().take(5) {
            page = page.push(badge(format!("warning: {w}"), warn));
        }
        if self.warnings.len() > 5 {
            page = page.push(dim(format!("… and {} more warnings", self.warnings.len() - 5)));
        }

        match &self.phase {
            Phase::Idle | Phase::Analyzing => {
                if self.sources.is_empty() {
                    page = page.push(
                        container(dim("Add files or folders, choose where they go, then press Preview."))
                            .center_x(Fill)
                            .padding(30),
                    );
                }
            }
            Phase::Previewed(p) => page = page.push(self.view_preview(p, true)),
            Phase::Applying(p) => {
                let (done, total, current) = &self.progress;
                page = page.push(
                    column![
                        progress_bar(0.0..=(*total).max(1) as f32, *done as f32).girth(10),
                        dim(format!("{done} / {total}  {current}")),
                    ]
                    .spacing(6),
                );
                page = page.push(self.view_log());
                page = page.push(self.view_preview(p, false));
            }
            Phase::Done(f) => page = page.push(self.view_done(f)),
        }
        scrollable(page.padding(iced::Padding::ZERO.right(14))).height(Fill).into()
    }

    fn view_sources(&self) -> Element<'_, Msg> {
        let editable = !self.busy();
        let mut list = Column::new().spacing(4);
        for (i, p) in self.sources.iter().enumerate() {
            list = list.push(
                row![
                    text(p.display().to_string()).size(14).width(Fill),
                    button(text("Remove").size(12))
                        .style(button::text)
                        .on_press_maybe(editable.then_some(Msg::RemoveSource(i))),
                ]
                .align_y(Center),
            );
        }
        let buttons = row![
            button("Add files…").on_press_maybe(editable.then_some(Msg::AddFiles)),
            button("Add folder…").on_press_maybe(editable.then_some(Msg::AddFolder)),
            button("Clear")
                .style(button::secondary)
                .on_press_maybe((editable && !self.sources.is_empty()).then_some(Msg::ClearSources)),
            space().width(Fill),
            checkbox(self.recursive)
                .label("Include subfolders")
                .on_toggle_maybe(editable.then_some(|on| Msg::Toggle(Flag::Recursive, on))),
        ]
        .spacing(8)
        .align_y(Center);
        card(column![text("Files").size(16), list, buttons].spacing(8))
    }

    fn view_options(&self) -> Element<'_, Msg> {
        let editable = !self.busy();
        let label = |s: &'static str| text(s).size(14).width(130);
        let toggle = |on: bool, label: &'static str, flag: Flag| {
            checkbox(on)
                .label(label)
                .on_toggle_maybe(editable.then_some(move |v| Msg::Toggle(flag, v)))
        };

        let mut format_row = row![
            label("Naming"),
            pick_list(self.formats.as_slice(), Some(self.format.clone()), Msg::Format).width(180),
        ]
        .spacing(8)
        .align_y(Center);
        if self.format == FormatOpt::Custom {
            format_row = format_row.push(
                text_input("{n} ({y})/Season {s:02}/{n} - {s00e00}[ - {t}]", &self.template)
                    .on_input_maybe(editable.then_some(Msg::Template))
                    .font(iced::Font::MONOSPACE)
                    .width(Fill),
            );
        }

        let options = column![
            row![
                label("Library folder"),
                text_input("Empty: rename in place, or use the folders set in the config", &self.output)
                    .on_input_maybe(editable.then_some(Msg::OutputChanged))
                    .width(Fill),
                button("Browse…").on_press_maybe(editable.then_some(Msg::BrowseOutput)),
            ]
            .spacing(8)
            .align_y(Center),
            row![
                label("Media type"),
                pick_list(KINDS, Some(self.kind), Msg::Kind).width(180),
                space().width(24),
                text("Action").size(14),
                pick_list(ACTIONS, Some(self.action), Msg::Action).width(160),
                space().width(24),
                text("If the target exists").size(14),
                pick_list(CONFLICTS, Some(self.conflict), Msg::Conflict).width(160),
            ]
            .spacing(8)
            .align_y(Center),
            format_row,
            row![
                label("Options"),
                toggle(self.offline, "Offline (names from file names only)", Flag::Offline),
                toggle(self.clean, "Remove emptied folders", Flag::Clean),
            ]
            .spacing(18)
            .align_y(Center),
            row![
                label("Extras"),
                toggle(self.nfo, ".nfo files", Flag::Nfo),
                toggle(self.artwork, "Artwork", Flag::Artwork),
                toggle(self.comicinfo, "ComicInfo.xml in .cbz", Flag::ComicInfo),
                toggle(self.embed_cover, "Covers in .cbz (Kavita)", Flag::EmbedCover),
            ]
            .spacing(18)
            .align_y(Center),
        ]
        .spacing(10);
        card(options)
    }

    fn view_prompt<'a>(&'a self, p: &'a Prompt) -> Element<'a, Msg> {
        let year = p.year.map(|y| format!(" ({y})")).unwrap_or_default();
        let mut c = Column::new().spacing(8).push(
            text(format!("No confident {} match for “{}”{year}", p.kind, p.query))
                .size(16)
                .style(warn),
        );
        let shown = p.files.len().min(3);
        for f in &p.files[..shown] {
            c = c.push(dim(f.as_str()));
        }
        if p.files.len() > shown {
            c = c.push(dim(format!("… and {} more", p.files.len() - shown)));
        }
        if p.candidates.is_empty() {
            c = c.push(text("No results. Try another title, or skip these files.").size(14));
        }
        for (i, cand) in p.candidates.iter().enumerate() {
            let mut body = column![
                row![
                    text(&cand.label).size(15).width(Fill),
                    dim(cand.id.as_str()),
                    text(format!("{:.0}%", cand.score * 100.0)).size(14).width(50),
                ]
                .spacing(12)
                .align_y(Center)
            ];
            if let Some(o) = &cand.overview {
                body = body.push(dim(truncate(o, 180)));
            }
            c = c.push(button(body).width(Fill).padding([8, 12]).style(button::background).on_press(Msg::Pick(i)));
        }
        c = c.push(
            row![
                text_input("Search with another title", &self.search)
                    .on_input(Msg::SearchText)
                    .on_submit(Msg::Search)
                    .width(Fill),
                button("Search").on_press(Msg::Search),
                button("Skip these files").style(button::secondary).on_press(Msg::Skip),
            ]
            .spacing(8)
            .align_y(Center),
        );
        card(c)
    }

    fn view_preview<'a>(&'a self, p: &'a Preview, can_apply: bool) -> Element<'a, Msg> {
        let mut list = Column::new().spacing(8);
        for r in &p.rows {
            let line: Element<'_, Msg> = match &r.state {
                RowState::Ready | RowState::Overwrite => {
                    let mut l = row![text("→").size(14), text(r.to.as_deref().unwrap_or_default()).size(14).style(ok)]
                        .spacing(6)
                        .align_y(Center);
                    if r.state == RowState::Overwrite {
                        l = l.push(badge("replaces existing file (old one goes to the trash)", bad));
                    }
                    if r.sidecars > 0 {
                        l = l.push(dim(format!("+{} sidecar{}", r.sidecars, if r.sidecars == 1 { "" } else { "s" })));
                    }
                    l = l.push(space().width(Fill));
                    l = l.push(match (&r.matched_id, r.score) {
                        (Some(id), Some(s)) => dim(format!("{id}  {:.0}%", s * 100.0)),
                        _ => dim("from file name"),
                    });
                    l.into()
                }
                RowState::Skipped(reason) => badge(format!("skipped: {reason}"), warn),
                RowState::Unmatched(reason) => badge(reason.as_str(), bad),
            };
            list = list.push(column![dim(r.from.as_str()), line].spacing(2));
        }

        let (verb, _) = self.action.0.verbs(p.in_place);
        let mut footer = row![text(format!("{} file(s): {}", p.rows.len(), p.summary())).size(14).width(Fill)]
            .spacing(10)
            .align_y(Center);
        if can_apply {
            footer = footer.push(
                button(text(format!("{verb} {} file(s)", p.ready)).size(15))
                    .padding([8, 20])
                    .style(button::success)
                    .on_press_maybe((p.ready > 0).then_some(Msg::Apply)),
            );
        }
        card(column![footer, list].spacing(12))
    }

    fn view_log(&self) -> Element<'_, Msg> {
        let mut c = Column::new().spacing(2);
        for (kind, line) in self.log.iter().rev().take(50) {
            c = c.push(match kind {
                LogKind::Failed => badge(format!("failed: {line}"), bad),
                LogKind::Warning => badge(format!("warning: {line}"), warn),
                LogKind::Extra => dim(format!("+ {line}")),
            });
        }
        c.into()
    }

    fn view_done<'a>(&'a self, f: &'a Finished) -> Element<'a, Msg> {
        let extras = if f.extras > 0 {
            format!(", wrote {} extra file(s)", f.extras)
        } else {
            String::new()
        };
        let mut c = Column::new().spacing(10).push(
            text(format!("{} {} file(s){extras}.", f.done_verb, f.done))
                .size(18)
                .style(ok),
        );
        if f.failed > 0 {
            c = c.push(text(format!("{} operation(s) failed.", f.failed)).style(bad));
        }
        c = c.push(self.view_log());
        let undo_done = matches!(&self.undo_message, Some(Ok(m)) if m != "Undoing…");
        c = c.push(
            row![
                button(text(format!("Undo batch #{}", f.batch)))
                    .style(button::danger)
                    .on_press_maybe((!undo_done && self.undo_message.is_none()).then_some(Msg::Undo(f.batch))),
                button("Start over").style(button::primary).on_press(Msg::StartOver),
            ]
            .spacing(10),
        );
        match &self.undo_message {
            Some(Ok(m)) => c = c.push(text(m).size(14)),
            Some(Err(e)) => c = c.push(text(e).size(14).style(bad)),
            None => {}
        }
        card(c)
    }
}

/// Undo a batch and describe the result.
pub fn undo(batch: i64) -> anyhow::Result<String> {
    undo_with(batch, false)
}

pub fn undo_with(batch: i64, force: bool) -> anyhow::Result<String> {
    let h = History::open_default()?;
    let r = h.undo(batch, force)?;
    let mut parts = vec![format!("Batch #{batch} undone: {} restored", r.restored.len())];
    if !r.removed.is_empty() {
        parts.push(format!("{} removed", r.removed.len()));
    }
    if !r.skipped.is_empty() {
        parts.push(format!(
            "{} kept because they changed since (undo again from History with “Undo anyway”)",
            r.skipped.len()
        ));
    }
    let mut msg = parts.join(", ") + ".";
    for w in r.warnings.iter().take(3) {
        msg.push_str(&format!("\n{w}"));
    }
    Ok(msg)
}
