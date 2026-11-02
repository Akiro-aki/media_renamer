//! End-to-end: scan → analyze (mocked TMDB + AniList) → execute → undo.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use mrn_core::config::{Config, Extras};
use mrn_core::history::History;
use mrn_core::pipeline::{Choice, ChoiceRequest, Chooser, Engine, Options, SkipUncertain};
use mrn_core::providers::anilist::Anilist;
use mrn_core::providers::comicvine::ComicVine;
use mrn_core::providers::http::{Http, HttpOptions};
use mrn_core::providers::mal::Mal;
use mrn_core::providers::mangadex::Mangadex;
use mrn_core::providers::tmdb::Tmdb;
use mrn_core::providers::{Providers, Source};
use mrn_core::scan::scan;
use serde_json::json;
use wiremock::matchers::{body_partial_json, body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn mock(server: &MockServer, p: &str, q: Option<(&str, &str)>, body: serde_json::Value) {
    let mut m = Mock::given(method("GET")).and(path(p));
    if let Some((k, v)) = q {
        m = m.and(query_param(k, v));
    }
    m.respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn mock_tmdb(server: &MockServer) {
    mock(server, "/search/tv", Some(("query", "The Expanse")), json!({"results": [
        {"id": 63639, "name": "The Expanse", "first_air_date": "2015-12-14"}
    ]}))
    .await;
    mock(server, "/tv/63639", None, json!({
        "id": 63639, "name": "The Expanse", "first_air_date": "2015-12-14",
        "overview": "Belters.", "seasons": [{"season_number": 1}, {"season_number": 2}]
    }))
    .await;
    mock(server, "/tv/63639/season/1", None, json!({"episodes": [
        {"season_number": 1, "episode_number": 1, "name": "Dulcinea"}
    ]}))
    .await;
    mock(server, "/tv/63639/season/2", None, json!({"episodes": [
        {"season_number": 2, "episode_number": 5, "name": "Home", "air_date": "2017-03-01"}
    ]}))
    .await;
    mock(server, "/search/movie", Some(("query", "Dune")), json!({"results": [
        {"id": 841, "title": "Dune", "release_date": "1984-12-14"},
        {"id": 438631, "title": "Dune", "release_date": "2021-09-15"}
    ]}))
    .await;
    // Anime cross-reference: AniList's English title → TMDB show.
    mock(server, "/search/tv", Some(("query", "Frieren: Beyond Journey's End")), json!({"results": [
        {"id": 209867, "name": "Frieren: Beyond Journey's End", "first_air_date": "2023-09-29"}
    ]}))
    .await;
    mock(server, "/tv/209867", None, json!({
        "id": 209867, "name": "Frieren: Beyond Journey's End", "first_air_date": "2023-09-29",
        "seasons": [{"season_number": 1}]
    }))
    .await;
    let eps: Vec<_> = (1..=12)
        .map(|e| json!({"season_number": 1, "episode_number": e,
                        "name": if e == 12 { "A Real Hero".to_string() } else { format!("Ep {e}") }}))
        .collect();
    mock(server, "/tv/209867/season/1", None, json!({"episodes": eps})).await;

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"Page": {"media": [{
            "id": 154587, "idMal": 52991, "seasonYear": 2023,
            "title": {"romaji": "Sousou no Frieren", "english": "Frieren: Beyond Journey's End"},
            "synonyms": ["Frieren"]
        }]}}})))
        .mount(server)
        .await;
}

fn engine(server: &MockServer) -> Engine {
    engine_with(server, true)
}

/// Every provider points at `server`; Jikan lives under `/jikan`.
fn engine_with(server: &MockServer, tmdb_key: bool) -> Engine {
    let config = Config::default();
    let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
    let providers = Providers {
        tmdb: tmdb_key.then(|| {
            Arc::new(Tmdb::new(http.clone(), "key".into(), "en-US".into()).with_base_url(server.uri()))
        }),
        anilist: Arc::new(
            Anilist::new(http.clone(), Default::default())
                .with_base_url(format!("{}/graphql", server.uri())),
        ),
        mal: Arc::new(
            Mal::new(http.clone(), Default::default()).with_base_url(format!("{}/jikan", server.uri())),
        ),
        mangadex: Arc::new(Mangadex::new(http.clone()).with_base_url(server.uri())),
        comicvine: Some(Arc::new(
            ComicVine::new(http.clone(), "key".into()).with_base_url(server.uri()),
        )),
    };
    Engine::with_providers(config, providers, http)
}

