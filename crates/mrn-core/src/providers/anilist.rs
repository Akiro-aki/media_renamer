//! AniList GraphQL: anime and manga identity. No API key needed.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Candidate, Http, Provider, Source, strip_html};
use crate::MediaKind;
use crate::config::TitlePreference;

const BASE: &str = "https://graphql.anilist.co";

const FIELDS: &str = "
fragment f on Media {
  id idMal type format seasonYear startDate { year }
  title { romaji english native } synonyms
  coverImage { extraLarge large } bannerImage
  description(asHtml: false) genres siteUrl
}";

pub struct Anilist {
    http: Arc<Http>,
    title_pref: TitlePreference,
    base: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaJson {
    id: u64,
    id_mal: Option<u64>,
    format: Option<String>,
    season_year: Option<u16>,
    start_date: Option<StartDate>,
    title: Titles,
    #[serde(default)]
    synonyms: Vec<String>,
    cover_image: Option<Cover>,
    banner_image: Option<String>,
    description: Option<String>,
    #[serde(default)]
    genres: Vec<String>,
    site_url: Option<String>,
}

#[derive(Deserialize)]
struct StartDate {
    year: Option<u16>,
}

#[derive(Deserialize)]
struct Titles {
    romaji: Option<String>,
    english: Option<String>,
    native: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cover {
    extra_large: Option<String>,
    large: Option<String>,
}

impl Anilist {
    pub fn new(http: Arc<Http>, title_pref: TitlePreference) -> Anilist {
        Anilist {
            http,
            title_pref,
            base: BASE.to_string(),
        }
    }

    pub fn with_base_url(mut self, base: impl Into<String>) -> Anilist {
        self.base = base.into();
        self
    }

    async fn query(&self, query: &str, variables: Value) -> Result<Value> {
        self.query_raw(&format!("{query}\n{FIELDS}"), variables).await
    }

    /// A query without the shared fragment (GraphQL rejects unused fragments).
    async fn query_raw(&self, query: &str, variables: Value) -> Result<Value> {
        let body = json!({ "query": query, "variables": variables });
        let v = self.http.post_json(&self.base, &body).await?;
        if let Some(errors) = v.get("errors").and_then(|e| e.as_array())
            && !errors.is_empty()
        {
            let msg = errors[0]["message"].as_str().unwrap_or("unknown error");
            bail!("AniList: {msg}");
        }
        Ok(v)
    }

    fn media_type(kind: MediaKind) -> &'static str {
        if kind.is_video() { "ANIME" } else { "MANGA" }
    }

    fn candidate(&self, m: MediaJson, kind: MediaKind) -> Candidate {
        let t = &m.title;
        let romaji = t.romaji.clone();
        let title = match self.title_pref {
            TitlePreference::Romaji => romaji.clone(),
            TitlePreference::English => t.english.clone().or(romaji.clone()),
            TitlePreference::Native => t.native.clone().or(romaji.clone()),
        }
        .unwrap_or_default();

        let mut alt: Vec<String> = [t.romaji.clone(), t.english.clone(), t.native.clone()]
            .into_iter()
            .flatten()
            .chain(m.synonyms)
            .filter(|s| *s != title && !s.is_empty())
            .collect();
        alt.dedup();

        Candidate {
            source: Some(Source::Anilist),
            id: m.id.to_string(),
            kind: Some(kind),
            title,
            alt_titles: alt,
            year: m.season_year.or(m.start_date.and_then(|d| d.year)),
            overview: m.description.map(|d| strip_html(&d)).filter(|d| !d.is_empty()),
            poster: m.cover_image.and_then(|c| c.extra_large.or(c.large)),
            backdrop: m.banner_image,
            genres: m.genres,
            english_title: t.english.clone(),
            url: m.site_url,
            ext_ids: m.id_mal.map(|id| (Source::Mal, id.to_string())).into_iter().collect(),
            ..Default::default()
        }
    }
}

/// One TV entry of a franchise, placed in TheTVDB-style seasons.
#[derive(Debug, Clone, PartialEq)]
pub struct SeasonEntry {
    pub anilist_id: u64,
    pub mal_id: Option<u64>,
    pub title: String,
    /// Episode count, when AniList knows it (airing shows often don't).
    pub episodes: Option<u32>,
    /// Season number, counting the franchise's TV entries from 1.
    pub season: u32,
    /// This entry continues the previous entry's season ("Part 2", "2nd Cour").
    pub continues: bool,
}

/// The TV entries around a matched entry, in airing order.
#[derive(Debug, Clone, PartialEq)]
pub struct SeasonChain {
    pub entries: Vec<SeasonEntry>,
    /// Index of the matched entry in `entries`.
    pub matched: usize,
}

const RELATIONS: &str = "query ($id: Int) { Media(id: $id, type: ANIME) {
  id idMal format episodes title { romaji }
  relations { edges { relationType node { id idMal type format episodes title { romaji } } } }
} }";
const RELATIONS_BY_MAL: &str = "query ($idMal: Int) { Media(idMal: $idMal, type: ANIME) {
  id idMal format episodes title { romaji }
  relations { edges { relationType node { id idMal type format episodes title { romaji } } } }
} }";

