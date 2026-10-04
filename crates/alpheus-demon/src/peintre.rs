//! Dessin de la grille dans un tampon ARGB8888 (wl_shm : octets B, G, R, A),
//! case par case : fond de chaque case, puis glyphe façonné par cosmic-text
//! et posé au bord gauche de sa case. La grille reste alignée même quand
//! une police de repli (emoji, CJK) a une autre chasse.
//!
//! Mémoire bornée : la base de polices ne contient que les faces utiles
//! (cosmic-text analyse d'emblée toutes les faces à chasse fixe de sa
//! base), et le cache de glyphes est vidé au-delà de `CACHE_MAX` entrées.

use crate::config::PoliceConfig;
use crate::grille::{Grille, Teinte, SUITE};
use alpheus_commun::theme::{Palette, Rgb};
use cosmic_text::{fontdb, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
use std::path::{Path, PathBuf};

/// Glyphes rastérisés gardés au plus (≈ 1 Mo à 14 px).
pub const CACHE_MAX: usize = 2048;

pub struct Polices {
    fs: FontSystem,
    cache: SwashCache,
    tampon: Buffer,
    famille: String,
    taille: f32,
    hauteur: u32,
    echelle: i32,
    avance: f32,
    /// Avance déjà mesurée pour chaque échelle vue (petit tableau : il n'y a
    /// en pratique qu'une poignée d'échelles distinctes par session).
    /// Évite de remettre en forme « M » à chaque alternance.
    avances: Vec<(i32, f32)>,
}

pub fn couleur(p: &Palette, t: Teinte) -> Rgb {
    match t {
        Teinte::Fond => p.bg,
        Teinte::Texte => p.fg,
        Teinte::Accent => p.accent,
        Teinte::Estompe => p.muted,
        Teinte::Surface => p.surface,
        Teinte::Erreur => p.error,
    }
}

/// Nombre de cases dans une largeur physique.
pub fn colonnes(largeur_phys: u32, avance: f32) -> u16 {
    if avance <= 0.0 {
        return 0;
    }
    ((largeur_phys as f32 / avance).floor() as u32).min(u16::MAX as u32) as u16
}

/// Case sous un point (x en px logiques, comme le pointeur Wayland).
pub fn colonne_sous(x_logique: f64, echelle: i32, avance: f32) -> u16 {
    if avance <= 0.0 || x_logique <= 0.0 {
        return 0;
    }
    ((x_logique * echelle.max(1) as f64) / avance as f64).floor().min(u16::MAX as f64) as u16
}

fn attrs(famille: &str, gras: bool) -> Attrs<'_> {
    let a = Attrs::new().family(Family::Name(famille));
    if gras { a.weight(Weight::BOLD) } else { a }
}

/// Fond uni sur [x0, x1) × [0, hauteur).
fn remplir(px: &mut [u8], largeur: u32, x0: u32, x1: u32, hauteur: u32, Rgb(r, g, b): Rgb) {
    for y in 0..hauteur {
        for x in x0..x1.min(largeur) {
            // usize d'emblée : `(y * largeur + x) * 4` peut dépasser u32
            // sur un grand tampon avant la conversion.
            let o = (y as usize * largeur as usize + x as usize) * 4;
            px[o..o + 4].copy_from_slice(&[b, g, r, 0xff]);
        }
    }
}

/// Mélange un pixel de glyphe (couleur et couverture) sur le fond.
fn melanger(px: &mut [u8], largeur: u32, hauteur: u32, x: i32, y: i32, c: Color) {
    if x < 0 || y < 0 || x as u32 >= largeur || y as u32 >= hauteur {
        return;
    }
    let a = c.a() as u32;
    if a == 0 {
        return;
    }
    // x et y sont positifs et dans les bornes ici : usize d'emblée.
    let o = (y as usize * largeur as usize + x as usize) * 4;
    let src = [c.b(), c.g(), c.r()];
    for k in 0..3 {
        px[o + k] = ((src[k] as u32 * a + px[o + k] as u32 * (255 - a)) / 255) as u8;
    }
    px[o + 3] = 0xff;
}

