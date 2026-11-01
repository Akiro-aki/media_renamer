//! The History tab: past batches, with undo.

use iced::widget::{Column, button, column, row, scrollable, space, text};
use iced::{Center, Element, Fill, Font, Task};
use mrn_core::history::{BatchSummary, History};
use mrn_core::preview::ago;

use crate::rename_tab::undo_with;
use crate::widgets::{bad, card, dim, ok, truncate, warn};
use crate::worker::blocking;

#[derive(Debug, Clone)]
pub enum Msg {
    Reload,
    Loaded(Result<Vec<BatchSummary>, String>),
    Ask(i64),
    Keep,
    Undo(i64, bool),
    Undone(i64, Result<String, String>),
}

#[derive(Default)]
pub struct State {
    batches: Vec<BatchSummary>,
    loading: bool,
    error: Option<String>,
    confirm: Option<i64>,
    working: Option<i64>,
    /// Last undo result, and whether "Undo anyway" makes sense.
    message: Option<(i64, Result<String, String>)>,
}

impl State {
    pub fn reload(&mut self) -> Task<Msg> {
        self.loading = true;
        Task::perform(
            blocking(|| {
                History::open_default()
                    .and_then(|h| h.batches(100))
                    .map_err(|e| format!("{e:#}"))
            }),
            Msg::Loaded,
        )
    }

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Reload => return self.reload(),
            Msg::Loaded(r) => {
                self.loading = false;
                match r {
                    Ok(b) => {
                        self.batches = b;
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Msg::Ask(id) => self.confirm = Some(id),
            Msg::Keep => self.confirm = None,
            Msg::Undo(id, force) => {
                self.confirm = None;
                self.working = Some(id);
                return Task::perform(
                    blocking(move || undo_with(id, force).map_err(|e| format!("{e:#}"))),
                    move |r| Msg::Undone(id, r),
                );
            }
            Msg::Undone(id, r) => {
                self.working = None;
                self.message = Some((id, r));
                return self.reload();
            }
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let mut page = Column::new().spacing(10).push(
            row![
                text("Past batches").size(18),
                space().width(Fill),
                button("Refresh")
                    .style(button::secondary)
                    .on_press_maybe((!self.loading).then_some(Msg::Reload)),
            ]
            .align_y(Center),
        );
        if let Some(e) = &self.error {
            page = page.push(text(e).style(bad));
        }
        if let Some((id, r)) = &self.message {
            let mut c = Column::new().spacing(8);
            match r {
                Ok(m) => {
                    c = c.push(text(m).size(14));
                    if m.contains("kept because") {
                        c = c.push(
                            button("Undo anyway (revert changed files too)")
                                .style(button::danger)
                                .on_press(Msg::Undo(*id, true)),
                        );
                    }
                }
                Err(e) => c = c.push(text(e).size(14).style(bad)),
            }
            page = page.push(card(c));
        }
        if self.batches.is_empty() && !self.loading && self.error.is_none() {
            page = page.push(dim("No history yet. Every rename is recorded here and can be undone."));
        }

        for b in &self.batches {
            let files = if b.failed > 0 {
                format!("{} files ({} failed)", b.done, b.failed)
            } else {
                format!("{} files", b.done)
            };
            let status_style = match b.status.as_str() {
                "undone" => text::secondary,
                "done" => ok,
                _ => warn,
            };
            let controls: Element<'_, Msg> = if self.working == Some(b.id) {
                dim("Undoing…")
            } else if self.confirm == Some(b.id) {
                row![
                    text("Undo this batch?").size(14),
                    button("Undo").style(button::danger).on_press(Msg::Undo(b.id, false)),
                    button("Keep").style(button::secondary).on_press(Msg::Keep),
                ]
                .spacing(8)
                .align_y(Center)
                .into()
            } else if b.status != "undone" {
                button("Undo")
                    .style(button::secondary)
                    .on_press_maybe(self.working.is_none().then_some(Msg::Ask(b.id)))
                    .into()
            } else {
                space().into()
            };
            page = page.push(card(
                column![
                    row![
                        text(format!("#{}", b.id)).size(15).width(50),
                        text(ago(b.created)).size(14).width(110),
                        text(b.action.as_str()).size(14).width(90),
                        text(files).size(14).width(150),
                        text(b.status.as_str()).size(14).style(status_style).width(Fill),
                        controls,
                    ]
                    .spacing(8)
                    .align_y(Center),
                    text(truncate(&b.args, 160)).size(12).font(Font::MONOSPACE).style(text::secondary),
                ]
                .spacing(6),
            ));
        }
        scrollable(page.padding(iced::Padding::ZERO.right(14))).height(Fill).into()
    }
}
