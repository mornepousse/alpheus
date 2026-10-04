//! Minimal fuzzy search (palette, list filtering): subsequence match,
//! case- and accent-insensitive (plain letters still match accented ones,
//! e.g. "cafe" finds "Café").

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
            if prev.is_some_and(|p| p + 1 == i) {
                score += 5; // contiguous
            }
            if i == 0 || !c[i - 1].is_alphanumeric() {
                score += 8; // start of word
            }
            prev = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then(|| score - c.len() as i32 / 10)
}

/// Indices of the labels that match, best first (original order on a tie;
/// an empty query matches everything, in order).
pub fn rank<S: AsRef<str>>(query: &str, labels: &[S]) -> Vec<usize> {
    let mut v: Vec<(usize, i32)> = labels.iter().enumerate().filter_map(|(i, l)| score(query, l.as_ref()).map(|s| (i, s))).collect();
    v.sort_by_key(|&(_, s)| std::cmp::Reverse(s)); // stable sort: original order on a tie
    v.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn empty_query_returns_everything_in_order() {
        assert_eq!(rank("", &["b", "a", "c"]), vec![0, 1, 2]);
    }
}