fn touch(dir: &Path, name: &str) {
    let p = dir.join(name);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, name.as_bytes()).unwrap();
}

#[tokio::test]
async fn tv_movie_anime_end_to_end_with_undo() {
    let server = MockServer::start().await;
    mock_tmdb(&server).await;
    let engine = engine(&server);

    let d = tempfile::tempdir().unwrap();
    let input = d.path().join("in");
    let lib = d.path().join("lib");
    for f in [
        "The.Expanse.S02E05.1080p.WEB.mkv",
        "The.Expanse.S02E05.1080p.WEB.en.srt",
        "Dune.2021.2160p.mkv",
        "[SubsPlease] Sousou no Frieren - 12 (1080p) [ABCD1234].mkv",
    ] {
        touch(&input, f);
    }

    let mut opts = Options::from_config(&engine.config);
    opts.output = Some(lib.clone());
    opts.interactive = false;
    opts.extras = Extras {
        nfo: true,
        artwork: false,
        comicinfo: false,
    };

    let files = scan(std::slice::from_ref(&input), true).unwrap().files;
    let analysis = engine
        .analyze(files, &opts, &mut SkipUncertain, &|_| {})
        .await
        .unwrap();
    assert!(analysis.warnings.is_empty(), "{:?}", analysis.warnings);

    let targets: Vec<String> = analysis
        .items
        .iter()
        .map(|i| match &i.target {
            Ok(t) => t.strip_prefix(&lib).unwrap().display().to_string(),
            Err(e) => format!("ERR {e}"),
        })
        .collect();
    assert_eq!(
        targets,
        [
            "Dune (2021) [tmdbid-438631]/Dune (2021).mkv",
            "The Expanse (2015)/Season 02/The Expanse - S02E05 - Home.mkv",
            "Sousou no Frieren (2023)/Season 01/Sousou no Frieren - S01E12 - A Real Hero.mkv",
        ]
    );

    let plan = engine.plan(&analysis, &opts);
    let history = History::open(&d.path().join("history.db")).unwrap();
    let report = engine
        .execute(&analysis, &plan, &opts, &history, "test", &mut |_| {})
        .await
        .unwrap();
    assert_eq!(report.done, 4, "3 videos + 1 subtitle");
    assert_eq!(report.failed, 0);

    let expanse = lib.join("The Expanse (2015)");
    assert!(expanse.join("Season 02/The Expanse - S02E05 - Home.en.srt").exists());
    let ep_nfo = fs::read_to_string(expanse.join("Season 02/The Expanse - S02E05 - Home.nfo")).unwrap();
    assert!(ep_nfo.contains("<title>Home</title>") && ep_nfo.contains("<aired>2017-03-01</aired>"));
    assert!(fs::read_to_string(expanse.join("tvshow.nfo")).unwrap().contains("63639"));
    assert!(lib.join("Dune (2021) [tmdbid-438631]/Dune (2021).nfo").exists());
    assert!(fs::read_dir(&input).unwrap().next().is_none(), "input emptied");

    let undo = history.undo(report.batch, false).unwrap();
    assert!(undo.skipped.is_empty(), "{:?}", undo.skipped);
    assert!(input.join("Dune.2021.2160p.mkv").exists());
    assert!(input.join("The.Expanse.S02E05.1080p.WEB.en.srt").exists());
    assert!(!lib.exists(), "library folders created by the batch are removed");
}

struct Scripted(Vec<Choice>, Vec<String>);

impl Chooser for Scripted {
    fn choose(&mut self, req: &ChoiceRequest<'_>) -> Choice {
        self.1.push(req.query.to_string());
        self.0.remove(0)
    }
}

