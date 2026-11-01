//! Kodi/Jellyfin `.nfo` files.

use std::fmt::Write;

use crate::providers::{Candidate, Episode, Source};

/// Escape text for XML element content.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c if c.is_control() && !matches!(c, '\n' | '\t') => {}
            c => out.push(c),
        }
    }
    out
}

/// A tiny XML builder for flat documents.
pub(crate) struct Xml {
    buf: String,
    root: &'static str,
}

impl Xml {
    pub fn new(root: &'static str, attrs: &str) -> Xml {
        let mut buf = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n");
        let _ = writeln!(buf, "<{root}{attrs}>");
        Xml { buf, root }
    }

    pub fn el(&mut self, name: &str, value: Option<impl AsRef<str>>) -> &mut Self {
        if let Some(v) = value {
            let v = v.as_ref().trim();
            if !v.is_empty() {
                let _ = writeln!(self.buf, "  <{name}>{}</{name}>", escape(v));
            }
        }
        self
    }

    pub fn raw(&mut self, line: &str) -> &mut Self {
        let _ = writeln!(self.buf, "  {line}");
        self
    }

    pub fn finish(&mut self) -> String {
        let _ = writeln!(self.buf, "</{}>", self.root);
        std::mem::take(&mut self.buf)
    }
}

fn unique_id(x: &mut Xml, c: &Candidate) {
    if c.id.is_empty() || c.source() == Source::Local {
        return;
    }
    x.raw(&format!(
        "<uniqueid type=\"{}\" default=\"true\">{}</uniqueid>",
        c.source(),
        escape(&c.id)
    ));
}

fn genres(x: &mut Xml, c: &Candidate) {
    for g in &c.genres {
        x.el("genre", Some(g));
    }
}

pub fn movie(c: &Candidate) -> String {
    let mut x = Xml::new("movie", "");
    x.el("title", Some(&c.title))
        .el("year", c.year.map(|y| y.to_string()))
        .el("plot", c.overview.as_ref());
    unique_id(&mut x, c);
    genres(&mut x, c);
    x.finish()
}

pub fn tvshow(c: &Candidate) -> String {
    let mut x = Xml::new("tvshow", "");
    x.el("title", Some(&c.title))
        .el("year", c.year.map(|y| y.to_string()))
        .el("plot", c.overview.as_ref());
    unique_id(&mut x, c);
    genres(&mut x, c);
    x.finish()
}

pub fn episode(c: &Candidate, ep: Option<&Episode>, season: Option<u32>, numbers: &[u32]) -> String {
    let mut x = Xml::new("episodedetails", "");
    let season = ep.map(|e| e.season).or(season);
    let number = ep.map(|e| e.episode).or(numbers.first().copied());
    x.el("title", ep.and_then(|e| e.title.as_ref()))
        .el("showtitle", Some(&c.title))
        .el("season", season.map(|s| s.to_string()))
        .el("episode", number.map(|n| n.to_string()))
        .el("aired", ep.and_then(|e| e.air_date.as_ref()))
        .el("plot", ep.and_then(|e| e.overview.as_ref()));
    x.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movie_nfo() {
        let c = Candidate {
            source: Some(Source::Tmdb),
            id: "438631".into(),
            title: "Dune & <Sand>".into(),
            year: Some(2021),
            genres: vec!["Science Fiction".into()],
            ..Default::default()
        };
        let s = movie(&c);
        assert!(s.contains("<title>Dune &amp; &lt;Sand&gt;</title>"), "{s}");
        assert!(s.contains("<uniqueid type=\"tmdb\" default=\"true\">438631</uniqueid>"));
        assert!(s.contains("<genre>Science Fiction</genre>"));
        assert!(!s.contains("<plot>"));
        assert!(s.trim_end().ends_with("</movie>"));
    }
}