fn a_la_famille(f: &fontdb::FaceInfo, famille: &str) -> bool {
    f.families.iter().any(|(n, _)| n == famille)
}

/// Vrai si `famille` n'a aucune face droite (style normal, posture
/// verticale) dans la base. Une famille seulement en italique ou oblique
/// est alors traitée comme absente (remplaçante et avertissement, voir
/// `depuis`) plutôt que de ne charger silencieusement aucune face.
fn sans_face_droite(db: &fontdb::Database, famille: &str) -> bool {
    !db.faces().any(|f| a_la_famille(f, famille) && f.style == fontdb::Style::Normal)
}

/// Clé de tri déterministe d'une face, pour choisir une remplaçante : par
/// nom de famille puis chemin puis index, jamais par l'ordre de parcours
/// de `fontdb` (lui-même `read_dir`, qui change d'une machine à l'autre).
fn cle_tri(f: &fontdb::FaceInfo) -> (String, String, u32) {
    let famille = f.families.first().map(|(n, _)| n.clone()).unwrap_or_default();
    let chemin = match &f.source {
        fontdb::Source::File(p) => p.display().to_string(),
        _ => String::new(),
    };
    (famille, chemin, f.index)
}

/// Remplaçante à chasse fixe : parmi les faces droites (style normal), la
/// première par `cle_tri`, pour un choix stable d'une machine à l'autre.
fn repere_remplacante<'a>(faces: impl Iterator<Item = &'a fontdb::FaceInfo>) -> Option<String> {
    faces
        .filter(|f| f.monospaced && f.style == fontdb::Style::Normal)
        .min_by_key(|f| cle_tri(f))
        .and_then(|f| f.families.first().map(|(n, _)| n.clone()))
}