#[tokio::test]
async fn uncertain_matches_ask_or_fail() {
    let server = MockServer::start().await;
    mock_tmdb(&server).await;
    mock(&server, "/search/movie", Some(("query", "Doon")), json!({"results": [
        {"id": 438631, "title": "Dune", "release_date": "2021-09-15"}
    ]}))
    .await;
    let engine = engine(&server);

    let d = tempfile::tempdir().unwrap();
    touch(d.path(), "Doon.2021.mkv");
    let files = || scan(&[d.path().to_path_buf()], true).unwrap().files;

    let mut opts = Options::from_config(&engine.config);
    opts.interactive = false;
    let a = engine.analyze(files(), &opts, &mut SkipUncertain, &|_| {}).await.unwrap();
    let err = a.items[0].target.as_ref().unwrap_err();
    assert!(err.contains("uncertain match") && err.contains("Dune (2021)"), "{err}");

    // Interactive: the user searches for "Dune", then picks the 2021 film.
    opts.interactive = true;
    let mut chooser = Scripted(vec![Choice::Search("Dune".into()), Choice::Pick(0)], vec![]);
    let a = engine.analyze(files(), &opts, &mut chooser, &|_| {}).await.unwrap();
    assert_eq!(chooser.1, ["Doon", "Doon"]);
    let target = a.items[0].target.as_ref().unwrap();
    assert_eq!(target.file_name().unwrap(), "Dune (2021).mkv");

    // In place (no output root): the file stays in its folder.
    assert_eq!(target.parent().unwrap(), d.path());
}

#[tokio::test]
async fn forced_id_and_offline() {
    let server = MockServer::start().await;
    mock_tmdb(&server).await;
    mock(&server, "/movie/438631", None, json!({"id": 438631, "title": "Dune", "release_date": "2021-09-15"})).await;
    let engine = engine(&server);
    let d = tempfile::tempdir().unwrap();
    touch(d.path(), "some random name.mkv");
    let files = || scan(&[d.path().to_path_buf()], true).unwrap().files;

    let mut opts = Options::from_config(&engine.config);
    opts.interactive = false;
    opts.kind = Some(mrn_core::MediaKind::Movie);
    opts.forced_id = Some("tmdb:438631".parse().unwrap());
    let a = engine.analyze(files(), &opts, &mut SkipUncertain, &|_| {}).await.unwrap();
    assert_eq!(a.items[0].target.as_ref().unwrap().file_name().unwrap(), "Dune (2021).mkv");

    opts.forced_id = None;
    opts.offline = true;
    let a = engine.analyze(files(), &opts, &mut SkipUncertain, &|_| {}).await.unwrap();
    assert_eq!(
        a.items[0].target.as_ref().unwrap().file_name().unwrap(),
        "some random name.mkv"
    );
    assert!(a.items[0].matched.is_none());
}

/// Target file names for `files` in `dir`, renamed in place.
async fn names(engine: &Engine, opts: &Options, dir: &Path) -> Vec<String> {
    let files = scan(&[dir.to_path_buf()], true).unwrap().files;
    let analysis = engine
        .analyze(files, opts, &mut SkipUncertain, &|_| {})
        .await
        .unwrap();
    analysis
        .items
        .iter()
        .map(|i| match &i.target {
            Ok(t) => t.file_name().unwrap().to_string_lossy().into_owned(),
            Err(e) => format!("ERR {e}"),
        })
        .collect()
}

