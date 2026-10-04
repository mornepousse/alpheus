//! Heure locale (fuseau du système, `localtime_r`) et format à la strftime,
//! en français, sans dépendance. Directives : %a %A %e %d %b %B %m %Y %H %M
//! %S %%. `%e` = jour sans zéro ni espace (« 4 »).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

// La crate `libc` de ce dépôt ne déclare `tzset` que pour Windows : sur
// Unix, c'est pourtant une fonction C standard (time.h), déjà liée via la
// bibliothèque C du système. On la déclare donc nous-mêmes.
unsafe extern "C" {
    fn tzset();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeureLocale {
    pub annee: i32,
    /// 1 à 12.
    pub mois: u8,
    pub jour: u8,
    /// 0 = dimanche … 6 = samedi.
    pub jour_semaine: u8,
    pub heure: u8,
    pub minute: u8,
    pub seconde: u8,
}

const JOURS: [&str; 7] = ["dimanche", "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi"];
const JOURS_COURTS: [&str; 7] = ["dim.", "lun.", "mar.", "mer.", "jeu.", "ven.", "sam."];
const MOIS: [&str; 12] = [
    "janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre",
];
const MOIS_COURTS: [&str; 12] = [
    "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.", "déc.",
];
const DIRECTIVES: &str = "aAedbBmYHMS%";

impl HeureLocale {
    /// Maintenant, et les nanosecondes écoulées dans la seconde (pour caler
    /// le prochain tic).
    pub fn maintenant() -> (HeureLocale, u32) {
        let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        (Self::depuis_epoch(d.as_secs() as i64), d.subsec_nanos())
    }

    /// Secondes depuis l'époque Unix → heure locale du système.
    pub fn depuis_epoch(secs: i64) -> HeureLocale {
        let t = secs as libc::time_t;
        // SAFETY : tzset() ne fait que relire TZ et le fichier de zone ; pas
        // de pointeur gardé. Sans cet appel, `localtime_r` garde le fuseau
        // lu à la première utilisation : un changement de fuseau pendant la
        // vie du démon (un portable qui change de continent, un TZ modifié
        // par erreur puis corrigé) ne serait jamais pris en compte.
        unsafe { tzset() };
        // SAFETY : tm est une structure C simple, valide remplie de zéros ;
        // localtime_r écrit dedans et ne garde aucun pointeur.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY : `t` et `tm` vivent pendant l'appel.
        if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
            return HeureLocale { annee: 1970, mois: 1, jour: 1, jour_semaine: 4, heure: 0, minute: 0, seconde: 0 };
        }
        HeureLocale {
            annee: tm.tm_year + 1900,
            mois: (tm.tm_mon + 1).clamp(1, 12) as u8,
            jour: tm.tm_mday.clamp(1, 31) as u8,
            jour_semaine: tm.tm_wday.clamp(0, 6) as u8,
            heure: tm.tm_hour.clamp(0, 23) as u8,
            minute: tm.tm_min.clamp(0, 59) as u8,
            seconde: tm.tm_sec.clamp(0, 59) as u8, // seconde intercalaire (60) ramenée à 59
        }
    }
}

/// Le format n'emploie-t-il que des directives connues ?
pub fn valider(fmt: &str) -> Result<(), String> {
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c == '%' {
            match it.next() {
                Some(d) if DIRECTIVES.contains(d) => {}
                Some(d) => return Err(format!("directive inconnue « %{d} » (connues : %a %A %e %d %b %B %m %Y %H %M %S %%)")),
                None => return Err("« % » seul en fin de format".into()),
            }
        }
    }
    Ok(())
}

pub fn formater(fmt: &str, h: &HeureLocale) -> String {
    let mut s = String::new();
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            s.push(c);
            continue;
        }
        match it.next() {
            Some('a') => s.push_str(JOURS_COURTS[h.jour_semaine as usize % 7]),
            Some('A') => s.push_str(JOURS[h.jour_semaine as usize % 7]),
            Some('e') => s.push_str(&h.jour.to_string()),
            Some('d') => s.push_str(&format!("{:02}", h.jour)),
            Some('b') => s.push_str(MOIS_COURTS[(h.mois as usize + 11) % 12]),
            Some('B') => s.push_str(MOIS[(h.mois as usize + 11) % 12]),
            Some('m') => s.push_str(&format!("{:02}", h.mois)),
            Some('Y') => s.push_str(&h.annee.to_string()),
            Some('H') => s.push_str(&format!("{:02}", h.heure)),
            Some('M') => s.push_str(&format!("{:02}", h.minute)),
            Some('S') => s.push_str(&format!("{:02}", h.seconde)),
            Some('%') => s.push('%'),
            // Validé au chargement : une directive inconnue ne devrait pas
            // arriver ici ; on la recopie telle quelle plutôt que de paniquer.
            Some(d) => {
                s.push('%');
                s.push(d);
            }
            None => s.push('%'),
        }
    }
    s
}

