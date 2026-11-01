//! TMDB v3: movies and TV (also used for anime episode titles).

use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use super::{Candidate, Episode, Http, Provider, SeriesInfo, Source, year_of};
use crate::MediaKind;

const BASE: &str = "https://api.themoviedb.org/3";
const IMAGE_BASE: &str = "https://image.tmdb.org/t/p/original";

pub struct Tmdb {
    http: Arc<Http>,
    key: String,
    language: String,
    base: String,
}

#[derive(Deserialize)]
struct Page<T> {
    #[serde(default = "Vec::new")]
    results: Vec<T>,
}

#[derive(Deserialize)]
struct MovieJson {
    id: u64,
    title: Option<String>,
    original_title: Option<String>,
    release_date: Option<String>,
    overview: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    #[serde(default)]
    genres: Vec<Genre>,
}

#[derive(Deserialize)]
struct TvJson {
    id: u64,
    name: Option<String>,
    original_name: Option<String>,
    first_air_date: Option<String>,
    overview: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    #[serde(default)]
    genres: Vec<Genre>,
    #[serde(default)]
    seasons: Vec<SeasonSummary>,
}

#[derive(Deserialize)]
struct Genre {
    name: String,
}

#[derive(Deserialize)]
struct SeasonSummary {
    season_number: u32,
    poster_path: Option<String>,
}

#[derive(Deserialize)]
struct SeasonJson {
    #[serde(default)]
    episodes: Vec<EpisodeJson>,
}

#[derive(Deserialize)]
struct EpisodeJson {
    season_number: u32,
    episode_number: u32,
    name: Option<String>,
    air_date: Option<String>,
    overview: Option<String>,
}

impl Tmdb {
    pub fn new(http: Arc<Http>, key: String, language: String) -> Tmdb {
        Tmdb {
            http,
            key,
            language,
            base: BASE.to_string(),
        }
    }

    /// Point at a different API root (tests).
    pub fn with_base_url(mut self, base: impl Into<String>) -> Tmdb {
        self.base = base.into();
        self
    }

    async fn get(&self, path: &str, mut query: Vec<(&str, String)>) -> Result<Value> {
        query.push(("language", self.language.clone()));
        let mut headers = Vec::new();
        // v4 read access tokens are JWTs; v3 keys are 32 hex chars.
        if self.key.starts_with("eyJ") {
            headers.push(("Authorization", format!("Bearer {}", self.key)));
        } else {
            query.push(("api_key", self.key.clone()));
        }
        self.http
            .get_json(&format!("{}{path}", self.base), &query, &headers, &["api_key"])
            .await
    }

    fn image(path: Option<&String>) -> Option<String> {
        path.filter(|p| !p.is_empty())
            .map(|p| format!("{IMAGE_BASE}{p}"))
    }

    fn movie_candidate(m: MovieJson) -> Candidate {
        let title = m.title.clone().unwrap_or_default();
        let alt = m.original_title.into_iter().filter(|t| *t != title).collect();
        Candidate {
            source: Some(Source::Tmdb),
            id: m.id.to_string(),
            kind: Some(MediaKind::Movie),
            alt_titles: alt,
            year: year_of(m.release_date.as_deref()),
            overview: m.overview.filter(|o| !o.is_empty()),
            poster: Self::image(m.poster_path.as_ref()),
            backdrop: Self::image(m.backdrop_path.as_ref()),
            genres: m.genres.into_iter().map(|g| g.name).collect(),
            url: Some(format!("https://www.themoviedb.org/movie/{}", m.id)),
            title,
            ..Default::default()
        }
    }

    fn tv_candidate(t: TvJson) -> Candidate {
        let title = t.name.clone().unwrap_or_default();
        let alt = t.original_name.into_iter().filter(|n| *n != title).collect();
        Candidate {
            source: Some(Source::Tmdb),
            id: t.id.to_string(),
            kind: Some(MediaKind::Tv),
            alt_titles: alt,
            year: year_of(t.first_air_date.as_deref()),
            overview: t.overview.filter(|o| !o.is_empty()),
            poster: Self::image(t.poster_path.as_ref()),
            backdrop: Self::image(t.backdrop_path.as_ref()),
            genres: t.genres.into_iter().map(|g| g.name).collect(),
            url: Some(format!("https://www.themoviedb.org/tv/{}", t.id)),
            title,
            ..Default::default()
        }
    }

    async fn search_once(&self, query: &str, year: Option<u16>, kind: MediaKind) -> Result<Vec<Candidate>> {
        let mut q = vec![("query", query.to_string())];
        if kind == MediaKind::Movie {
            if let Some(y) = year {
                q.push(("year", y.to_string()));
            }
            let v = self.get("/search/movie", q).await?;
            let page: Page<MovieJson> = serde_json::from_value(v).context("TMDB movie search")?;
            Ok(page.results.into_iter().take(10).map(Self::movie_candidate).collect())
        } else {
            if let Some(y) = year {
                q.push(("first_air_date_year", y.to_string()));
            }
            let v = self.get("/search/tv", q).await?;
            let page: Page<TvJson> = serde_json::from_value(v).context("TMDB TV search")?;
            Ok(page.results.into_iter().take(10).map(Self::tv_candidate).collect())
        }
    }
}

