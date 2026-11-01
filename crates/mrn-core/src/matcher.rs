//! Rank provider candidates against what we parsed from the filename.

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::providers::Candidate;

/// Default confidence needed to accept a match without asking.
pub const DEFAULT_THRESHOLD: f64 = 0.85;

/// Normalize a title for comparison: strip accents, lowercase, `&` → `and`,
/// drop punctuation, collapse spaces, drop a leading article.
pub fn normalize(s: &str) -> String {
    let folded: String = s
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
        .replace(['\'', '’', '`'], "")
        .replace('&', " and ");
    let cleaned: String = folded
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let words: Vec<&str> = cleaned.split_whitespace().collect();
    let words = match words.first() {
        Some(&"the") | Some(&"a") | Some(&"an") if words.len() > 1 => &words[1..],
        _ => &words[..],
    };
    words.join(" ")
}

/// Similarity of two titles in `0.0..=1.0`.
pub fn title_similarity(a: &str, b: &str) -> f64 {
    let (a, b) = (normalize(a), normalize(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    // Compare with spaces removed too, so "Spiderman" ~ "Spider-Man".
    let squash = |s: &str| s.replace(' ', "");
    if squash(&a) == squash(&b) {
        return 0.98;
    }
    // Jaro-Winkler alone over-rewards shared prefixes ("Dune" vs "Dune: Part
    // Two"); edit distance penalises the extra words.
    (strsim::jaro_winkler(&a, &b) + strsim::normalized_levenshtein(&a, &b)) / 2.0
}

/// The query as written, plus a variant without parenthesised parts
/// ("The Office (US)" → "The Office").
fn query_variants(query: &str) -> Vec<String> {
    let mut v = vec![query.to_string()];
    let mut stripped = String::new();
    let mut depth = 0;
    for c in query.chars() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            c if depth == 0 => stripped.push(c),
            _ => {}
        }
    }
    let stripped = stripped.trim().to_string();
    if !stripped.is_empty() && stripped != query {
        v.push(stripped);
    }
    v
}

/// Score one candidate. `rank` is its position in the provider's result list
/// and breaks ties in favour of the provider's own ordering.
pub fn score(query: &str, year: Option<u16>, candidate: &Candidate, rank: usize) -> f64 {
    let queries = query_variants(query);
    let sim = std::iter::once(&candidate.title)
        .chain(candidate.alt_titles.iter())
        .flat_map(|t| queries.iter().map(move |q| title_similarity(q, t)))
        .fold(0.0, f64::max);

    let year_adj = match (year, candidate.year) {
        (Some(q), Some(c)) => match q.abs_diff(c) {
            0 => 0.08,
            1 => 0.03,
            _ => -0.2,
        },
        _ => 0.0,
    };
    let rank_adj = -0.005 * rank.min(10) as f64;
    (sim + year_adj + rank_adj).clamp(0.0, 1.0)
}

/// Score and sort candidates, best first.
pub fn rank(query: &str, year: Option<u16>, candidates: Vec<Candidate>) -> Vec<Scored> {
    let mut scored: Vec<Scored> = candidates
        .into_iter()
        .enumerate()
        .map(|(i, c)| Scored {
            score: score(query, year, &c, i),
            candidate: c,
        })
        .collect();
    scored.sort_by(|a, b| b.score.total_cmp(&a.score));
    scored
}

#[derive(Debug, Clone)]
pub struct Scored {
    pub candidate: Candidate,
    pub score: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(normalize("The Lord of the Rings: The Two Towers"), "lord of the rings the two towers");
        assert_eq!(normalize("Pokémon"), "pokemon");
        assert_eq!(normalize("Law & Order"), "law and order");
        assert_eq!(normalize("The"), "the");
    }

    #[test]
    fn similarity() {
        assert_eq!(title_similarity("Spiderman", "Spider-Man"), 0.98);
        assert!(title_similarity("The Expanse", "Expanse") >= 0.99);
        assert!(title_similarity("Dune", "Dune: Part Two") < 0.85);
        assert!(title_similarity("Its Always Sunny", "It's Always Sunny") >= 0.99);
        assert!(title_similarity("game of thrones", "Game of Thrones") >= 0.99);
    }

    #[test]
    fn scoring_uses_year_and_variants() {
        let c = |title: &str, year| Candidate {
            title: title.into(),
            year: Some(year),
            ..Default::default()
        };
        assert!(score("The Office (US)", None, &c("The Office", 2005), 0) >= 0.99);
        let right = score("Dune", Some(2021), &c("Dune", 2021), 1);
        let wrong = score("Dune", Some(2021), &c("Dune", 1984), 0);
        assert!(right > wrong);
        assert!(wrong < DEFAULT_THRESHOLD);
        let ranked = rank("Dune", Some(2021), vec![c("Dune", 1984), c("Dune: Part Two", 2024), c("Dune", 2021)]);
        assert_eq!(ranked[0].candidate.year, Some(2021));
    }
}
