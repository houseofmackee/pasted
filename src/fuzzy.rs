//! Fuzzy matching for search: fzf-style subsequences ("gthb" finds
//! "github") and small typos ("quartely" finds "quarterly").
//!
//! Every query word must match one whitespace-separated token of the item,
//! either as a subsequence (letters in order within that token, so letters
//! scattered across a sentence do not count) or by edit distance against a
//! word (one edit for words of 4 to 7 letters, two from 8).

use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Minimum subsequence score per query character (nucleo awards 16 per
/// matched character plus bonuses, minus gap penalties).
pub const MIN_SCORE_PER_CHAR: u32 = 13;
/// Score credited to a word matched by typo tolerance, per character.
const TYPO_SCORE_PER_CHAR: u32 = 9;
/// Only the start of long text is considered; a match there is enough to
/// surface the item.
const MAX_HAYSTACK_CHARS: usize = 1500;

pub struct FuzzyQuery {
    words: Vec<(Vec<char>, Atom)>,
    matcher: Matcher,
    buf: Vec<char>,
}

fn normalize(word: &str) -> Vec<char> {
    word.chars().flat_map(|c| c.to_lowercase()).map(nucleo_matcher::chars::normalize).collect()
}

/// Optimal string alignment distance (Damerau-Levenshtein without
/// substring transpositions), giving up once it exceeds `max`.
pub fn edit_distance(a: &[char], b: &[char], max: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let (n, m) = (a.len(), b.len());
    let mut prev2 = vec![0usize; m + 1];
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut cur = vec![0usize; m + 1];
    for i in 1..=n {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=m {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut v = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(prev2[j - 2] + 1);
            }
            cur[j] = v;
            row_min = row_min.min(v);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    (prev[m] <= max).then_some(prev[m])
}

impl FuzzyQuery {
    /// `None` when there is nothing to match.
    pub fn new(text: &str) -> Option<FuzzyQuery> {
        let words: Vec<_> = text
            .split_whitespace()
            .map(|w| (normalize(w), Atom::new(w, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy, false)))
            .collect();
        (!words.is_empty()).then(|| FuzzyQuery { words, matcher: Matcher::new(Config::DEFAULT), buf: Vec::new() })
    }

    fn typo_match(word: &[char], haystack: &str) -> bool {
        if word.len() < 4 {
            return false;
        }
        let max = if word.len() >= 8 { 2 } else { 1 };
        haystack.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).any(|w| {
            // Cheap checks first, without allocating: the length must be
            // within reach, and typos rarely change both of the first two
            // letters.
            if w.chars().count().abs_diff(word.len()) > max {
                return false;
            }
            let mut start = w.chars().flat_map(char::to_lowercase).map(nucleo_matcher::chars::normalize);
            let (c0, c1) = (start.next(), start.next());
            if c0 != word.first().copied() && c1 != word.first().copied() {
                return false;
            }
            edit_distance(word, &normalize(w), max).is_some()
        })
    }

    /// Best subsequence score of `atom` within a single token of `haystack`.
    fn token_score(&mut self, chars: &[char], atom: &Atom, haystack: &str) -> Option<u32> {
        let first = *chars.first()?;
        let mut best: Option<u32> = None;
        for token in haystack.split_whitespace() {
            // Cheap prefilter before running the matcher.
            if token.chars().count() < chars.len()
                || !token.chars().flat_map(char::to_lowercase).map(nucleo_matcher::chars::normalize).any(|c| c == first)
            {
                continue;
            }
            if let Some(s) = atom.score(Utf32Str::new(token, &mut self.buf), &mut self.matcher) {
                best = Some(best.map_or(s as u32, |b| b.max(s as u32)));
            }
        }
        best
    }

    /// Score for `haystack`, or `None` if some word does not match.
    pub fn score(&mut self, haystack: &str) -> Option<u32> {
        let haystack = match haystack.char_indices().nth(MAX_HAYSTACK_CHARS) {
            Some((i, _)) => &haystack[..i],
            None => haystack,
        };
        let mut total = 0u32;
        let words = std::mem::take(&mut self.words);
        let result = (|| {
            for (chars, atom) in &words {
                let len = chars.len() as u32;
                let subsequence = self.token_score(chars, atom, haystack).filter(|s| *s >= MIN_SCORE_PER_CHAR * len);
                match subsequence {
                    Some(s) => total += s,
                    None if Self::typo_match(chars, haystack) => total += TYPO_SCORE_PER_CHAR * len,
                    None => return None,
                }
            }
            Some(total)
        })();
        self.words = words;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(q: &str, h: &str) -> bool {
        FuzzyQuery::new(q).unwrap().score(h).is_some()
    }

    #[test]
    #[ignore = "prints scores used to choose MIN_SCORE_PER_CHAR"]
    fn calibrate() {
        let cases = [
            ("gthb", "https://github.com/rust-lang/rust"),
            ("gthb", "The meeting got through half of the agenda before lunch, because everyone was busy."),
            ("qrtrly", "Quarterly roadmap review"),
            ("rdmp", "Quarterly roadmap review"),
            ("slck", "Slack message from the design team"),
            ("slck", "She lost her lucky coin keeping track of the score"),
            ("jra", "JIRA-1234 fix login"),
            ("jra", "just read about it"),
        ];
        for (q, h) in cases {
            let atom = Atom::new(q, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy, false);
            let mut buf = Vec::new();
            let s = atom.score(Utf32Str::new(h, &mut buf), &mut Matcher::new(Config::DEFAULT));
            println!("{q:>8} {:>5?} per char {:>5?}  {h}", s, s.map(|s| s as usize / q.len()));
        }
    }

    #[test]
    fn subsequences_close_together() {
        assert!(matches("gthb", "https://github.com/rust-lang/rust"));
        assert!(matches("rdmp", "Quarterly roadmap review"));
        assert!(matches("slck", "Slack message from the design team"));
        assert!(!matches(
            "gthb",
            "The meeting got through half of the agenda before lunch, because everyone was busy."
        ));
        assert!(!matches("slck", "She lost her lucky coin keeping track of the score"));
        assert!(!matches("jra", "just read about it"));
        assert!(matches("jra", "JIRA-1234 fix login"));
    }

    #[test]
    fn typos() {
        assert!(matches("quartely", "Quarterly roadmap review"));
        assert!(matches("roadamp", "Quarterly roadmap review"), "transposition");
        assert!(matches("reveiw", "Quarterly roadmap review"));
        assert!(matches("rodmap qarterly", "Quarterly roadmap review"));
        assert!(!matches("roadkill", "Quarterly roadmap review"));
        assert!(!matches("cat", "the cut"), "short words need an exact or subsequence match");
    }

    #[test]
    fn every_word_must_match_and_accents_fold() {
        assert!(!matches("roadmap budget", "Quarterly roadmap review"));
        assert!(matches("cafe", "Meet at Café Lumière"));
        assert!(matches("lumiere", "Meet at Café Lumière"), "accented words reach the typo check");
    }

    #[test]
    fn typo_check_counts_characters_not_bytes() {
        let typo = |q: &str, h: &str| FuzzyQuery::typo_match(&normalize(q), h);
        assert!(typo("東京タワ展望台", "週末 東京タワー展望台 へ"), "one missing character");
        assert!(typo("lumeire", "Café Lumière"));
        assert!(!typo("roadmap", "a very long unrelated word: roadmapping-and-planning"));
    }

    #[test]
    fn distances() {
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(edit_distance(&c("kitten"), &c("sitten"), 2), Some(1));
        assert_eq!(edit_distance(&c("abcd"), &c("abdc"), 1), Some(1));
        assert_eq!(edit_distance(&c("abc"), &c("xyz"), 2), None);
        assert_eq!(edit_distance(&c("short"), &c("muchlongerword"), 2), None);
    }
}