#[tokio::test]
async fn provider_order_and_cross_site_ids() {
    let server = MockServer::start().await;
    mock_tmdb(&server).await;
    mock(&server, "/manga", Some(("title", "Chainsaw Man")), json!({"data": [{
        "id": "a77742b1",
        "attributes": {"title": {"en": "Chainsaw Man"}, "year": 2018,
                       "links": {"al": "105778", "mal": "116778"}},
        "relationships": []
    }]}))
    .await;
    let engine = engine(&server);
    let d = tempfile::tempdir().unwrap();

    // Anime: {malid} comes from AniList's idMal; titles follow anime_episodes.
    let anime = d.path().join("anime");
    touch(&anime, "[SubsPlease] Sousou no Frieren - 12 (1080p).mkv");
    let mut opts = Options::from_config(&engine.config);
    opts.interactive = false;
    opts.format = Some(r"{n} \[{malid}\][ - {t}]".into());
    assert_eq!(names(&engine, &opts, &anime).await, ["Sousou no Frieren [52991] - A Real Hero.mkv"]);
    opts.providers.anime_episodes.clear();
    assert_eq!(names(&engine, &opts, &anime).await, ["Sousou no Frieren [52991].mkv"]);

    // Manga: MangaDex first means AniList is never asked.
    let manga = d.path().join("manga");
    touch(&manga, "Chainsaw Man v01.cbz");
    opts.kind = Some(mrn_core::MediaKind::Manga);
    opts.format = Some(r"{n} \[{source} {anilistid} {malid}\]".into());
    opts.providers.manga = vec![Source::Mangadex];
    let graphql_calls = || async {
        let reqs = server.received_requests().await.unwrap();
        reqs.iter().filter(|r| r.url.path() == "/graphql").count()
    };
    let before = graphql_calls().await;
    assert_eq!(names(&engine, &opts, &manga).await, ["Chainsaw Man [mangadex 105778 116778].cbz"]);
    assert_eq!(graphql_calls().await, before, "AniList was searched");
}

/// Jikan's episode list for Frieren (MAL 52991), one page.
async fn mock_jikan_episodes(server: &MockServer) {
    let eps: Vec<_> = (1..=28)
        .map(|n| json!({"mal_id": n, "title": if n == 3 { "Killing Magic".to_string() } else { format!("MAL ep {n}") },
                        "aired": "2023-10-06T00:00:00+00:00"}))
        .collect();
    mock(server, "/jikan/anime/52991/episodes", Some(("page", "1")), json!({
        "pagination": {"last_visible_page": 1, "has_next_page": false}, "data": eps
    }))
    .await;
}

#[tokio::test]
async fn anime_titles_from_mal_without_tmdb_key() {
    let server = MockServer::start().await;
    mock_tmdb(&server).await; // only AniList is reachable without a key
    mock_jikan_episodes(&server).await;
    let engine = engine_with(&server, false);
    let d = tempfile::tempdir().unwrap();
    let anime = d.path().join("anime");
    touch(&anime, "[SubsPlease] Sousou no Frieren - 03 (1080p).mkv");
    // Season-numbered names (detected as TV without a group tag, hence -t anime).
    // A MAL list is one season: S02 most likely matched the wrong entry, so no title.
    touch(&anime, "Sousou no Frieren S01E03.mkv");
    touch(&anime, "Sousou no Frieren S02E03.mkv");
    let mut opts = Options::from_config(&engine.config);
    opts.interactive = false;
    opts.kind = Some(mrn_core::MediaKind::Anime);
    assert_eq!(
        names(&engine, &opts, &anime).await,
        [
            "Sousou no Frieren - S01E03 - Killing Magic.mkv",
            "Sousou no Frieren - S02E03.mkv",
            "Sousou no Frieren - S01E03 - Killing Magic.mkv",
        ]
    );
}

#[tokio::test]
async fn anime_episodes_order_picks_mal_first() {
    let server = MockServer::start().await;
    mock_tmdb(&server).await;
    mock_jikan_episodes(&server).await;
    let engine = engine(&server);
    let d = tempfile::tempdir().unwrap();
    let anime = d.path().join("anime");
    touch(&anime, "[SubsPlease] Sousou no Frieren - 12 (1080p).mkv");
    let mut opts = Options::from_config(&engine.config);
    opts.interactive = false;
    assert_eq!(names(&engine, &opts, &anime).await, ["Sousou no Frieren - S01E12 - A Real Hero.mkv"]);
    opts.providers.anime_episodes = vec![Source::Mal, Source::Tmdb];
    assert_eq!(names(&engine, &opts, &anime).await, ["Sousou no Frieren - S01E12 - MAL ep 12.mkv"]);
}