pub fn a_des_secondes(fmt: &str) -> bool {
    fmt.contains("%S")
}

/// Marge après la frontière : on ne se réveille jamais une poussière avant.
pub const MARGE: Duration = Duration::from_millis(20);

/// Délai jusqu'au prochain changement d'affichage, d'après la seconde
/// courante (0-59) et les nanosecondes écoulées dans cette seconde.
pub fn prochain_tic(seconde: u8, nanos: u32, secondes: bool) -> Duration {
    let reste_seconde = Duration::from_nanos(1_000_000_000 - u64::from(nanos.min(999_999_999)));
    if secondes {
        return reste_seconde + MARGE;
    }
    let secondes_restantes = 59 - u64::from(seconde.min(59));
    Duration::from_secs(secondes_restantes) + reste_seconde + MARGE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dimanche 4 octobre 2026, 14:05:09.
    const DIM: HeureLocale = HeureLocale { annee: 2026, mois: 10, jour: 4, jour_semaine: 0, heure: 14, minute: 5, seconde: 9 };

    #[test]
    fn format_francais() {
        assert_eq!(formater("%a %e %b", &DIM), "dim. 4 oct.");
        assert_eq!(formater("%A %d %B %Y", &DIM), "dimanche 04 octobre 2026");
        assert_eq!(formater("%H:%M:%S", &DIM), "14:05:09");
        assert_eq!(formater("%d/%m 100%%", &DIM), "04/10 100%");
        let fev = HeureLocale { mois: 2, jour_semaine: 3, ..DIM };
        assert_eq!(formater("%a %b", &fev), "mer. févr.");
        let dec = HeureLocale { mois: 12, jour_semaine: 6, ..DIM };
        assert_eq!(formater("%A %B", &dec), "samedi décembre");
        assert_eq!(formater("", &DIM), "");
    }

    #[test]
    fn validation_des_formats() {
        assert_eq!(valider("%H:%M"), Ok(()));
        assert_eq!(valider("%a %e %b"), Ok(()));
        assert_eq!(valider(""), Ok(()));
        assert!(valider("%H:%Q").unwrap_err().contains("%Q"));
        assert!(valider("%H%").unwrap_err().contains("fin"));
    }

    #[test]
    fn tic_a_la_minute_ou_a_la_seconde() {
        assert_eq!(prochain_tic(30, 0, false), Duration::from_secs(30) + MARGE);
        assert_eq!(prochain_tic(0, 0, false), Duration::from_secs(60) + MARGE);
        assert_eq!(prochain_tic(59, 999_000_000, false), Duration::from_millis(1) + MARGE);
        assert_eq!(prochain_tic(10, 500_000_000, true), Duration::from_millis(500) + MARGE);
        assert!(prochain_tic(60, 2_000_000_000, false) <= Duration::from_secs(1) + MARGE, "hors bornes : pas de débordement");
    }

    #[test]
    fn tzset_rappele_a_chaque_lecture() {
        // Décalages fixes (« FOO-3 » = UTC+3, sans « : ») plutôt que des noms
        // de zone (« Europe/Paris ») : aucune base tzdata nécessaire, le test
        // reste portable (sandbox ou CI sans /usr/share/zoneinfo). Seule
        // CETTE fonction lit TZ (`tzset`, voir `depuis_epoch`) ; aucun autre
        // test de cette suite n'en dépend, le risque de l'écriture d'une
        // variable d'environnement partagée par les fils de test est nul en
        // pratique ici.
        let avant = std::env::var("TZ").ok();
        // SAFETY : seul ce test lit ou écrit TZ dans cette suite.
        unsafe { std::env::set_var("TZ", "FOO-3") }; // UTC+3
        let h1 = HeureLocale::depuis_epoch(1_700_000_000);
        // SAFETY : idem.
        unsafe { std::env::set_var("TZ", "BAR2") }; // UTC-2
        let h2 = HeureLocale::depuis_epoch(1_700_000_000);
        assert_ne!(h1.heure, h2.heure, "le fuseau change : sans tzset(), la deuxième lecture garderait le premier fuseau");
        match avant {
            // SAFETY : idem.
            Some(v) => unsafe { std::env::set_var("TZ", v) },
            None => unsafe { std::env::remove_var("TZ") },
        }
    }

    #[test]
    fn heure_locale_du_systeme_coherente() {
        // 2026-10-15 12:00 UTC : même mois et même année dans tout fuseau (±14 h).
        let h = HeureLocale::depuis_epoch(1_792_065_600);
        assert_eq!((h.annee, h.mois), (2026, 10), "{h:?}");
        assert!((14..=16).contains(&h.jour), "{h:?}");
        // Le 15/10/2026 est un jeudi (4) ; la veille ou le lendemain selon le fuseau.
        assert!((3..=5).contains(&h.jour_semaine), "{h:?}");
        let (m, nanos) = HeureLocale::maintenant();
        assert!(m.annee >= 2026 && nanos < 1_000_000_000);
    }
}