/// Relation hops followed in each direction. Long franchises (Gintama) stay
/// well below this.
const MAX_HOPS: usize = 25;

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct NodeJson {
    id: u64,
    id_mal: Option<u64>,
    #[serde(rename = "type")]
    media_type: Option<String>,
    format: Option<String>,
    episodes: Option<u32>,
    title: Option<RomajiTitle>,
}

#[derive(Deserialize, Clone)]
struct RomajiTitle {
    romaji: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelationsJson {
    #[serde(flatten)]
    node: NodeJson,
    relations: Option<Edges>,
}

#[derive(Deserialize)]
struct Edges {
    #[serde(default)]
    edges: Vec<Edge>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Edge {
    relation_type: Option<String>,
    node: NodeJson,
}

impl NodeJson {
    /// Formats that count as a season. Movies, OVAs, specials and music
    /// videos sit in the chain but don't count.
    fn is_season(&self) -> bool {
        matches!(self.format.as_deref(), Some("TV" | "TV_SHORT" | "ONA"))
    }

    fn title(&self) -> String {
        self.title.as_ref().and_then(|t| t.romaji.clone()).unwrap_or_default()
    }
}

impl Anilist {
    async fn relations(&self, id: u64) -> Result<RelationsJson> {
        let v = self.query_raw(RELATIONS, json!({ "id": id })).await?;
        serde_json::from_value(v["data"]["Media"].clone()).context("AniList relations")
    }

    /// Next entry in `direction` (`SEQUEL` or `PREQUEL`), preferring a TV
    /// entry when there are several.
    fn step(r: &RelationsJson, direction: &str) -> Option<NodeJson> {
        let all: Vec<&NodeJson> = r
            .relations
            .iter()
            .flat_map(|e| &e.edges)
            .filter(|e| e.relation_type.as_deref() == Some(direction))
            .map(|e| &e.node)
            .filter(|n| n.media_type.as_deref() == Some("ANIME"))
            .collect();
        all.iter().find(|n| n.is_season()).or(all.first()).map(|n| (*n).clone())
    }

    /// Season entries reached by following `direction` links from `from`,
    /// nearest first. Non-season entries are passed through.
    async fn walk(&self, from: &RelationsJson, direction: &str) -> Result<Vec<NodeJson>> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::from([from.node.id]);
        let mut next = Self::step(from, direction);
        for _ in 0..MAX_HOPS {
            let Some(node) = next.take() else { break };
            if !seen.insert(node.id) {
                break;
            }
            let r = self.relations(node.id).await?;
            next = Self::step(&r, direction);
            if node.is_season() {
                out.push(node);
            }
        }
        Ok(out)
    }

