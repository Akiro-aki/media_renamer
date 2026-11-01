//! User configuration: `~/.config/mrn/config.toml` plus environment overrides.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::MediaKind;
use crate::ops::{Action, Conflict};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub api: ApiKeys,
    pub general: General,
    pub output: PerKind<PathBuf>,
    pub format: PerKind<String>,
    pub extras: Extras,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiKeys {
    /// TMDB v3 API key or v4 read access token. Env: `TMDB_API_KEY`.
    pub tmdb: Option<String>,
    /// ComicVine API key. Env: `COMICVINE_API_KEY`.
    pub comicvine: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Metadata language for TMDB (`en-US`, `de-DE`, ...).
    pub language: String,
    /// Naming preset: jellyfin, plex, kodi, anime, kavita, komga.
    pub preset: String,
    pub action: Action,
    pub conflict: Conflict,
    /// Minimum match confidence (0-1) accepted without asking.
    pub threshold: f64,
    /// Which AniList title to name anime and manga by.
    pub anime_title: TitlePreference,
    /// Strip characters Windows/SMB can't store (`?*"<>|\`).
    pub windows_safe: bool,
    /// How long provider responses stay cached.
    pub cache_ttl_hours: u64,
    /// Parallel provider lookups.
    pub concurrency: usize,
}

impl Default for General {
    fn default() -> Self {
        Self {
            language: "en-US".into(),
            preset: "jellyfin".into(),
            action: Action::Move,
            conflict: Conflict::Skip,
            threshold: crate::matcher::DEFAULT_THRESHOLD,
            anime_title: TitlePreference::Romaji,
            windows_safe: true,
            cache_ttl_hours: 72,
            concurrency: 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitlePreference {
    #[default]
    Romaji,
    English,
    Native,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Extras {
    /// Write Kodi/Jellyfin .nfo files (movies, TV, anime).
    pub nfo: bool,
    /// Download posters/fanart/covers.
    pub artwork: bool,
    /// Embed ComicInfo.xml into .cbz archives (comics, manga).
    pub comicinfo: bool,
}

/// One optional value per media kind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PerKind<T> {
    pub movie: Option<T>,
    pub tv: Option<T>,
    pub anime: Option<T>,
    pub comic: Option<T>,
    pub manga: Option<T>,
}

impl<T> Default for PerKind<T> {
    fn default() -> Self {
        Self {
            movie: None,
            tv: None,
            anime: None,
            comic: None,
            manga: None,
        }
    }
}

impl<T> PerKind<T> {
    pub fn get(&self, kind: MediaKind) -> Option<&T> {
        match kind {
            MediaKind::Movie => self.movie.as_ref(),
            MediaKind::Tv => self.tv.as_ref(),
            MediaKind::Anime => self.anime.as_ref(),
            MediaKind::Comic => self.comic.as_ref(),
            MediaKind::Manga => self.manga.as_ref(),
        }
    }
}

/// XDG locations used by mrn.
pub struct Dirs {
    pub config: PathBuf,
    pub cache: PathBuf,
    pub data: PathBuf,
}

pub fn dirs() -> Result<Dirs> {
    let p = ProjectDirs::from("", "", "mrn").context("could not determine home directory")?;
    Ok(Dirs {
        config: p.config_dir().to_path_buf(),
        cache: p.cache_dir().to_path_buf(),
        data: p.data_dir().to_path_buf(),
    })
}

impl Config {
    /// Default config path, or `$MRN_CONFIG` if set.
    pub fn default_path() -> Result<PathBuf> {
        if let Some(p) = std::env::var_os("MRN_CONFIG") {
            return Ok(PathBuf::from(p));
        }
        Ok(dirs()?.config.join("config.toml"))
    }

    /// Load from `path` (or the default path). A missing file gives defaults.
    /// Environment variables override file values.
    pub fn load(path: Option<&Path>) -> Result<Config> {
        let mut config = Self::load_file(path)?;
        config.apply_env();
        config.expand_paths();
        Ok(config)
    }

    /// Like [`Config::load`] but without environment overrides or path
    /// expansion: what the file itself says (for editing it).
    pub fn load_file(path: Option<&Path>) -> Result<Config> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => Self::default_path()?,
        };
        if !path.exists() {
            return Ok(Config::default());
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    fn apply_env(&mut self) {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        if let Some(v) = env("TMDB_API_KEY") {
            self.api.tmdb = Some(v);
        }
        if let Some(v) = env("COMICVINE_API_KEY") {
            self.api.comicvine = Some(v);
        }
        // Treat empty strings in the file as unset.
        self.api.tmdb = self.api.tmdb.take().filter(|v| !v.trim().is_empty());
        self.api.comicvine = self.api.comicvine.take().filter(|v| !v.trim().is_empty());
    }

    fn expand_paths(&mut self) {
        for slot in [
            &mut self.output.movie,
            &mut self.output.tv,
            &mut self.output.anime,
            &mut self.output.comic,
            &mut self.output.manga,
        ] {
            if let Some(p) = slot.as_mut() {
                *p = expand_tilde(p);
            }
        }
    }

    /// A commented starter config for `mrn config init`.
    pub fn template() -> &'static str {
        include_str!("config_template.toml")
    }
}

/// Expand a leading `~/` to the home directory.
pub fn expand_tilde(p: &Path) -> PathBuf {
    let Ok(rest) = p.strip_prefix("~") else {
        return p.to_path_buf();
    };
    match directories::BaseDirs::new() {
        Some(b) => b.home_dir().join(rest),
        None => p.to_path_buf(),
    }
}

/// Set API keys under `[api]` in the config file at `path`, keeping its
/// comments and layout. A missing file is first created from the starter
/// template. An empty value clears the key. The file is made private (0600)
/// because it holds secrets.
pub fn save_api_keys(path: &Path, keys: &[(&str, &str)]) -> Result<()> {
    let text = if path.exists() {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    } else {
        Config::template().to_string()
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    if !doc.contains_table("api") {
        doc["api"] = toml_edit::table();
    }
    for (name, value) in keys {
        let value = value.trim();
        match doc["api"].get_mut(name) {
            // Replace the value but keep the comment lines above it.
            Some(item) if item.is_value() => {
                let decor = item.as_value().map(|v| v.decor().clone());
                *item = toml_edit::value(value);
                if let (Some(decor), Some(v)) = (decor, item.as_value_mut()) {
                    *v.decor_mut() = decor;
                }
            }
            _ => doc["api"][name] = toml_edit::value(value),
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_api_keys_keeps_comments() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("sub/config.toml");
        save_api_keys(&path, &[("tmdb", " abc123 ")]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# Free key from https://www.themoviedb.org"), "{text}");
        assert!(text.contains("tmdb = \"abc123\""), "{text}");
        assert!(text.contains("[general]"));

        std::fs::write(&path, "# mine\n[general]\nthreshold = 0.9\n").unwrap();
        save_api_keys(&path, &[("tmdb", "k1"), ("comicvine", "")]).unwrap();
        let c = Config::load(Some(&path)).unwrap();
        assert_eq!(c.api.tmdb.as_deref(), Some("k1"));
        assert_eq!(c.api.comicvine, None);
        assert_eq!(c.general.threshold, 0.9);
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# mine"));
    }

    #[test]
    fn template_parses() {
        let c: Config = toml::from_str(Config::template()).unwrap();
        assert_eq!(c.general.preset, "jellyfin");
        assert!(!c.extras.artwork);
    }
}
