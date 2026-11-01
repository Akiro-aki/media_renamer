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
use mrn_core::providers::mangadex::Mangadex;
use mrn_core::providers::tmdb::Tmdb;
use mrn_core::providers::Providers;
use mrn_core::scan::scan;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
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
            "id": 154587, "seasonYear": 2023,
            "title": {"romaji": "Sousou no Frieren", "english": "Frieren: Beyond Journey's End"},
            "synonyms": ["Frieren"]
        }]}}})))
        .mount(server)
        .await;
}

fn engine(server: &MockServer) -> Engine {
    let config = Config::default();
    let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
    let providers = Providers {
        tmdb: Some(Arc::new(
            Tmdb::new(http.clone(), "key".into(), "en-US".into()).with_base_url(server.uri()),
        )),
        anilist: Arc::new(
            Anilist::new(http.clone(), Default::default())
                .with_base_url(format!("{}/graphql", server.uri())),
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
