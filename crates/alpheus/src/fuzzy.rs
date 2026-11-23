//! A small fuzzy search for the command palette and the lists' filter: the query's characters
//! must appear in order in the candidate, case- and accent-insensitive ("cafe" finds "Café").
//!
//! `score` says whether and how well one candidate matches; `rank` orders a list of labels by it.
//! A match scores higher when its characters are contiguous or start a word, and a longer
//! candidate loses one point per ten characters (integer division: candidates in the same block of
//! ten characters tie, and `rank` keeps their original order).
//!
//! Never:
//! - read or write anything but its arguments: it is a pure function of the query and the candidates.
//!
//! See: docs/features/control-center.md.

fn fold(c: char) -> char {
    match c {
        'à' | 'â' | 'ä' | 'À' | 'Â' | 'Ä' => 'a',
        'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
        'î' | 'ï' | 'Î' | 'Ï' => 'i',
        'ô' | 'ö' | 'Ô' | 'Ö' => 'o',
        'ù' | 'û' | 'ü' | 'Ù' | 'Û' | 'Ü' => 'u',
        'ç' | 'Ç' => 'c',
        c => c.to_lowercase().next().unwrap_or(c),
    }
}

/// `s` without accents or uppercase letters ("Café" → "cafe").
pub fn fold_str(s: &str) -> String {
    s.chars().map(fold).collect()
}

/// Score (higher = better) if the characters of `query` (whitespace
/// ignored) appear in order in `cand`; `None` otherwise.
pub fn score(query: &str, cand: &str) -> Option<i32> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).map(fold).collect();
    if q.is_empty() {
        return Some(0);
    }
    let c: Vec<char> = cand.chars().map(fold).collect();
    let (mut qi, mut score, mut prev) = (0usize, 0i32, None::<usize>);
    for (i, &ch) in c.iter().enumerate() {
        if qi < q.len() && ch == q[qi] {
            score += 1;
            if let Some(p) = prev
                && p + 1 == i
            {
                score += 5; // contiguous
            }
            if i == 0 || !c[i - 1].is_alphanumeric() {
                score += 8; // start of word
            }
            prev = Some(i);
            qi += 1;
        }
    }
    // Every character of the query was found. A longer candidate loses a point per ten characters.
    if qi == q.len() { Some(score - c.len() as i32 / 10) } else { None }
}

/// Indices of the labels that match, best first (original order on a tie;
/// an empty query matches everything, in order).
pub fn rank<S: AsRef<str>>(query: &str, labels: &[S]) -> Vec<usize> {
    let mut v: Vec<(usize, i32)> = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        if let Some(s) = score(query, label.as_ref()) {
            v.push((i, s));
        }
    }
    v.sort_by_key(|&(_, s)| std::cmp::Reverse(s)); // stable sort: original order on a tie
    v.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    //! Tests of the fuzzy search (`fuzzy.rs`): accents and case, word starts and contiguity, ranking.

    use super::*;

    #[test]
    fn without_accents_or_case() {
        assert!(score("cafe", "Café").is_some());
        assert!(score("DEJA VU", "déjà vu").is_some());
        assert!(score("zz", "Café").is_none());
    }

    #[test]
    fn start_of_word_and_contiguity_win() {
        let labels = ["Keep awake", "Wi-Fi", "Lock"];
        assert_eq!(rank("wifi", &labels), vec![1]);
        assert_eq!(rank("loc", &labels)[0], 2);
    }

    /// Among labels that match the same way, the shorter one comes first, and a contiguous match
    /// beats a scattered one.
    #[test]
    fn a_shorter_label_and_a_contiguous_match_rank_first() {
        let long = "lock the session and wait for a very long time here";
        assert_eq!(rank("lock", &[long, "lock"]), vec![1, 0]);
        assert!(score("abc", "xabc").unwrap() > score("abc", "xaxbxc").unwrap());
    }

    /// The length penalty is one point per ten characters: 9 and 4 characters are in the same
    /// block, so they tie and keep their original order.
    #[test]
    fn labels_in_the_same_block_of_ten_characters_tie() {
        assert_eq!(rank("lock", &["lock aaaa", "lock"]), vec![0, 1]);
        assert_eq!(score("lock", "lock aaaa"), score("lock", "lock"));
    }

    #[test]
    fn empty_query_returns_everything_in_order() {
        assert_eq!(rank("", &["b", "a", "c"]), vec![0, 1, 2]);
    }
}
