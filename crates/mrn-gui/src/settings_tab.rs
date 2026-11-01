//! The Settings tab: API keys, and a shortcut to the full config file.

use std::path::PathBuf;

use iced::widget::{Column, button, checkbox, column, row, text, text_input};
use iced::{Center, Element, Fill, Task};
use mrn_core::config::{Config, save_api_keys};

use crate::widgets::{bad, card, dim, ok, warn};
use crate::worker::blocking;

#[derive(Debug, Clone)]
pub enum Msg {
    Tmdb(String),
    ComicVine(String),
    Show(bool),
    Save,
    Saved(Result<(), String>),
    OpenConfig,
    Opened(Result<(), String>),
}

pub struct State {
    path: Option<PathBuf>,
    tmdb: String,
    comicvine: String,
    show: bool,
    message: Option<Result<String, String>>,
}

impl State {
    pub fn new() -> State {
        let mut s = State {
            path: None,
            tmdb: String::new(),
            comicvine: String::new(),
            show: false,
            message: None,
        };
        s.reload();
        s
    }

    /// Re-read the keys stored in the config file (not the environment).
    pub fn reload(&mut self) {
        self.path = Config::default_path().ok();
        match Config::load_file(None) {
            Ok(c) => {
                self.tmdb = c.api.tmdb.unwrap_or_default();
                self.comicvine = c.api.comicvine.unwrap_or_default();
            }
            Err(e) => self.message = Some(Err(format!("{e:#}"))),
        }
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Tmdb(s) => self.tmdb = s,
            Msg::ComicVine(s) => self.comicvine = s,
            Msg::Show(on) => self.show = on,
            Msg::Save => {
                let Some(path) = self.path.clone() else {
                    self.message = Some(Err("no config location (is $HOME set?)".into()));
                    return Task::none();
                };
                let (tmdb, cv) = (self.tmdb.clone(), self.comicvine.clone());
                return Task::perform(
                    blocking(move || {
                        save_api_keys(&path, &[("tmdb", &tmdb), ("comicvine", &cv)]).map_err(|e| format!("{e:#}"))
                    }),
                    Msg::Saved,
                );
            }
            Msg::Saved(r) => {
                self.message = Some(r.map(|()| "Saved. New keys apply to the next preview.".to_string()));
            }
            Msg::OpenConfig => {
                let Some(path) = self.path.clone() else {
                    return Task::none();
                };
                return Task::perform(blocking(move || open_config(path)), Msg::Opened);
            }
            Msg::Opened(r) => {
                if let Err(e) = r {
                    self.message = Some(Err(e));
                }
            }
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let label = |s: &'static str| text(s).size(14).width(130);
        let mut keys = Column::new().spacing(10).push(text("API keys").size(18)).push(
            row![
                label("TMDB"),
                text_input("Needed for movies and TV, and anime episode titles", &self.tmdb)
                    .on_input(Msg::Tmdb)
                    .secure(!self.show)
                    .width(Fill),
            ]
            .spacing(8)
            .align_y(Center),
        );
        keys = keys.push(dim("Free key: themoviedb.org → Settings → API. A v3 key or a v4 read token both work."));
        if std::env::var_os("TMDB_API_KEY").is_some() {
            keys = keys.push(text("TMDB_API_KEY is set in the environment and overrides this value.").size(13).style(warn));
        }
        keys = keys.push(
            row![
                label("ComicVine"),
                text_input("Optional, for western comics", &self.comicvine)
                    .on_input(Msg::ComicVine)
                    .secure(!self.show)
                    .width(Fill),
            ]
            .spacing(8)
            .align_y(Center),
        );
        keys = keys.push(dim("Free key: comicvine.gamespot.com/api. Without one, comics are named from the file name."));
        if std::env::var_os("COMICVINE_API_KEY").is_some() {
            keys = keys.push(
                text("COMICVINE_API_KEY is set in the environment and overrides this value.")
                    .size(13)
                    .style(warn),
            );
        }
        keys = keys.push(
            row![
                checkbox(self.show).label("Show keys").on_toggle(Msg::Show),
                iced::widget::space().width(Fill),
                button("Save keys").style(button::primary).on_press(Msg::Save),
            ]
            .align_y(Center),
        );
        match &self.message {
            Some(Ok(m)) => keys = keys.push(text(m).size(14).style(ok)),
            Some(Err(e)) => keys = keys.push(text(e).size(14).style(bad)),
            None => {}
        }

        let path = self
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "unknown".into());
        let config = column![
            text("Configuration file").size(18),
            text(path).size(14).font(iced::Font::MONOSPACE),
            dim("Library folders per media type, naming templates, language, title language and defaults live here."),
            button("Open in editor").style(button::secondary).on_press(Msg::OpenConfig),
        ]
        .spacing(8);

        column![card(keys), card(config)].spacing(14).into()
    }
}

/// Open the config file with the desktop's default handler, creating it from
/// the starter template first.
fn open_config(path: PathBuf) -> Result<(), String> {
    if !path.exists() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, Config::template()).map_err(|e| e.to_string())?;
    }
    std::process::Command::new("xdg-open")
        .arg(&path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not run xdg-open: {e}"))
}