#[tokio::test]
async fn anime_search_falls_back_to_mal() {
    let server = MockServer::start().await;
    // AniList knows nothing; MAL has the show.
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"Page": {"media": []}}})))
        .mount(&server)
        .await;
    mock(&server, "/jikan/anime", Some(("q", "Sousou no Frieren")), json!({"data": [{
        "mal_id": 52991, "titles": [{"type": "Default", "title": "Sousou no Frieren"}],
        "type": "TV", "year": 2023
    }]}))
    .await;
    mock_jikan_episodes(&server).await;
    let engine = engine_with(&server, false);
    let d = tempfile::tempdir().unwrap();
    let anime = d.path().join("anime");
    touch(&anime, "[SubsPlease] Sousou no Frieren - 03 (1080p).mkv");
    let mut opts = Options::from_config(&engine.config);
    opts.interactive = false;
    opts.format = Some(r"{n} - {abs:02} - {t} \[{source}-{malid}\]".into());
    assert_eq!(
        names(&engine, &opts, &anime).await,
        ["Sousou no Frieren - 03 - Killing Magic [mal-52991].mkv"]
    );
}

/// AniList relations: Frieren season 1 (154587, MAL 52991) → season 2
/// (182255, MAL 59978); and Jikan's 10 episodes of season 2.
async fn mock_frieren_seasons(server: &MockServer) {
    let node = |id: u64, mal: u64, title: &str| {
        json!({"id": id, "idMal": mal, "type": "ANIME", "format": "TV", "episodes": null, "title": {"romaji": title}})
    };
    let s1 = node(154587, 52991, "Sousou no Frieren");
    let s2 = node(182255, 59978, "Sousou no Frieren 2nd Season");
    for (id, me, rel, other) in [(154587, &s1, "SEQUEL", &s2), (182255, &s2, "PREQUEL", &s1)] {
        let mut media = me.clone();
        media["relations"] = json!({"edges": [{"relationType": rel, "node": other}]});
        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_string_contains("relations"))
            .and(body_partial_json(json!({"variables": {"id": id}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"Media": media}})))
            .with_priority(1)
            .mount(server)
            .await;
    }
    let eps: Vec<_> = (1..=10).map(|n| json!({"mal_id": n, "title": format!("Second {n}")})).collect();
    mock(server, "/jikan/anime/59978/episodes", Some(("page", "1")), json!({
        "pagination": {"has_next_page": false}, "data": eps
    }))
    .await;
}

#[tokio::test]
async fn later_seasons_through_the_sequel_chain() {
    let server = MockServer::start().await;
    mock_tmdb(&server).await;
    mock_jikan_episodes(&server).await;
    mock_frieren_seasons(&server).await;
    let d = tempfile::tempdir().unwrap();
    let anime = d.path().join("anime");
    // Each search finds season 1; the file names point at season 2.
    touch(&anime, "Sousou no Frieren S02E03.mkv");
    touch(&anime, "[SubsPlease] Sousou no Frieren - 31 (1080p).mkv");
    touch(&anime, "[SubsPlease] Sousou no Frieren - 03 (1080p).mkv");

    for tmdb_key in [false, true] {
        let engine = engine_with(&server, tmdb_key);
        let mut opts = Options::from_config(&engine.config);
        opts.interactive = false;
        opts.kind = Some(mrn_core::MediaKind::Anime);
        opts.format = Some("{n} - {s00e00}[ - {t}]".into());
        let got = names(&engine, &opts, &anime).await;
        // With a key, TMDB names what it has (season 1 up to 12 here) and
        // MAL fills in season 2.
        let ep3 = if tmdb_key { "Ep 3" } else { "Killing Magic" };
        assert_eq!(
            got,
            [
                "Sousou no Frieren - S02E03 - Second 3.mkv".to_string(),
                format!("Sousou no Frieren - S01E03 - {ep3}.mkv"),
                "Sousou no Frieren - S02E03 - Second 3.mkv".to_string(),
            ],
            "tmdb key: {tmdb_key}"
        );
    }
}
