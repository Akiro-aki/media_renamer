//! `mrn-gui`: a graphical front end for mrn. It drives the same
//! `mrn_core::pipeline` as the CLI: pick files, preview the new names,
//! resolve uncertain matches, apply, and undo from the history.

mod history_tab;
mod rename_tab;
mod settings_tab;
mod widgets;
mod worker;

use std::path::PathBuf;

use iced::widget::{button, column, container, row, rule, space, text};
use iced::{Element, Fill, Size, Subscription, Task, window};

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title(App::title)
        .subscription(App::subscription)
        .window(window::Settings {
            size: Size::new(1080.0, 760.0),
            min_size: Some(Size::new(760.0, 520.0)),
            #[cfg(target_os = "linux")]
            platform_specific: window::settings::PlatformSpecific {
                application_id: "mrn-gui".into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .run()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Rename,
    History,
    Settings,
}

#[derive(Debug, Clone)]
pub enum Message {
    Tab(Tab),
    Rename(rename_tab::Msg),
    History(history_tab::Msg),
    Settings(settings_tab::Msg),
    FileDropped(PathBuf),
}

struct App {
    tab: Tab,
    rename: rename_tab::State,
    history: history_tab::State,
    settings: settings_tab::State,
}

impl App {
    fn new() -> (App, Task<Message>) {
        // Paths given on the command line (e.g. from a file manager) are preloaded.
        let paths: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
        let app = App {
            tab: Tab::Rename,
            rename: rename_tab::State::new(paths),
            history: history_tab::State::default(),
            settings: settings_tab::State::new(),
        };
        (app, Task::none())
    }

    fn title(&self) -> String {
        "mrn: media renamer".into()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tab(tab) => {
                self.tab = tab;
                match tab {
                    Tab::History => self.history.reload().map(Message::History),
                    Tab::Settings => {
                        self.settings.reload();
                        Task::none()
                    }
                    Tab::Rename => Task::none(),
                }
            }
            Message::Rename(m) => self.rename.update(m).map(Message::Rename),
            Message::History(m) => self.history.update(m).map(Message::History),
            Message::Settings(m) => self.settings.update(m).map(Message::Settings),
            Message::FileDropped(p) => {
                self.tab = Tab::Rename;
                self.rename.update(rename_tab::Msg::Picked(vec![p])).map(Message::Rename)
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let tab = |label: &'static str, t: Tab| {
            let b = button(text(label).size(15)).padding([6, 16]);
            let b = b.on_press(Message::Tab(t));
            if self.tab == t { b.style(button::primary) } else { b.style(button::text) }
        };
        let header = row![
            text("mrn").size(20).font(iced::Font {
                weight: iced::font::Weight::Bold,
                ..Default::default()
            }),
            space().width(16),
            tab("Rename", Tab::Rename),
            tab("History", Tab::History),
            tab("Settings", Tab::Settings),
        ]
        .spacing(4)
        .align_y(iced::Center)
        .padding([8, 14]);

        let body = match self.tab {
            Tab::Rename => self.rename.view().map(Message::Rename),
            Tab::History => self.history.view().map(Message::History),
            Tab::Settings => self.settings.view().map(Message::Settings),
        };
        column![header, rule::horizontal(1), container(body).width(Fill).height(Fill).padding(14)].into()
    }

    fn subscription(&self) -> Subscription<Message> {
        // Works on X11; winit does not deliver Wayland drag and drop yet.
        iced::event::listen_with(|event, _, _| match event {
            iced::Event::Window(window::Event::FileDropped(p)) => Some(Message::FileDropped(p)),
            _ => None,
        })
    }
}

/// Renders every screen with sample data to PNG, headless:
/// `MRN_GUI_SNAPSHOTS=/tmp/shots cargo test -p mrn-gui -- --ignored snapshots`
#[cfg(test)]
mod snapshots {
    use std::sync::Arc;

    use iced::{Font, Settings, Theme};
    use iced_test::Simulator;
    use mrn_core::MediaKind;
    use mrn_core::history::BatchSummary;
    use mrn_core::preview::{Preview, PreviewRow, RowState};

    use super::*;
    use crate::worker::{CandidateView, Event, Finished, Prompt};

    fn row(from: &str, to: Option<&str>, id: Option<&str>, score: Option<f64>, state: RowState) -> PreviewRow {
        PreviewRow {
            item: 0,
            kind: MediaKind::Tv,
            source: from.into(),
            from: from.into(),
            to: to.map(Into::into),
            target: to.map(Into::into),
            sidecars: usize::from(from.contains("S02E05")),
            matched: None,
            matched_id: id.map(Into::into),
            score,
            state,
        }
    }

    fn shoot(app: &App, name: &str, theme: &Theme) {
        let dir = std::env::var("MRN_GUI_SNAPSHOTS").unwrap_or_else(|_| "snapshots".into());
        let settings = Settings {
            default_font: Font::with_name("Noto Sans"),
            ..Settings::default()
        };
        let mut ui = Simulator::with_size(settings, (1080.0, 760.0), app.view());
        let shot = ui.snapshot(theme).unwrap();
        let _ = std::fs::remove_file(format!("{dir}/{name}-tiny-skia.png"));
        assert!(shot.matches_image(format!("{dir}/{name}.png")).unwrap());
    }

    #[test]
    #[ignore = "writes PNGs; run on demand"]
    fn snapshots() {
        let theme = Theme::Dark;
        let (mut app, _) = App::new();
        app.rename = rename_tab::State::new(vec!["/home/me/Downloads/TV".into(), "/home/me/Downloads/Comics".into()]);
        shoot(&app, "1-rename", &theme);

        let preview = Preview {
            rows: vec![
                row(
                    "The.Expanse.S02E05.1080p.WEB.x264.mkv",
                    Some("The Expanse/Season 02/The Expanse - S02E05 - Home.mkv"),
                    Some("tmdb:63639"),
                    Some(0.97),
                    RowState::Ready,
                ),
                row(
                    "The.Expanse.S02E06.1080p.WEB.x264.mkv",
                    Some("The Expanse/Season 02/The Expanse - S02E06 - Paradigm Shift.mkv"),
                    Some("tmdb:63639"),
                    Some(0.97),
                    RowState::Ready,
                ),
                row(
                    "[SubsPlease] Sousou no Frieren - 12 (1080p).mkv",
                    Some("Sousou no Frieren/Season 01/Sousou no Frieren - S01E12.mkv"),
                    Some("anilist:154587"),
                    Some(1.0),
                    RowState::Overwrite,
                ),
                row(
                    "Dune.2021.2160p.mkv",
                    Some("Dune (2021)/Dune (2021).mkv"),
                    None,
                    None,
                    RowState::Skipped("target exists".into()),
                ),
                row("random_clip.mkv", None, None, None, RowState::Unmatched("no match found for 'random clip'".into())),
            ],
            ready: 3,
            skipped: 1,
            unmatched: 1,
            in_place: false,
        };
        let _ = app.rename.update(rename_tab::Msg::Worker(0, Event::Preview(Arc::new(preview))));
        shoot(&app, "2-preview", &theme);

        let prompt = Prompt {
            kind: MediaKind::Movie,
            query: "Dune".into(),
            year: None,
            files: vec!["Dune.2160p.UHD.BluRay.mkv".into()],
            candidates: vec![
                CandidateView {
                    label: "Dune (2021)".into(),
                    id: "tmdb:438631".into(),
                    score: 0.82,
                    overview: Some("Paul Atreides, a brilliant and gifted young man born into a great destiny beyond his understanding, must travel to the most dangerous planet in the universe.".into()),
                },
                CandidateView {
                    label: "Dune (1984)".into(),
                    id: "tmdb:841".into(),
                    score: 0.80,
                    overview: Some("In the year 10,191, the most precious substance in the universe is the spice Melange.".into()),
                },
                CandidateView {
                    label: "Dune: Part Two (2024)".into(),
                    id: "tmdb:693134".into(),
                    score: 0.61,
                    overview: None,
                },
            ],
        };
        let mut app2 = App::new().0;
        app2.rename = rename_tab::State::new(vec!["/home/me/Downloads/Movies".into()]);
        let _ = app2.rename.update(rename_tab::Msg::Worker(0, Event::Choose(Arc::new(prompt))));
        shoot(&app2, "3-uncertain", &theme);

        let _ = app.rename.update(rename_tab::Msg::Worker(
            0,
            Event::Finished(Finished {
                batch: 12,
                done: 4,
                failed: 0,
                extras: 2,
                done_verb: "Moved",
            }),
        ));
        shoot(&app, "4-done", &theme);

        app.tab = Tab::History;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let _ = app.history.update(history_tab::Msg::Loaded(Ok(vec![
            BatchSummary {
                id: 12,
                created: now - 120,
                action: "move".into(),
                args: "mrn-gui rename /home/me/Downloads/TV -o /home/me/Media -a move".into(),
                status: "done".into(),
                done: 4,
                failed: 0,
            },
            BatchSummary {
                id: 11,
                created: now - 7200,
                action: "copy".into(),
                args: "mrn rename ~/Downloads/Comics -o ~/Media/Comics -a copy --comicinfo".into(),
                status: "undone".into(),
                done: 9,
                failed: 1,
            },
        ])));
        shoot(&app, "5-history", &theme);

        app.tab = Tab::Settings;
        shoot(&app, "6-settings", &theme);
        shoot(&app, "7-settings-light", &Theme::Light);
    }
}