#[async_trait]
impl Provider for Tmdb {
    fn source(&self) -> Source {
        Source::Tmdb
    }

    async fn search(&self, query: &str, year: Option<u16>, kind: MediaKind) -> Result<Vec<Candidate>> {
        let results = self.search_once(query, year, kind).await?;
        // A wrong year in the filename should not hide the right show.
        if results.is_empty() && year.is_some() {
            return self.search_once(query, None, kind).await;
        }
        Ok(results)
    }

    async fn lookup(&self, id: &str, kind: MediaKind) -> Result<Candidate> {
        if kind == MediaKind::Movie {
            let v = self.get(&format!("/movie/{id}"), vec![]).await?;
            Ok(Self::movie_candidate(serde_json::from_value(v).context("TMDB movie")?))
        } else {
            let v = self.get(&format!("/tv/{id}"), vec![]).await?;
            let mut c = Self::tv_candidate(serde_json::from_value(v).context("TMDB TV show")?);
            c.kind = Some(kind);
            Ok(c)
        }
    }

    async fn series(&self, candidate: &Candidate) -> Result<SeriesInfo> {
        let id = &candidate.id;
        let v = self.get(&format!("/tv/{id}"), vec![]).await?;
        let show: TvJson = serde_json::from_value(v).context("TMDB TV show")?;

        let mut info = SeriesInfo::default();
        let mut numbers: Vec<u32> = show.seasons.iter().map(|s| s.season_number).collect();
        numbers.sort_unstable();
        for s in &show.seasons {
            if let Some(p) = Self::image(s.poster_path.as_ref()) {
                info.season_posters.insert(s.season_number, p);
            }
        }

        let seasons = futures::future::try_join_all(numbers.iter().map(|n| async move {
            let v = self.get(&format!("/tv/{id}/season/{n}"), vec![]).await?;
            serde_json::from_value::<SeasonJson>(v).context("TMDB season")
        }))
        .await?;

        // Absolute numbering counts regular seasons in order; specials (0) have none.
        let mut absolute = 0;
        for season in seasons {
            for e in season.episodes {
                let abs = if e.season_number > 0 {
                    absolute += 1;
                    Some(absolute)
                } else {
                    None
                };
                info.episodes.push(Episode {
                    season: e.season_number,
                    episode: e.episode_number,
                    absolute: abs,
                    title: e.name.filter(|n| !n.is_empty()),
                    air_date: e.air_date.filter(|d| !d.is_empty()),
                    overview: e.overview.filter(|o| !o.is_empty()),
                });
            }
        }
        Ok(info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::http::HttpOptions;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn tmdb(server: &MockServer) -> Tmdb {
        let http = Arc::new(Http::new(HttpOptions::default()).unwrap());
        Tmdb::new(http, "k".into(), "en-US".into()).with_base_url(server.uri())
    }

    #[tokio::test]
    async fn search_movie_retries_without_year() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/movie"))
            .and(query_param("year", "1999"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": []})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/search/movie"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [
                {"id": 438631, "title": "Dune", "original_title": "Dune",
                 "release_date": "2021-09-15", "poster_path": "/p.jpg"}
            ]})))
            .mount(&server)
            .await;
        let r = tmdb(&server).await.search("Dune", Some(1999), MediaKind::Movie).await.unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "438631");
        assert_eq!(r[0].year, Some(2021));
        assert_eq!(r[0].poster.as_deref(), Some("https://image.tmdb.org/t/p/original/p.jpg"));
        assert!(r[0].alt_titles.is_empty());
    }

    #[tokio::test]
    async fn series_absolute_numbering() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/tv/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1, "name": "Show", "first_air_date": "2020-01-01",
                "seasons": [{"season_number": 0}, {"season_number": 1, "poster_path": "/s1.jpg"}, {"season_number": 2}]
            })))
            .mount(&server)
            .await;
        for (n, eps) in [(0, 1), (1, 2), (2, 2)] {
            let episodes: Vec<_> = (1..=eps)
                .map(|e| json!({"season_number": n, "episode_number": e, "name": format!("S{n}E{e}")}))
                .collect();
            Mock::given(method("GET"))
                .and(path(format!("/tv/1/season/{n}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"episodes": episodes})))
                .mount(&server)
                .await;
        }
        let t = tmdb(&server).await;
        let c = t.lookup("1", MediaKind::Tv).await.unwrap();
        let s = t.series(&c).await.unwrap();
        assert_eq!(s.episodes.len(), 5);
        assert_eq!(s.by_absolute(3).unwrap().title.as_deref(), Some("S2E1"));
        assert_eq!(s.by_season_episode(0, 1).unwrap().absolute, None);
        assert!(s.season_posters.contains_key(&1));
    }
}