/// Faces à charger pour une famille : la régulière et la grasse (style
/// normal, poids le plus proche de 400 et de 700), sans doublon. Rendues
/// en `FaceInfo` clonées (id remis à `ID::dummy()`, voir `depuis`) plutôt
/// qu'en `Source` : une police empaquetée en collection (.ttc, fréquent
/// pour les polices CJK du système) ne doit apporter que CETTE face à la
/// base finale, jamais toutes les autres faces de son fichier.
fn faces_utiles(inventaire: &fontdb::Database, famille: &str) -> Vec<fontdb::FaceInfo> {
    let faces: Vec<&fontdb::FaceInfo> =
        inventaire.faces().filter(|f| a_la_famille(f, famille) && f.style == fontdb::Style::Normal).collect();
    let proche = |cible: i32| faces.iter().min_by_key(|f| (f.weight.0 as i32 - cible).abs()).map(|f| f.id);
    let mut ids = Vec::new();
    for id in [proche(400), proche(700)].into_iter().flatten() {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.into_iter()
        .filter_map(|id| inventaire.face(id))
        .map(|f| {
            let mut info = f.clone();
            info.id = fontdb::ID::dummy();
            info
        })
        .collect()
}

/// Rend au système la mémoire de l'inventaire (glibc garde sinon les pages
/// libérées pour lui-même).
fn rendre_la_memoire() {
    #[cfg(target_env = "gnu")]
    // SAFETY : malloc_trim(3) ne fait que rendre des pages libres.
    unsafe {
        libc::malloc_trim(0);
    }
}

impl Polices {
    pub fn charger(police: &PoliceConfig, hauteur: u32) -> Result<(Polices, Option<String>), String> {
        let dossier = std::env::var_os("ALPHEUS_POLICES")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| option_env!("ALPHEUS_POLICES").map(PathBuf::from));
        Self::depuis(dossier.as_deref(), police, hauteur)
    }

    pub fn depuis(dossier: Option<&Path>, police: &PoliceConfig, hauteur: u32) -> Result<(Polices, Option<String>), String> {
        // 1. Inventaire (métadonnées seulement, jeté ensuite) : le dossier du
        //    paquet, puis les polices du système si une famille y manque.
        let mut inventaire = fontdb::Database::new();
        if let Some(d) = dossier {
            inventaire.load_fonts_dir(d);
        }
        let voulues = std::iter::once(&police.famille).chain(&police.repli);
        if voulues.clone().any(|f| sans_face_droite(&inventaire, f)) {
            inventaire.load_system_fonts();
        }
        let mut avertissements = Vec::new();
        let famille = if sans_face_droite(&inventaire, &police.famille) {
            // Absente, ou présente seulement en italique/oblique (aucune
            // face droite) : traitée pareil, une remplaçante et un avertissement.
            let remplacante = repere_remplacante(inventaire.faces())
                .ok_or_else(|| format!("aucune police à chasse fixe (police « {} » introuvable)", police.famille))?;
            avertissements.push(format!("police « {} » introuvable, remplacée par « {remplacante} »", police.famille));
            remplacante
        } else {
            police.famille.clone()
        };
        // 2. Base finale : les seules faces utiles.
        let mut db = fontdb::Database::new();
        for f in faces_utiles(&inventaire, &famille) {
            db.push_face_info(f);
        }
        for r in &police.repli {
            let faces = faces_utiles(&inventaire, r);
            if faces.is_empty() {
                avertissements.push(format!("police de repli « {r} » introuvable"));
            }
            for f in faces {
                db.push_face_info(f);
            }
        }
        drop(inventaire);
        rendre_la_memoire();
        db.set_monospace_family(famille.clone());
        let mut fs = FontSystem::new_with_locale_and_db("fr-FR".into(), db);
        let tampon = Buffer::new(&mut fs, Metrics::new(police.taille, hauteur as f32));
        let mut p =
            Polices { fs, cache: SwashCache::new(), tampon, famille, taille: police.taille, hauteur, echelle: 0, avance: 0.0, avances: Vec::new() };
        p.regler(1);
        let avertissement = (!avertissements.is_empty()).then(|| avertissements.join(" ; "));
        Ok((p, avertissement))
    }

    pub fn famille(&self) -> &str {
        &self.famille
    }

    /// Faces dans la base de cosmic-text (garde de la mémoire).
    pub fn faces_chargees(&self) -> usize {
        self.fs.db().len()
    }

    /// Glyphes rastérisés en cache (garde de la mémoire).
    pub fn glyphes_en_cache(&self) -> usize {
        self.cache.image_cache.len()
    }

    /// Taille et métriques pour l'échelle `e` ; mesure l'avance d'une case
    /// (mémorisée par échelle : jamais remesurée deux fois). Le cache de
    /// glyphes n'est PAS vidé ici : ses clés swash incluent la taille, donc
    /// les glyphes de plusieurs échelles cohabitent (écrans à l'échelle 1 et
    /// 2 dessinés en alternance) ; `CACHE_MAX`, dans `glyphe`, reste la
    /// seule borne mémoire.
    fn regler(&mut self, e: i32) {
        let e = e.max(1);
        if e == self.echelle {
            return;
        }
        self.echelle = e;
        let (t, h) = (self.taille * e as f32, (self.hauteur * e as u32) as f32);
        self.tampon.set_metrics_and_size(&mut self.fs, Metrics::new(t, h), None, Some(h));
        self.avance = match self.avances.iter().find(|(echelle, _)| *echelle == e) {
            Some((_, avance)) => *avance,
            None => {
                self.tampon.set_text(&mut self.fs, "M", attrs(&self.famille, false), Shaping::Advanced);
                self.tampon.shape_until_scroll(&mut self.fs, false);
                let avance = self.tampon.layout_runs().flat_map(|r| r.glyphs.iter()).map(|g| g.w).next().unwrap_or(t * 0.6);
                self.avances.push((e, avance));
                avance
            }
        };
    }

    pub fn avance(&mut self, echelle: i32) -> f32 {
        self.regler(echelle);
        self.avance
    }

    pub fn peindre(&mut self, grille: &Grille, palette: &Palette, px: &mut [u8], largeur: u32, hauteur: u32, echelle: i32) {
        if px.len() < (largeur as usize) * (hauteur as usize) * 4 {
            return; // tampon incohérent : rien plutôt qu'une panique
        }
        self.regler(echelle);
        let avance = self.avance;
        let bord = |col: usize| ((col as f32) * avance).round() as u32;
        remplir(px, largeur, 0, largeur, hauteur, couleur(palette, Teinte::Fond));
        for (col, c) in grille.cellules().iter().enumerate() {
            if c.style.bg != Teinte::Fond {
                remplir(px, largeur, bord(col), bord(col + 1), hauteur, couleur(palette, c.style.bg));
            }
        }
        for (col, c) in grille.cellules().iter().enumerate() {
            if c.ch == ' ' || c.ch == SUITE {
                continue;
            }
            self.glyphe(c.ch, c.style.gras, bord(col) as f32, couleur(palette, c.style.fg), px, largeur, hauteur);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn glyphe(&mut self, ch: char, gras: bool, x0: f32, encre: Rgb, px: &mut [u8], largeur: u32, hauteur: u32) {
        // Mémoire bornée : au-delà du plafond, on repart d'un cache vide
        // (les glyphes affichés se rastérisent à nouveau, une fois).
        if self.cache.image_cache.len() >= CACHE_MAX {
            self.cache.image_cache.clear();
            self.cache.outline_command_cache.clear();
        }
        let mut s = [0u8; 4];
        self.tampon.set_text(&mut self.fs, ch.encode_utf8(&mut s), attrs(&self.famille, gras), Shaping::Advanced);
        self.tampon.shape_until_scroll(&mut self.fs, false);
        let Rgb(r, g, b) = encre;
        let base = Color::rgb(r, g, b);
        for run in self.tampon.layout_runs() {
            for gl in run.glyphs.iter() {
                // Posé au bord de la case, quelle que soit sa propre avance.
                let phys = gl.physical((x0 - gl.x, 0.0), 1.0);
                let teinte = gl.color_opt.unwrap_or(base);
                self.cache.with_pixels(&mut self.fs, phys.cache_key, teinte, |dx, dy, c| {
                    melanger(px, largeur, hauteur, phys.x + dx, run.line_y as i32 + phys.y + dy, c);
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HAUTEUR_DEFAUT;
    use crate::grille::Style;

    fn polices() -> Polices {
        let (p, w) = Polices::charger(&PoliceConfig::default(), HAUTEUR_DEFAUT)
            .expect("aucune police à chasse fixe : lancer les tests dans `nix develop` (ALPHEUS_POLICES)");
        assert_eq!(w, None, "JetBrainsMono Nerd Font Mono attendue (nix develop)");
        p
    }

    #[test]
    fn fond_inverse_et_glyphes() {
        let mut p = polices();
        let mut g = Grille::vide(4);
        g.ecrire(0, 4, "A", Style::NORMAL);
        g.ecrire(1, 4, " ", Style::INVERSE);
        g.ecrire(2, 4, "·", Style::NORMAL);
        let pal = Palette::default();
        let av = p.avance(1);
        let (w, h) = ((av * 4.0).ceil() as u32, HAUTEUR_DEFAUT);
        let mut px = vec![0u8; (w * h * 4) as usize];
        p.peindre(&g, &pal, &mut px, w, h, 1);
        let lire = |x: u32, y: u32| {
            let o = ((y * w + x) * 4) as usize;
            (Rgb(px[o + 2], px[o + 1], px[o]), px[o + 3])
        };
        assert_eq!(lire((av * 1.5) as u32, h / 2), (pal.accent, 0xff), "case inversée : fond d'accent");
        let encre_a = (0..av as u32).any(|x| (0..h).any(|y| lire(x, y).0 != pal.bg));
        assert!(encre_a, "aucun pixel dessiné pour « A »");
        let x3 = (av * 3.0).ceil() as u32;
        assert!((x3..w).all(|x| (0..h).all(|y| lire(x, y) == (pal.bg, 0xff))), "case vide : fond, opaque");
    }

    #[test]
    fn seules_les_faces_utiles_sont_chargees() {
        // Régulière + grasse : ≈ 2 Mo. Toute la base (système + dossier
        // Nerd Font) coûtait ≈ 47 Mo à la revue du plan.
        let p = polices();
        assert_eq!(p.faces_chargees(), 2, "faces chargées : {}", p.faces_chargees());
    }

    #[test]
    fn cache_de_glyphes_borne_sur_des_titres_varies() {
        let mut p = polices();
        let pal = Palette::default();
        let av = p.avance(1);
        let (w, h) = ((av * 256.0).ceil() as u32, HAUTEUR_DEFAUT);
        let mut px = vec![0u8; (w * h * 4) as usize];
        let mut plus_haut = 0;
        // 4 096 glyphes distincts (icônes md de la Nerd Font), 256 par titre.
        for bloc in 0..16u32 {
            let titre: String = (0..256u32).filter_map(|i| char::from_u32(0xF0001 + bloc * 256 + i)).collect();
            let mut g = Grille::vide(256);
            g.ecrire(0, 256, &titre, Style::NORMAL);
            p.peindre(&g, &pal, &mut px, w, h, 1);
            plus_haut = plus_haut.max(p.glyphes_en_cache());
            assert!(p.glyphes_en_cache() <= CACHE_MAX, "cache : {} glyphes", p.glyphes_en_cache());
        }
        assert!(plus_haut > 1000, "le cache sert bien ({plus_haut})");
    }

    #[test]
    fn echelle_2_double_la_case() {
        let mut p = polices();
        let (a1, a2) = (p.avance(1), p.avance(2));
        assert!(a1 > 4.0 && (a2 - 2.0 * a1).abs() <= 1.0, "{a1} {a2}");
    }

    /// Un écran à l'échelle 2 et un autre à 1 : `dessiner` alterne les
    /// échelles à chaque trame. Les clés de swash incluent la taille (les
    /// glyphes des deux échelles cohabitent) : le cache ne doit donc jamais
    /// être vidé à l'alternance, seulement borné par CACHE_MAX. Mord sur
    /// `self.cache.image_cache.clear()` dans `regler` : en le rétablissant,
    /// chaque retour à l'échelle 1 ferait retomber le cache à son compte
    /// d'une seule échelle au lieu de rester stable aux deux.
    #[test]
    fn echelles_mixtes_pas_de_vidage_a_chaque_alternance() {
        let mut p = polices();
        let pal = Palette::default();
        let mut g = Grille::vide(4);
        g.ecrire(0, 4, "Ab1", Style::NORMAL);
        let av2 = p.avance(2);
        let (w, h) = ((av2 * 4.0).ceil() as u32, HAUTEUR_DEFAUT * 2);
        let mut px = vec![0u8; (w * h * 4) as usize];
        p.peindre(&g, &pal, &mut px, w, h, 1);
        let n1 = p.glyphes_en_cache();
        p.peindre(&g, &pal, &mut px, w, h, 2);
        let n2 = p.glyphes_en_cache();
        assert!(n2 > n1, "les glyphes de l'échelle 2 s'ajoutent à ceux de l'échelle 1 : {n1} puis {n2}");
        for _ in 0..4 {
            p.peindre(&g, &pal, &mut px, w, h, 1);
            assert_eq!(p.glyphes_en_cache(), n2, "vidage au retour à l'échelle 1 : le cache ne devrait pas rétrécir");
            p.peindre(&g, &pal, &mut px, w, h, 2);
            assert_eq!(p.glyphes_en_cache(), n2, "vidage au retour à l'échelle 2 : le cache ne devrait pas rétrécir");
        }
    }

    #[test]
    fn famille_absente_repli_et_avertissement() {
        let demande = PoliceConfig { famille: "Police Qui N Existe Pas".into(), ..PoliceConfig::default() };
        let (p, w) = Polices::charger(&demande, HAUTEUR_DEFAUT).expect("repli sur une police à chasse fixe");
        let w = w.expect("avertissement attendu");
        assert!(w.contains("Police Qui N Existe Pas") && w.contains(p.famille()), "{w}");
        assert_ne!(p.famille(), "Police Qui N Existe Pas");
        assert!(p.faces_chargees() <= 2, "repli : une famille, pas toute la base ({})", p.faces_chargees());
    }

    #[test]
    fn famille_de_repli_absente_avertie() {
        let demande = PoliceConfig { repli: vec!["Repli Introuvable".into()], ..PoliceConfig::default() };
        let (p, w) = Polices::charger(&demande, HAUTEUR_DEFAUT).unwrap();
        assert!(w.unwrap().contains("Repli Introuvable"));
        assert_eq!(p.faces_chargees(), 2);
    }

    #[test]
    fn colonnes_et_clic() {
        assert_eq!(colonnes(1920, 8.4), 228);
        assert_eq!(colonnes(100, 0.0), 0);
        assert_eq!(colonne_sous(100.0, 2, 16.8), 11);
        assert_eq!(colonne_sous(-3.0, 1, 8.4), 0);
        assert_eq!(colonne_sous(10.0, 0, 8.4), 1, "échelle 0 traitée comme 1");
    }

    #[test]
    fn tampon_trop_petit_sans_panique() {
        let mut p = polices();
        let mut g = Grille::vide(10);
        g.ecrire(0, 10, "abcdefghij", Style::NORMAL);
        let mut px = vec![0u8; 16];
        p.peindre(&g, &Palette::default(), &mut px, 100, 22, 1);
    }

    /// Dossier de test (nom unique par pid, comme `config.rs`), jeté tout
    /// seul à la fin du test (`Drop`) : un nettoyage manuel oublié dans un
    /// test laisserait le dossier derrière lui pour toujours.
    struct DossierJete(PathBuf);

    impl std::ops::Deref for DossierJete {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for DossierJete {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn dossier_de_test(nom: &str) -> DossierJete {
        let d = std::env::temp_dir().join(format!("alpheus-{}-peintre-{nom}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        DossierJete(d)
    }

    fn racine_polices() -> PathBuf {
        std::env::var_os("ALPHEUS_POLICES")
            .map(PathBuf::from)
            .expect("ALPHEUS_POLICES requis (nix develop) pour fabriquer les fixtures de ce test")
    }

    /// Décale de `decalage` tous les offsets de table d'un sfnt autonome
    /// (TrueType/OpenType, pas une collection) : son en-tête (12 octets),
    /// puis `numTables` enregistrements de 16 octets (tag, somme de
    /// contrôle, offset, longueur), où seul le champ offset (3ᵉ, aux
    /// octets 8..12 de l'enregistrement) change. Nécessaire pour empaqueter
    /// une police autonome dans une collection : ses offsets de table sont
    /// absolus (depuis le début du FICHIER), donc faux une fois la police
    /// déplacée à une autre position dans le fichier combiné.
    fn decaler_tables_sfnt(sfnt: &mut [u8], decalage: u32) {
        let num_tables = u16::from_be_bytes([sfnt[4], sfnt[5]]) as usize;
        for i in 0..num_tables {
            let o = 12 + i * 16 + 8;
            let ancien = u32::from_be_bytes([sfnt[o], sfnt[o + 1], sfnt[o + 2], sfnt[o + 3]]);
            sfnt[o..o + 4].copy_from_slice(&(ancien + decalage).to_be_bytes());
        }
    }

    /// Assemble un fichier `.ttc` (une collection) à partir de polices TTF
    /// entières : magie `ttcf`, version (ignorée par ttf-parser), nombre de
    /// faces, leurs offsets, puis les polices collées bout à bout (offsets
    /// de table décalés à leur position dans le fichier combiné, sinon la
    /// collection entière est malformée). Sert à vérifier qu'une collection
    /// n'apporte que la face choisie à la base finale, jamais toutes les
    /// autres faces du même fichier (le piège visé par ce correctif).
    fn fabriquer_ttc(dossier: &Path, faces: &[&str]) -> PathBuf {
        let racine = racine_polices();
        let mut contenus: Vec<Vec<u8>> =
            faces.iter().map(|f| std::fs::read(racine.join(f)).unwrap_or_else(|e| panic!("{f} : {e}"))).collect();
        let entete = 12 + 4 * contenus.len() as u32;
        let mut decalages = Vec::new();
        let mut pos = entete;
        for c in &contenus {
            decalages.push(pos);
            pos += c.len() as u32;
        }
        for (c, &decalage) in contenus.iter_mut().zip(&decalages) {
            decaler_tables_sfnt(c, decalage);
        }
        let mut ttc = Vec::new();
        ttc.extend_from_slice(b"ttcf");
        ttc.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]); // version 1.0, lue mais ignorée
        ttc.extend_from_slice(&(contenus.len() as u32).to_be_bytes());
        for d in &decalages {
            ttc.extend_from_slice(&d.to_be_bytes());
        }
        for c in &contenus {
            ttc.extend_from_slice(c);
        }
        let chemin = dossier.join("fabriquee.ttc");
        std::fs::write(&chemin, &ttc).unwrap();
        chemin
    }

    #[test]
    fn collection_ttc_une_seule_face_par_poids_choisi() {
        let dossier = dossier_de_test("ttc");
        let chemin = fabriquer_ttc(&dossier, &["JetBrainsMonoNerdFontMono-Regular.ttf", "JetBrainsMonoNerdFontMono-Bold.ttf"]);
        let (p, w) = Polices::depuis(Some(&dossier), &PoliceConfig::default(), HAUTEUR_DEFAUT)
            .expect("la collection fabriquée contient une police à chasse fixe");
        assert_eq!(w, None, "{w:?}");
        // Régulière + grasse, une fois chacune : pas 4 (la collection
        // chargée deux fois par `load_font_source`, une par poids choisi).
        assert_eq!(p.faces_chargees(), 2, "faces chargées depuis la collection : {}", p.faces_chargees());
        // Pas assez : « 2 faces » pourrait aussi venir d'une police système
        // homonyme (JetBrainsMono Nerd Font Mono est installée sur ce
        // poste) si la collection fabriquée n'était en fait pas lue.
        // Vérifie que c'est bien ELLE : chaque face chargée a `chemin` pour
        // source, et les deux index de la collection (0 et 1) y sont, sans
        // doublon ni face en trop.
        let mut index: Vec<u32> = Vec::new();
        for f in p.fs.db().faces() {
            let source = match &f.source {
                fontdb::Source::File(c) => c,
                // `fontdb` mappe les fichiers en mémoire (fonctionnalité
                // « memmap ») : le chemin reste accessible, juste à côté
                // des octets mappés.
                fontdb::Source::SharedFile(c, _) => c,
                autre => panic!("source inattendue (pas un fichier) : {autre:?}"),
            };
            assert_eq!(source, &chemin, "source inattendue (pas la collection fabriquée) : {}", source.display());
            index.push(f.index);
        }
        index.sort_unstable();
        assert_eq!(index, vec![0, 1], "index des faces chargées depuis la collection");
    }

    /// Face fictive (pas de fichier réel : `push_face_info` ne lit pas la
    /// source), pour tester la logique de sélection sans dépendre des
    /// polices présentes sur la machine qui lance le test.
    fn face_fictive(famille: &str, chemin: &str, style: fontdb::Style) -> fontdb::FaceInfo {
        fontdb::FaceInfo {
            id: fontdb::ID::dummy(),
            source: fontdb::Source::File(chemin.into()),
            index: 0,
            families: vec![(famille.to_string(), fontdb::Language::English_UnitedStates)],
            post_script_name: String::new(),
            style,
            weight: fontdb::Weight::NORMAL,
            stretch: fontdb::Stretch::Normal,
            monospaced: true,
        }
    }

    #[test]
    fn sans_face_droite_traite_une_famille_seulement_italique_comme_absente() {
        let mut db = fontdb::Database::new();
        db.push_face_info(face_fictive("Famille Fictive", "/fictif-italique.ttf", fontdb::Style::Italic));
        // Présente dans la base, mais seulement en italique : absente au
        // sens de `depuis` (remplaçante et avertissement attendus).
        assert!(sans_face_droite(&db, "Famille Fictive"), "une famille seulement italique doit compter comme absente");
        db.push_face_info(face_fictive("Famille Fictive", "/fictif-droit.ttf", fontdb::Style::Normal));
        // Une face droite suffit maintenant à la considérer présente.
        assert!(!sans_face_droite(&db, "Famille Fictive"));
    }

    #[test]
    fn remplacante_deterministe_par_nom_puis_chemin() {
        // Logique de tri pure (sans fichiers réels : `push_face_info` ne
        // lit pas la source) : l'ordre d'insertion ne doit pas décider,
        // seulement le nom de famille puis le chemin.
        let mut db = fontdb::Database::new();
        let face = |famille: &str, chemin: &str| fontdb::FaceInfo {
            id: fontdb::ID::dummy(),
            source: fontdb::Source::File(chemin.into()),
            index: 0,
            families: vec![(famille.to_string(), fontdb::Language::English_UnitedStates)],
            post_script_name: String::new(),
            style: fontdb::Style::Normal,
            weight: fontdb::Weight::NORMAL,
            stretch: fontdb::Stretch::Normal,
            monospaced: true,
        };
        // Insérée en premier, mais pas la première alphabétiquement.
        db.push_face_info(face("Zorro Mono", "/z/a.ttf"));
        db.push_face_info(face("Alpha Mono", "/b/z.ttf"));
        db.push_face_info(face("Alpha Mono", "/a/a.ttf"));
        assert_eq!(repere_remplacante(db.faces()), Some("Alpha Mono".into()));
    }

    #[test]
    fn large_en_derniere_colonne_et_grille_plus_large_que_le_tampon() {
        let mut p = polices();
        let av = p.avance(1);
        // Tampon étroit (3 cases), dimensionné pour lui-même.
        let (w, h) = ((av * 3.0).ceil() as u32, HAUTEUR_DEFAUT);
        let mut px = vec![0u8; (w * h * 4) as usize];
        // Grille bien plus large (10 cases) que le tampon, avec un
        // caractère large (CJK, 2 colonnes) posé tout au bord droit, en
        // style inversé (fond d'accent) : exerce aussi le fond de case
        // par cellule, pas seulement le dessin du glyphe.
        let mut g = Grille::vide(10);
        g.ecrire(8, 10, "日", Style::INVERSE);
        p.peindre(&g, &Palette::default(), &mut px, w, h, 1); // ne doit pas paniquer
        // Chaque pixel du tampon reste bien formé (alpha opaque) : aucune
        // écriture n'a débordé de son emplacement calculé.
        for o in (0..px.len()).step_by(4) {
            assert_eq!(px[o + 3], 0xff, "pixel non peint à l'octet {o}");
        }
    }
}