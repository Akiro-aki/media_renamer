//! The end-to-end flow shared by the CLI and any future GUI:
//! [`Engine::analyze`] (parse, detect, match, format) → [`Engine::plan`]
//! (conflicts) → [`Engine::execute`] (file operations, history, extras).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use futures::StreamExt;

use crate::MediaKind;
use crate::cache::Cache;
use crate::config::{Config, Extras};
use crate::detect::{detect, resolve_book_numbers};
use crate::extras::{self, ExtraContent, ExtraContext, comicinfo};
use crate::format::{Fields, Formatter};
use crate::history::{History, OpKind};
use crate::matcher::{self, Scored, normalize};
use crate::ops::{self, Action, Conflict, FileOp, ItemStatus, PlanItem, Request};
use crate::parse::{ParsedInfo, parse_path};
use crate::providers::http::{Http, HttpOptions};
use crate::providers::{Candidate, Episode, ForcedId, Issue, Provider, Providers, SeriesInfo, Source};
use crate::scan::MediaFile;

/// Minimum similarity for using a TMDB show's episode list for an AniList entry.
const CROSS_REF_THRESHOLD: f64 = 0.85;

/// Per-run options (CLI flags merged over config).
#[derive(Debug, Clone)]
pub struct Options {
    /// Force the media kind instead of detecting it.
    pub kind: Option<MediaKind>,
    /// Preset name or template.
    pub format: Option<String>,
    /// Library root; without one files are renamed in place.
    pub output: Option<PathBuf>,
    pub action: Action,
    pub conflict: Conflict,
    /// Search with this title instead of the parsed one.
    pub query: Option<String>,
    /// Skip searching and use this entry.
    pub forced_id: Option<ForcedId>,
    /// Name files from the parsed filename only.
    pub offline: bool,
    /// Ask the [`Chooser`] about uncertain matches.
    pub interactive: bool,
    pub threshold: f64,
    pub extras: Extras,
    /// Remove source folders left empty after moving.
    pub clean: bool,
}

impl Options {
    pub fn from_config(config: &Config) -> Options {
        Options {
            kind: None,
            format: None,
            output: None,
            action: config.general.action,
            conflict: config.general.conflict,
            query: None,
            forced_id: None,
            offline: false,
            interactive: true,
            threshold: config.general.threshold,
            extras: config.extras.clone(),
            clean: false,
        }
    }
}

/// The answer to an uncertain match.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    Pick(usize),
    Skip,
    Search(String),
}

pub struct ChoiceRequest<'a> {
    pub kind: MediaKind,
    pub query: &'a str,
    pub year: Option<u16>,
    pub files: Vec<&'a Path>,
    pub candidates: &'a [Scored],
}