    /// The franchise's TV entries around anime `id` (AniList id, or MAL id
    /// with `by_mal`), following prequel and sequel links. Seasons are counted
    /// from the first TV entry; a "Part 2" of the previous entry continues its
    /// season.
    pub async fn season_chain(&self, id: u64, by_mal: bool) -> Result<SeasonChain> {
        let start: RelationsJson = if by_mal {
            let v = self.query_raw(RELATIONS_BY_MAL, json!({ "idMal": id })).await?;
            serde_json::from_value(v["data"]["Media"].clone()).context("AniList relations")?
        } else {
            self.relations(id).await?
        };
        let before = self.walk(&start, "PREQUEL").await?;
        let after = self.walk(&start, "SEQUEL").await?;

        let mut nodes: Vec<NodeJson> = before.into_iter().rev().collect();
        let matched = nodes.len();
        nodes.push(start.node);
        nodes.extend(after);

        let mut entries: Vec<SeasonEntry> = Vec::with_capacity(nodes.len());
        for n in nodes {
            let title = n.title();
            let continues = entries
                .last()
                .is_some_and(|prev| prev.title != title && base_title(&prev.title) == base_title(&title));
            let season = match entries.last() {
                None => 1,
                Some(prev) if continues => prev.season,
                Some(prev) => prev.season + 1,
            };
            entries.push(SeasonEntry {
                anilist_id: n.id,
                mal_id: n.id_mal,
                title,
                episodes: n.episodes,
                season,
                continues,
            });
        }
        Ok(SeasonChain { entries, matched })
    }
}

/// A title without a trailing part marker: "X Part 2", "X 2nd Cour",
/// "X Cour 2" → "x". Used to tell a split season from a new one.
fn base_title(title: &str) -> String {
    let lower = title.to_lowercase();
    let mut words: Vec<&str> = lower.split_whitespace().collect();
    let is_number = |w: &str| {
        let w = w.trim_end_matches(|c: char| !c.is_alphanumeric());
        !w.is_empty()
            && (w.chars().all(|c| c.is_ascii_digit())
                || matches!(w, "i" | "ii" | "iii" | "iv" | "v" | "vi" | "two" | "three"))
    };
    let is_ordinal = |w: &str| {
        let digits = w.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit())
            && matches!(&w[digits.len()..], "st" | "nd" | "rd" | "th")
    };
    loop {
        let n = words.len();
        // "part 2", "cour 2", or "2nd cour"
        let marker = n >= 2
            && ((matches!(words[n - 2], "part" | "cour") && is_number(words[n - 1]))
                || (is_ordinal(words[n - 2]) && words[n - 1].starts_with("cour")));
        if !marker {
            break;
        }
        words.truncate(n - 2);
    }
    words.join(" ").trim_end_matches([':', '-', ' ']).to_string()
}

#[async_trait]
impl Provider for Anilist {
    fn source(&self) -> Source {
        Source::Anilist
    }

