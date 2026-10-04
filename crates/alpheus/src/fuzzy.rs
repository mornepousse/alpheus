//! Recherche floue minimale (palette, filtre des listes) : sous-séquence,
//! insensible à la casse et aux accents (« eteindre » trouve « Éteindre »).

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

/// `s` sans accents ni majuscules (« Réseau » → « reseau »).
pub fn plier(s: &str) -> String {
    s.chars().map(fold).collect()
}

/// Score (plus grand = meilleur) si les caractères de `query` (espaces
/// ignorés) apparaissent dans l'ordre dans `cand` ; sinon None.
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
                score += 5; // contigu
            }
            if i == 0 || !c[i - 1].is_alphanumeric() {
                score += 8; // début de mot
            }
            prev = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then(|| score - c.len() as i32 / 10)
}

/// Indices des libellés qui correspondent, du meilleur au moins bon
/// (ordre d'origine à égalité ; requête vide = tout, dans l'ordre).
pub fn rank<S: AsRef<str>>(query: &str, labels: &[S]) -> Vec<usize> {
    let mut v: Vec<(usize, i32)> = labels
        .iter()
        .enumerate()
        .filter_map(|(i, l)| score(query, l.as_ref()).map(|s| (i, s)))
        .collect();
    v.sort_by_key(|&(_, s)| std::cmp::Reverse(s)); // tri stable : ordre d'origine à égalité
    v.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sans_accents_ni_casse() {
        assert!(score("eteindre", "Éteindre").is_some());
        assert!(score("NE PAS", "Ne pas déranger").is_some());
        assert!(score("zz", "Éteindre").is_none());
    }

    #[test]
    fn debut_de_mot_et_contiguite_l_emportent() {
        let labels = ["Garder éveillé", "Wi-Fi", "Verrouiller"];
        assert_eq!(rank("wifi", &labels), vec![1]);
        assert_eq!(rank("ver", &labels)[0], 2);
    }

    #[test]
    fn requete_vide_tout_dans_l_ordre() {
        assert_eq!(rank("", &["b", "a", "c"]), vec![0, 1, 2]);
    }
}