/// Resolves uncertain matches, e.g. by asking the user.
pub trait Chooser {
    fn choose(&mut self, request: &ChoiceRequest<'_>) -> Choice;
}

/// A chooser that skips everything uncertain.
pub struct SkipUncertain;

impl Chooser for SkipUncertain {
    fn choose(&mut self, _: &ChoiceRequest<'_>) -> Choice {
        Choice::Skip
    }
}

#[derive(Debug, Clone)]
pub struct Match {
    pub candidate: Arc<Candidate>,
    pub score: f64,
    pub series: Option<Arc<SeriesInfo>>,
}

/// One analyzed file.
#[derive(Debug, Clone)]
pub struct Item {
    pub file: MediaFile,
    pub info: ParsedInfo,
    pub kind: MediaKind,
    pub matched: Option<Match>,
    /// Episodes this file contains, from the provider's episode list.
    pub episodes: Vec<Episode>,
    pub issue: Option<Issue>,
    pub fields: Fields,
    /// Full destination path, or why there is none.
    pub target: Result<PathBuf, String>,
    /// Whether the target is inside a library root (folder layout applies).
    pub organized: bool,
    /// The library root used, if any.
    pub output_root: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Analysis {
    pub items: Vec<Item>,
    pub warnings: Vec<String>,
}

/// Progress reported while executing a plan.
pub enum ExecEvent<'a> {
    Done { op: &'a FileOp },
    Failed { op: &'a FileOp, error: String },
    Extra { path: &'a Path },
    Warning(String),
}

#[derive(Debug, Default)]
pub struct ExecReport {
    pub batch: i64,
    pub done: usize,
    pub failed: usize,
    pub extras: usize,
}

enum GroupState {
    Matched(Match),
    /// Name from the parsed filename (offline, or no provider for comics).
    Local,
    Failed(String),
}

struct Group {
    kind: MediaKind,
    query: String,
    year: Option<u16>,
    members: Vec<usize>,
}

struct SearchResult {
    accepted: Option<Scored>,
    candidates: Vec<Scored>,
}

pub struct Engine {
    pub config: Config,
    pub providers: Providers,
    pub http: Arc<Http>,
}

impl Engine {
    /// Build an engine with the on-disk cache (unless `use_cache` is false).
    pub fn new(config: Config, use_cache: bool, refresh: bool) -> Result<Engine> {
        let cache = if use_cache {
            match Cache::default_path().and_then(|p| Cache::open(&p)) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    tracing::warn!("cache disabled: {e:#}");
                    None
                }
            }
        } else {
            None
        };
        let http = Arc::new(Http::new(HttpOptions {
            cache,
            ttl: Duration::from_secs(config.general.cache_ttl_hours * 3600),
            refresh,
        })?);
        let providers = Providers::new(&config, http.clone());
        Ok(Engine {
            config,
            providers,
            http,
        })
    }

    pub fn with_providers(config: Config, providers: Providers, http: Arc<Http>) -> Engine {
        Engine {
            config,
            providers,
            http,
        }
    }

    /// Parse, match and name every file.
    pub async fn analyze(
        &self,
        files: Vec<MediaFile>,
        opts: &Options,
        chooser: &mut dyn Chooser,
        status: &dyn Fn(&str),
    ) -> Result<Analysis> {
        let mut warnings = Vec::new();
        let concurrency = self.config.general.concurrency.max(1);

        // 1. Parse and detect.
        let parsed: Vec<(MediaFile, ParsedInfo, MediaKind)> = files
            .into_iter()
            .map(|f| {
                let mut info = parse_path(&f.path);
                let kind = opts.kind.unwrap_or_else(|| detect(&info));
                resolve_book_numbers(&mut info, kind);
                (f, info, kind)
            })
            .collect();

        // 2. Group files that share a title so each is looked up once.
        let mut groups: Vec<Group> = Vec::new();
        let mut index: HashMap<(MediaKind, String, Option<u16>), usize> = HashMap::new();
        let mut group_of: Vec<Option<usize>> = Vec::with_capacity(parsed.len());
        for (i, (_, info, kind)) in parsed.iter().enumerate() {
            let Some(query) = opts.query.clone().or_else(|| info.title.clone()) else {
                group_of.push(None);
                continue;
            };
            let key = (*kind, normalize(&query), info.year);
            let gi = *index.entry(key).or_insert_with(|| {
                groups.push(Group {
                    kind: *kind,
                    query,
                    year: info.year,
                    members: Vec::new(),
                });
                groups.len() - 1
            });
            groups[gi].members.push(i);
            group_of.push(Some(gi));
        }

        // 3. Search every group concurrently.
        if !opts.offline && !groups.is_empty() {
            status(&format!("Looking up {} title(s)…", groups.len()));
        }
        let mut outcomes: Vec<(usize, Result<GroupState, SearchNeedsChoice>)> =
            futures::stream::iter(groups.iter().enumerate().map(|(gi, g)| async move {
                (gi, self.search_group(g, opts).await)
            }))
            .buffer_unordered(concurrency)
            .collect()
            .await;
        outcomes.sort_by_key(|(gi, _)| *gi);

        // 4. Resolve uncertain matches one at a time (may ask the user).
        let mut states: Vec<GroupState> = Vec::with_capacity(groups.len());
        for (gi, outcome) in outcomes {
            let g = &groups[gi];
            let state = match outcome {
                Ok(state) => state,
                Err(SearchNeedsChoice::Warn(msg, state)) => {
                    warnings.push(msg);
                    state
                }
                Err(SearchNeedsChoice::Ask(candidates)) => {
                    let files: Vec<&Path> = g
                        .members
                        .iter()
                        .map(|&i| parsed[i].0.path.as_path())
                        .collect();
                    self.resolve(g, candidates, files, opts, chooser).await
                }
            };
            states.push(state);
        }

        // 5. Episode lists for TV and anime.
        let series_jobs: Vec<(usize, MediaKind, Arc<Candidate>)> = groups
            .iter()
            .enumerate()
            .filter_map(|(gi, g)| match &states[gi] {
                GroupState::Matched(m) if g.kind.is_video() && g.kind != MediaKind::Movie => {
                    Some((gi, g.kind, m.candidate.clone()))
                }
                _ => None,
            })
            .collect();
        if !series_jobs.is_empty() {
            status("Fetching episode lists…");
        }
        let series: Vec<(usize, Result<Option<SeriesInfo>>)> =
            futures::stream::iter(series_jobs.into_iter().map(|(gi, kind, c)| async move {
                (gi, self.series_for(kind, &c).await)
            }))
            .buffer_unordered(concurrency)
            .collect()
            .await;
        for (gi, result) in series {
            match result {
                Ok(Some(s)) => {
                    if let GroupState::Matched(m) = &mut states[gi] {
                        m.series = Some(Arc::new(s));
                    }
                }
                Ok(None) => {}
                Err(e) => warnings.push(format!(
                    "could not fetch episodes for '{}': {e:#}",
                    groups[gi].query
                )),
            }
        }

        // 6. Templates for each kind present.
        let mut formatters: BTreeMap<MediaKind, Formatter> = BTreeMap::new();
        for (_, _, kind) in &parsed {
            if !formatters.contains_key(kind) {
                formatters.insert(*kind, Formatter::for_kind(*kind, opts.format.as_deref(), &self.config)?);
            }
        }

        // 7. Comic issue titles, only when something uses them.
        let want_issues = opts.extras.comicinfo
            || formatters
                .get(&MediaKind::Comic)
                .is_some_and(|f| f.uses_field("t"));
        let mut issues: HashMap<usize, Issue> = HashMap::new();
        if want_issues {
            let jobs: Vec<(usize, Arc<Candidate>, f64)> = parsed
                .iter()
                .enumerate()
                .filter_map(|(i, (_, info, kind))| {
                    let gi = group_of[i]?;
                    match (&states[gi], kind, info.issue) {
                        (GroupState::Matched(m), MediaKind::Comic, Some(n)) => {
                            Some((i, m.candidate.clone(), n))
                        }
                        _ => None,
                    }
                })
                .collect();
            if !jobs.is_empty() {
                status(&format!("Fetching {} issue(s)…", jobs.len()));
            }
            let results: Vec<(usize, Result<Option<Issue>>)> =
                futures::stream::iter(jobs.into_iter().map(|(i, c, n)| async move {
                    let r = match self.providers.by_source(c.source()) {
                        Ok(p) => p.issue(&c, n).await,
                        Err(e) => Err(e),
                    };
                    (i, r)
                }))
                .buffer_unordered(concurrency)
                .collect()
                .await;
            for (i, r) in results {
                match r {
                    Ok(Some(issue)) => {
                        issues.insert(i, issue);
                    }
                    Ok(None) => {}
                    Err(e) => warnings.push(format!("issue lookup failed: {e:#}")),
                }
            }
        }

        // 8. Name every file.
        let mut items = Vec::with_capacity(parsed.len());
        for (i, (file, info, kind)) in parsed.into_iter().enumerate() {
            let (matched, error) = match group_of[i].map(|gi| &states[gi]) {
                None => (None, Some("could not read a title from the file name".to_string())),
                Some(GroupState::Matched(m)) => (Some(m.clone()), None),
                Some(GroupState::Local) => (None, None),
                Some(GroupState::Failed(e)) => (None, Some(e.clone())),
            };
            let episodes = matched
                .as_ref()
                .and_then(|m| m.series.as_deref())
                .map(|s| find_episodes(s, &info))
                .unwrap_or_default();
            let issue = issues.remove(&i);
            let fields = build_fields(&file, &info, kind, matched.as_ref(), &episodes, issue.as_ref());
            let root = opts
                .output
                .clone()
                .or_else(|| self.config.output.get(kind).cloned());
            let target = match error {
                Some(e) => Err(e),
                None => render_target(&formatters[&kind], &fields, &file.path, root.as_deref())
                    .map_err(|e| format!("{e:#}")),
            };
            items.push(Item {
                file,
                info,
                kind,
                matched,
                episodes,
                issue,
                fields,
                target,
                organized: root.is_some(),
                output_root: root,
            });
        }

        Ok(Analysis { items, warnings })
    }

    /// Search one group. `Err` means the result needs a decision.
    async fn search_group(&self, g: &Group, opts: &Options) -> Result<GroupState, SearchNeedsChoice> {
        if opts.offline {
            return Ok(GroupState::Local);
        }
        if let Some(forced) = &opts.forced_id {
            return Ok(match self.lookup(forced, g.kind).await {
                Ok(c) => GroupState::Matched(Match {
                    candidate: Arc::new(c),
                    score: 1.0,
                    series: None,
                }),
                Err(e) => GroupState::Failed(format!("{e:#}")),
            });
        }
        if self.providers.for_kind(g.kind).is_empty() {
            let reason = self
                .providers
                .missing_reason(g.kind)
                .unwrap_or("no metadata provider")
                .to_string();
            // Comics still get a tidy name from the parsed filename.
            return if g.kind == MediaKind::Comic {
                Err(SearchNeedsChoice::Warn(reason, GroupState::Local))
            } else {
                Ok(GroupState::Failed(reason))
            };
        }
        match self.search(g.kind, &g.query, g.year, opts.threshold).await {
            Ok(SearchResult {
                accepted: Some(best),
                ..
            }) => Ok(GroupState::Matched(Match {
                candidate: Arc::new(best.candidate),
                score: best.score,
                series: None,
            })),
            Ok(SearchResult { candidates, .. }) => Err(SearchNeedsChoice::Ask(candidates)),
            Err(e) => Ok(GroupState::Failed(format!("lookup failed: {e:#}"))),
        }
    }

    async fn resolve(
        &self,
        g: &Group,
        mut candidates: Vec<Scored>,
        files: Vec<&Path>,
        opts: &Options,
        chooser: &mut dyn Chooser,
    ) -> GroupState {
        loop {
            if !opts.interactive {
                return GroupState::Failed(match candidates.first() {
                    None => format!("no match found for '{}'", g.query),
                    Some(best) => format!(
                        "uncertain match for '{}' (best: {}, score {:.2}); use --query or --id, or run interactively",
                        g.query,
                        best.candidate.label(),
                        best.score
                    ),
                });
            }
            let request = ChoiceRequest {
                kind: g.kind,
                query: &g.query,
                year: g.year,
                files: files.clone(),
                candidates: &candidates,
            };
            match chooser.choose(&request) {
                Choice::Pick(i) if i < candidates.len() => {
                    let s = candidates.swap_remove(i);
                    return GroupState::Matched(Match {
                        candidate: Arc::new(s.candidate),
                        score: s.score,
                        series: None,
                    });
                }
                Choice::Pick(_) | Choice::Skip => return GroupState::Failed("skipped".into()),
                // Keep the file's year so ranking still prefers the right release.
                Choice::Search(q) => match self.search(g.kind, &q, g.year, f64::INFINITY).await {
                    Ok(r) => candidates = r.candidates,
                    Err(e) => {
                        tracing::warn!("search failed: {e:#}");
                        candidates.clear();
                    }
                },
            }
        }
    }

    /// Search the providers for `kind` in priority order. A candidate scoring
    /// at least `accept` ends the search early.
    async fn search(&self, kind: MediaKind, query: &str, year: Option<u16>, accept: f64) -> Result<SearchResult> {
        let mut all: Vec<Scored> = Vec::new();
        let mut errors = Vec::new();
        for p in self.providers.for_kind(kind) {
            match p.search(query, year, kind).await {
                Ok(found) => {
                    let ranked = matcher::rank(query, year, found);
                    if let Some(best) = ranked.first()
                        && best.score >= accept
                    {
                        return Ok(SearchResult {
                            accepted: Some(best.clone()),
                            candidates: ranked,
                        });
                    }
                    all.extend(ranked);
                }
                Err(e) => errors.push(format!("{}: {e:#}", p.source())),
            }
        }
        if all.is_empty() && !errors.is_empty() {
            bail!("{}", errors.join("; "));
        }
        all.sort_by(|a, b| b.score.total_cmp(&a.score));
        all.truncate(10);
        Ok(SearchResult {
            accepted: None,
            candidates: all,
        })
    }

    /// Ranked candidates for a title across all providers (for `mrn info`).
    pub async fn candidates(&self, kind: MediaKind, query: &str, year: Option<u16>) -> Result<Vec<Scored>> {
        Ok(self.search(kind, query, year, f64::INFINITY).await?.candidates)
    }

    pub async fn lookup(&self, forced: &ForcedId, kind: MediaKind) -> Result<Candidate> {
        self.providers
            .by_source(forced.source)?
            .lookup(&forced.id, kind)
            .await
    }

    /// Episode list for a matched series. AniList has no episode titles, so
    /// anime is cross-referenced with TMDB by title when a key is available.
    async fn series_for(&self, kind: MediaKind, c: &Candidate) -> Result<Option<SeriesInfo>> {
        let Some(tmdb) = &self.providers.tmdb else {
            return Ok(None);
        };
        match c.source() {
            Source::Tmdb => Ok(Some(tmdb.series(c).await?)),
            Source::Anilist if kind == MediaKind::Anime => {
                let mut queries: Vec<&str> = Vec::new();
                for q in c.english_title.iter().chain(std::iter::once(&c.title)) {
                    if !queries.contains(&q.as_str()) {
                        queries.push(q);
                    }
                }
                for q in queries {
                    let found = tmdb.search(q, c.year, MediaKind::Tv).await?;
                    let ranked = matcher::rank(q, c.year, found);
                    if let Some(best) = ranked.first()
                        && best.score >= CROSS_REF_THRESHOLD
                    {
                        tracing::debug!("anime '{}' → TMDB '{}'", c.title, best.candidate.label());
                        return Ok(Some(tmdb.series(&best.candidate).await?));
                    }
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    /// Resolve target conflicts for every named item.
    pub fn plan(&self, analysis: &Analysis, opts: &Options) -> Vec<PlanItem> {
        let requests = analysis
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let to = item.target.as_ref().ok()?;
                Some(Request {
                    tag: i,
                    from: item.file.path.clone(),
                    to: to.clone(),
                    sidecars: item.file.sidecars.clone(),
                })
            })
            .collect();
        ops::build_plan(requests, opts.conflict, opts.action)
    }

    /// Carry out a plan, recording every step in `history`.
    pub async fn execute(
        &self,
        analysis: &Analysis,
        plan: &[PlanItem],
        opts: &Options,
        history: &History,
        args: &str,
        on_event: &mut dyn FnMut(ExecEvent<'_>),
    ) -> Result<ExecReport> {
        let batch = history.begin_batch(opts.action.as_str(), args)?;
        let mut report = ExecReport {
            batch,
            ..Default::default()
        };
        let mut written: HashSet<PathBuf> = HashSet::new();
        let mut warned_comicinfo = false;

        for item in plan.iter().filter(|p| p.status.is_runnable()) {
            let meta = &analysis.items[item.tag];
            // Files replaced with `--conflict override` go to the trash.
            let replace = (item.status == ItemStatus::Overwrite).then(|| history.trash());

            let main = &item.main;
            let op_id = history.start_op(batch, opts.action.into(), Some(&main.from), &main.to)?;
            match ops::apply(main, opts.action, replace) {
                Ok(applied) => {
                    history.add_dirs(batch, &applied.dirs)?;
                    history.finish_op(op_id, &main.to, applied.replaced.as_deref())?;
                    // General notices ("not embedded with --action ...") are shown once;
                    // per-file failures every time.
                    if let Some(w) = self.embed_comicinfo(meta, &main.to, opts, history, batch)? {
                        let general = w.starts_with("ComicInfo");
                        if !(general && warned_comicinfo) {
                            on_event(ExecEvent::Warning(w));
                        }
                        warned_comicinfo |= general;
                    }
                    report.done += 1;
                    on_event(ExecEvent::Done { op: main });
                }
                Err(e) => {
                    history.fail_op(op_id, &e.to_string())?;
                    report.failed += 1;
                    on_event(ExecEvent::Failed {
                        op: main,
                        error: e.to_string(),
                    });
                    continue;
                }
            }

            for op in &item.sidecars {
                let id = history.start_op(batch, opts.action.into(), Some(&op.from), &op.to)?;
                match ops::apply(op, opts.action, replace) {
                    Ok(applied) => {
                        history.add_dirs(batch, &applied.dirs)?;
                        history.finish_op(id, &op.to, applied.replaced.as_deref())?;
                        report.done += 1;
                        on_event(ExecEvent::Done { op });
                    }
                    Err(e) => {
                        history.fail_op(id, &e.to_string())?;
                        report.failed += 1;
                        on_event(ExecEvent::Failed {
                            op,
                            error: e.to_string(),
                        });
                    }
                }
            }

            self.write_extras(meta, &main.to, opts, history, batch, &mut written, &mut report, on_event)
                .await?;

            if opts.clean
                && opts.action == Action::Move
                && let Some(dir) = main.from.parent()
            {
                ops::prune_empty_dirs(dir, &meta.file.root);
            }
        }

        history.finish_batch(batch)?;
        Ok(report)
    }

    /// Embed ComicInfo.xml if enabled, recorded as an `embed` op so undo can
    /// revert it. Returns a warning when it is skipped or fails.
    fn embed_comicinfo(
        &self,
        item: &Item,
        target: &Path,
        opts: &Options,
        history: &History,
        batch: i64,
    ) -> Result<Option<String>> {
        if !opts.extras.comicinfo || !item.kind.is_book() {
            return Ok(None);
        }
        let Some(m) = item.matched.as_ref() else { return Ok(None) };
        if !matches!(opts.action, Action::Move | Action::Copy) {
            return Ok(Some(format!(
                "ComicInfo.xml is not embedded with --action {} (it would modify the original)",
                opts.action
            )));
        }
        if !comicinfo::supported(target) {
            return Ok(Some("ComicInfo.xml can only be embedded in .cbz files; skipped others".into()));
        }
        let xml = comicinfo::build(item.kind, &m.candidate, &item.info, item.issue.as_ref());
        let id = history.start_op(batch, OpKind::Embed, None, target)?;
        match comicinfo::inject(target, &xml) {
            Ok(previous) => {
                history.finish_embed(id, target, previous.as_deref())?;
                Ok(None)
            }
            Err(e) => {
                history.fail_op(id, &format!("{e:#}"))?;
                Ok(Some(format!("{}: {e:#}", target.display())))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_extras(
        &self,
        item: &Item,
        target: &Path,
        opts: &Options,
        history: &History,
        batch: i64,
        written: &mut HashSet<PathBuf>,
        report: &mut ExecReport,
        on_event: &mut dyn FnMut(ExecEvent<'_>),
    ) -> Result<()> {
        let Some(m) = &item.matched else { return Ok(()) };
        let ctx = ExtraContext {
            kind: item.kind,
            target,
            organized: item.organized,
            candidate: &m.candidate,
            series: m.series.as_deref(),
            episode: item.episodes.first(),
            season: item.episodes.first().map(|e| e.season).or(item.info.season),
            episode_numbers: &item.info.episodes,
        };
        for extra in extras::plan(&ctx, &opts.extras) {
            if written.contains(&extra.path) || fs::symlink_metadata(&extra.path).is_ok() {
                continue;
            }
            written.insert(extra.path.clone());
            let result = match &extra.content {
                ExtraContent::Text(text) => write_text(&extra.path, text),
                ExtraContent::Download(url) => self.http.download(url, &extra.path).await,
            };
            match result {
                Ok(()) => {
                    let id = history.start_op(batch, OpKind::Create, None, &extra.path)?;
                    history.finish_op(id, &extra.path, None)?;
                    report.extras += 1;
                    on_event(ExecEvent::Extra { path: &extra.path });
                }
                Err(e) => on_event(ExecEvent::Warning(format!(
                    "could not write {}: {e:#}",
                    extra.path.display()
                ))),
            }
        }
        Ok(())
    }
}

enum SearchNeedsChoice {
    /// Show this warning and continue with the state.
    Warn(String, GroupState),
    /// Uncertain: ask the chooser.
    Ask(Vec<Scored>),
}

fn write_text(path: &Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("mrn-part");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// The provider episodes a file contains.
fn find_episodes(series: &SeriesInfo, info: &ParsedInfo) -> Vec<Episode> {
    if let Some(season) = info.season
        && !info.episodes.is_empty()
    {
        return info
            .episodes
            .iter()
            .filter_map(|&e| series.by_season_episode(season, e).cloned())
            .collect();
    }
    if !info.absolute.is_empty() {
        return info
            .absolute
            .iter()
            .filter_map(|&a| series.by_absolute(a).cloned())
            .collect();
    }
    if let Some(date) = info.date {
        return series.by_date(&date.to_string()).cloned().into_iter().collect();
    }
    Vec::new()
}

/// Template fields for one file.
pub fn build_fields(
    file: &MediaFile,
    info: &ParsedInfo,
    kind: MediaKind,
    matched: Option<&Match>,
    episodes: &[Episode],
    issue: Option<&Issue>,
) -> Fields {
    let mut f = Fields::default();
    let c = matched.map(|m| &*m.candidate);

    f.set_text(
        "n",
        c.map(|c| c.title.clone())
            .filter(|t| !t.trim().is_empty())
            .or_else(|| info.title.clone()),
    );
    f.set_num("y", c.and_then(|c| c.year).or(info.year).map(f64::from));
    f.set_text("kind", Some(kind.as_str()));
    if let Some(c) = c {
        f.set_text("id", Some(c.id.clone()));
        f.set_text("source", Some(c.source().as_str()));
        match c.source() {
            Source::Tmdb => f.set_text("tmdbid", Some(c.id.clone())),
            Source::Anilist => f.set_text("anilistid", Some(c.id.clone())),
            _ => {}
        }
        f.set_text("publisher", c.publisher.clone());
    }

    // Episode numbers: from the filename, or mapped from absolute numbers
    // through the provider's episode list.
    let (season, numbers): (Option<u32>, Vec<u32>) = if !info.episodes.is_empty() {
        (info.season, info.episodes.clone())
    } else if !episodes.is_empty() {
        (
            Some(episodes[0].season),
            episodes.iter().map(|e| e.episode).collect(),
        )
    } else {
        (info.season, Vec::new())
    };
    if let Some(s) = season {
        f.set_num("s", Some(s as f64));
    }
    if !numbers.is_empty() {
        f.set_nums("e", &numbers.iter().map(|&n| n as f64).collect::<Vec<_>>());
        if let Some(s) = season {
            let mut code = format!("S{s:02}E{:02}", numbers[0]);
            for n in &numbers[1..] {
                code.push_str(&format!("-E{n:02}"));
            }
            f.set_text("s00e00", Some(code));
            f.set_text("sxe", Some(format!("{s}x{:02}", numbers[0])));
        }
    }

    let absolute: Vec<f64> = if !info.absolute.is_empty() {
        info.absolute.iter().map(|&a| a as f64).collect()
    } else {
        episodes.iter().filter_map(|e| e.absolute).map(f64::from).collect()
    };
    f.set_nums("abs", &absolute);

    let mut titles: Vec<&str> = Vec::new();
    for t in episodes.iter().filter_map(|e| e.title.as_deref()) {
        if !titles.contains(&t) {
            titles.push(t);
        }
    }
    let title = if titles.is_empty() {
        issue.and_then(|i| i.title.clone())
    } else {
        Some(titles.join(" & "))
    };
    f.set_text("t", title);
    f.set_text(
        "d",
        info.date
            .map(|d| d.to_string())
            .or_else(|| episodes.first().and_then(|e| e.air_date.clone())),
    );

    f.set_num("vol", info.volume);
    f.set_num("ch", info.chapter);
    f.set_num("issue", info.issue);

    f.set_text("vf", info.resolution.clone());
    f.set_text("vs", info.source.clone());
    f.set_text("vc", info.video_codec.clone());
    f.set_text("ac", info.audio_codec.clone());
    f.set_text("group", info.group.clone());
    f.set_text("crc", info.crc32.clone());
    f.set_text("ot", info.title.clone());
    f.set_text(
        "orig",
        file.path.file_stem().map(|s| s.to_string_lossy().into_owned()),
    );
    f
}

fn render_target(fmt: &Formatter, fields: &Fields, src: &Path, root: Option<&Path>) -> Result<PathBuf> {
    let rel = fmt.render(fields, root.is_some())?;
    let base = match root {
        Some(r) => r.to_path_buf(),
        None => src.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    let mut target = base.join(rel);
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match src.extension() {
        Some(ext) => target.set_file_name(format!("{name}.{}", ext.to_string_lossy())),
        None => target.set_file_name(name),
    }
    Ok(target)
}