    async fn search(&self, query: &str, _year: Option<u16>, kind: MediaKind) -> Result<Vec<Candidate>> {
        let q = "query ($search: String, $type: MediaType) {
            Page(perPage: 10) { media(search: $search, type: $type, sort: SEARCH_MATCH) { ...f } }
        }";
        let v = self
            .query(q, json!({ "search": query, "type": Self::media_type(kind) }))
            .await?;
        let list: Vec<MediaJson> =
            serde_json::from_value(v["data"]["Page"]["media"].clone()).context("AniList search")?;
        // AniList's MANGA type includes light novels, which usually share the
        // manga's title and would win the match. `--id anilist:N` still
        // reaches a novel through `lookup`.
        Ok(list
            .into_iter()
            .filter(|m| m.format.as_deref() != Some("NOVEL"))
            .map(|m| self.candidate(m, kind))
            .collect())
    }

    async fn lookup(&self, id: &str, kind: MediaKind) -> Result<Candidate> {
        let id: i64 = id.parse().context("AniList ids are numbers")?;
        let q = "query ($id: Int, $type: MediaType) { Media(id: $id, type: $type) { ...f } }";
        let v = self
            .query(q, json!({ "id": id, "type": Self::media_type(kind) }))
            .await?;
        let m: MediaJson =
            serde_json::from_value(v["data"]["Media"].clone()).context("AniList lookup")?;
        Ok(self.candidate(m, kind))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::http::HttpOptions;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn search_prefers_configured_title() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"Page": {"media": [{
                "id": 154587, "idMal": 52991, "type": "ANIME", "seasonYear": 2023,
                "title": {"romaji": "Sousou no Frieren", "english": "Frieren: Beyond Journey's End", "native": "葬送のフリーレン"},
                "synonyms": ["Frieren"], "coverImage": {"extraLarge": "https://x/c.jpg"},
                "description": "A <i>fairy</i> tale.<br>", "genres": ["Adventure"]
            }]}}})))
            .mount(&server)
            .await;
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        let a = Anilist::new(http.clone(), TitlePreference::Romaji).with_base_url(server.uri());
        let r = a.search("Frieren", None, MediaKind::Anime).await.unwrap();
        assert_eq!(r[0].title, "Sousou no Frieren");
        assert!(r[0].alt_titles.contains(&"Frieren".to_string()));
        assert_eq!(r[0].overview.as_deref(), Some("A fairy tale."));
        assert_eq!(r[0].year, Some(2023));
        assert_eq!(r[0].id_for(Source::Mal), Some("52991"));
        assert_eq!(r[0].id_for(Source::Anilist), Some("154587"));

        let a = Anilist::new(http, TitlePreference::English).with_base_url(server.uri());
        let r = a.search("Frieren", None, MediaKind::Anime).await.unwrap();
        assert_eq!(r[0].title, "Frieren: Beyond Journey's End");
    }

    #[tokio::test]
    async fn manga_search_skips_light_novels() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"Page": {"media": [
                {"id": 1, "type": "MANGA", "format": "NOVEL", "title": {"romaji": "Spy Classroom"}},
                {"id": 2, "type": "MANGA", "format": "MANGA", "title": {"romaji": "Spy Classroom"}},
                {"id": 3, "type": "MANGA", "format": "ONE_SHOT", "title": {"romaji": "Spy Classroom: Extra"}}
            ]}}})))
            .mount(&server)
            .await;
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        let a = Anilist::new(http, TitlePreference::Romaji).with_base_url(server.uri());
        let r = a.search("Spy Classroom", None, MediaKind::Manga).await.unwrap();
        let ids: Vec<&str> = r.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["2", "3"]);
    }

    /// A `Media` node with relations, as the relations query returns it.
    fn media(id: u64, mal: Option<u64>, format: &str, title: &str, edges: &[(&str, Value)]) -> Value {
        json!({"data": {"Media": {
            "id": id, "idMal": mal, "format": format, "episodes": null, "title": {"romaji": title},
            "relations": {"edges": edges.iter().map(|(rel, node)| json!({"relationType": rel, "node": node})).collect::<Vec<_>>()}
        }}})
    }

    fn node(id: u64, mal: Option<u64>, kind: &str, format: &str, title: &str) -> Value {
        json!({"id": id, "idMal": mal, "type": kind, "format": format, "episodes": 28, "title": {"romaji": title}})
    }

    #[tokio::test]
    async fn season_chain_follows_relations() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        let s1 = node(154587, Some(52991), "ANIME", "TV", "Sousou no Frieren");
        let special = node(999, None, "ANIME", "SPECIAL", "Sousou no Frieren: Mini");
        let s2 = node(182255, Some(59978), "ANIME", "TV", "Sousou no Frieren 2nd Season");
        let s2b = node(182256, Some(60000), "ANIME", "TV", "Sousou no Frieren 2nd Season Part 2");
        let manga = node(118586, Some(126287), "MANGA", "MANGA", "Sousou no Frieren");
        let bodies = [
            (json!({"id": 154587}), media(154587, Some(52991), "TV", "Sousou no Frieren",
                &[("ADAPTATION", manga.clone()), ("SEQUEL", special.clone())])),
            (json!({"id": 999}), media(999, None, "SPECIAL", "Sousou no Frieren: Mini",
                &[("PREQUEL", s1.clone()), ("SEQUEL", s2.clone())])),
            (json!({"id": 182255}), media(182255, Some(59978), "TV", "Sousou no Frieren 2nd Season",
                &[("PREQUEL", special.clone()), ("SEQUEL", s2b.clone())])),
            (json!({"idMal": 59978}), media(182255, Some(59978), "TV", "Sousou no Frieren 2nd Season",
                &[("PREQUEL", special.clone()), ("SEQUEL", s2b.clone())])),
            (json!({"id": 182256}), media(182256, Some(60000), "TV", "Sousou no Frieren 2nd Season Part 2",
                &[("PREQUEL", s2.clone())])),
        ];
        for (vars, body) in bodies {
            Mock::given(method("POST"))
                .and(body_partial_json(json!({"variables": vars})))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
        }
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        let a = Anilist::new(http, TitlePreference::Romaji).with_base_url(server.uri());

        for by_mal in [false, true] {
            let id = if by_mal { 59978 } else { 182255 };
            let c = a.season_chain(id, by_mal).await.unwrap();
            let got: Vec<(u64, u32, bool)> = c.entries.iter().map(|e| (e.anilist_id, e.season, e.continues)).collect();
            assert_eq!(got, [(154587, 1, false), (182255, 2, false), (182256, 2, true)], "by_mal {by_mal}");
            assert_eq!(c.matched, 1);
            assert_eq!(c.entries[0].mal_id, Some(52991));
        }
    }

    #[test]
    fn part_markers() {
        assert_eq!(base_title("Shingeki no Kyojin: The Final Season Part 2"), "shingeki no kyojin: the final season");
        assert_eq!(base_title("Shingeki no Kyojin: The Final Season"), "shingeki no kyojin: the final season");
        assert_eq!(base_title("Re:Zero 2nd Season Part II"), "re:zero 2nd season");
        assert_eq!(base_title("Mushoku Tensei 2nd Cour"), "mushoku tensei");
        assert_eq!(base_title("Spy x Family Cour 2"), "spy x family");
        // Seasons are not parts.
        assert_eq!(base_title("Sousou no Frieren 2nd Season"), "sousou no frieren 2nd season");
        assert_eq!(base_title("JoJo no Kimyou na Bouken Part 5: Ougon no Kaze"), "jojo no kimyou na bouken part 5: ougon no kaze");
    }

    /// Against the real AniList: `cargo test -p mrn-core live_season_chain -- --ignored`.
    #[tokio::test]
    #[ignore = "needs network"]
    async fn live_season_chain() {
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        let a = Anilist::new(http, TitlePreference::Romaji);
        let show = |c: &SeasonChain| -> Vec<(u32, bool, String)> {
            c.entries.iter().map(|e| (e.season, e.continues, e.title.clone())).collect()
        };

        // Frieren season 2 by its MAL id: season 1 comes before it.
        let c = a.season_chain(59978, true).await.unwrap();
        println!("{:#?}", show(&c));
        assert_eq!(c.entries[c.matched].mal_id, Some(59978));
        assert_eq!(c.entries[c.matched].season, 2);
        assert_eq!(c.entries[0].mal_id, Some(52991));

        // Attack on Titan: "3 Part 2" and "The Final Season Part 2" continue
        // their seasons, as on TheTVDB (4 seasons).
        let c = a.season_chain(16498, false).await.unwrap();
        println!("{:#?}", show(&c));
        let seasons: Vec<u32> = c.entries.iter().map(|e| e.season).collect();
        assert_eq!(seasons.first(), Some(&1));
        assert_eq!(seasons.iter().max(), Some(&4), "{seasons:?}");
    }
}
