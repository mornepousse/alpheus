//! Wayland : une surface layer-shell (couche Top, ancrée en haut, zone
//! exclusive) par écran, suivie à chaud ; un pointeur pour les clics et la
//! molette. Rien d'autre n'est demandé au compositeur. Redessin seulement
//! quand la grille d'un écran change.

use crate::actions::{self, Bouton, Molette};
use crate::etat::Barre;
use crate::grille::Grille;
use crate::peintre::{self, Polices};
use alpheus_commun::exec::Executor;
use alpheus_commun::theme::Palette;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::seat::pointer::{CursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec, ThemedPointer};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::{slot::SlotPool, Shm, ShmHandler};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry, delegate_seat, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
};
use std::io::{self, Write};
use std::sync::Arc;

/// `eprintln!` panique si l'autre bout de l'erreur standard est fermé
/// (EPIPE) : un démon résident ne doit jamais mourir pour si peu.
fn journal(m: &str) {
    let _ = writeln!(io::stderr(), "alpheus-demon : {m}");
}

/// Taille de départ du pool (`Etat::new`) : jamais redescendu en dessous,
/// pour ne pas recréer le pool sans cesse autour d'un écran minuscule ou
/// quand il est déjà à sa taille plancher.
const POOL_PLANCHER: usize = 64 * 1024;
/// Un pool qui dépasse ce qu'il faut aux surfaces restantes par ce facteur
/// (ou plus) est jugé nettement surdimensionné.
const POOL_FACTEUR_SURDIMENSIONNE: usize = 4;

/// Octets nécessaires au tampon d'une surface de `largeur` cases logiques,
/// à l'échelle `echelle`, pour une barre de `hauteur` px logiques (même
/// calcul que `dessiner` : ARGB8888, 4 octets par pixel physique).
fn taille_tampon(largeur: u32, hauteur: u32, echelle: i32) -> usize {
    let e = echelle.max(1) as usize;
    (largeur as usize * e) * (hauteur as usize * e) * 4
}

/// Faut-il recréer le pool (actuellement `actuelle` octets) pour les
/// surfaces restantes, dont les besoins (en octets) sont `besoins` ? Rend
/// la taille cible si oui, `None` sinon. `SlotPool::resize` (wl_shm) ne peut
/// qu'agrandir, jamais rétrécir : la seule façon de rendre la mémoire d'un
/// grand écran débranché est de recréer le pool, plus petit. On ne le fait
/// que quand c'est nettement utile (`POOL_FACTEUR_SURDIMENSIONNE`), pour ne
/// pas recréer à la moindre fluctuation de taille.
///
/// Le besoin est la SOMME de `besoins`, pas son maximum : le pool porte le
/// tampon de chaque surface À LA FOIS (une par écran), pas un seul tampon
/// réutilisé en rotation — un pool dimensionné sur la plus grande surface
/// serait trop petit dès qu'une deuxième a aussi besoin de place.
fn pool_cible(actuelle: usize, besoins: &[usize]) -> Option<usize> {
    if besoins.is_empty() {
        // Plus aucune surface : au minimum, revenir au plancher.
        return (actuelle > POOL_PLANCHER).then_some(POOL_PLANCHER);
    }
    let necessaire = besoins.iter().copied().sum::<usize>().max(POOL_PLANCHER);
    if actuelle > necessaire.saturating_mul(POOL_FACTEUR_SURDIMENSIONNE) {
        // Marge (double) pour ne pas re-recréer tout de suite après.
        Some(necessaire.saturating_mul(2))
    } else {
        None
    }
}

/// Texte affiché au journal pour une sortie : son nom Wayland s'il est
/// connu, sinon un repli lisible avec son identifiant protocole — une
/// sortie `wl_output` antérieure à la version 4 du protocole n'annonce pas
/// de nom, et `barre posée sur ` (vide) ne dit rien à qui lit le journal.
/// Fonction pure (le reste, `nom_journal`, en tire l'identifiant depuis le
/// `wl_output` réel) : testable sans connexion Wayland.
fn texte_journal(nom: &str, id: u32) -> String {
    if nom.is_empty() { format!("une sortie sans nom (id {id})") } else { nom.to_string() }
}

fn nom_journal(nom: &str, sortie: &wl_output::WlOutput) -> String {
    texte_journal(nom, sortie.id().protocol_id())
}

/// Faut-il reposer la barre sur `sortie` après que le compositeur a fermé
/// sa surface (`LayerShellHandler::closed`) ? Oui si cette sortie figure
/// encore parmi `connues` : certains compositeurs ferment la SURFACE sans
/// jamais détruire la SORTIE (`output_destroyed`), en s'attendant qu'on la
/// repose — contrairement à un vrai débranchement, où les deux arrivent.
/// Générique sur `T: PartialEq` plutôt que sur `wl_output::WlOutput`
/// directement : un `wl_output::WlOutput` ne s'instancie pas hors d'une
/// vraie connexion, cette forme reste donc testable sans Wayland.
fn faut_reposer<T: PartialEq>(sortie: &T, connues: impl Iterator<Item = T>) -> bool {
    connues.into_iter().any(|s| s == *sortie)
}

struct Surface {
    sortie: wl_output::WlOutput,
    nom: String,
    couche: LayerSurface,
    /// Largeur logique donnée par le compositeur (0 avant le premier configure).
    largeur: u32,
    echelle: i32,
    configuree: bool,
    /// Dernière grille dessinée : pas de redessin si rien n'a changé.
    derniere: Option<Grille>,
}

pub struct Etat {
    registre: RegistryState,
    sorties: OutputState,
    sieges: SeatState,
    shm: Shm,
    compositeur: CompositorState,
    couches: LayerShell,
    pool: SlotPool,
    surfaces: Vec<Surface>,
    pointeur: Option<ThemedPointer>,
    molette: Molette,
    pub barre: Barre,
    palette: Palette,
    polices: Polices,
    exec: Arc<dyn Executor>,
    pub fin: bool,
}

impl Etat {
    pub fn new(
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
        barre: Barre,
        palette: Palette,
        polices: Polices,
        exec: Arc<dyn Executor>,
    ) -> Result<Etat, String> {
        let compositeur = CompositorState::bind(globals, qh).map_err(|e| format!("wl_compositor : {e}"))?;
        let couches = LayerShell::bind(globals, qh).map_err(|e| format!("layer-shell indisponible : {e}"))?;
        let shm = Shm::bind(globals, qh).map_err(|e| format!("wl_shm : {e}"))?;
        let pool = SlotPool::new(64 * 1024, &shm).map_err(|e| format!("tampon partagé : {e}"))?;
        Ok(Etat {
            registre: RegistryState::new(globals),
            sorties: OutputState::new(globals, qh),
            sieges: SeatState::new(globals, qh),
            shm,
            compositeur,
            couches,
            pool,
            surfaces: Vec::new(),
            pointeur: None,
            molette: Molette::default(),
            barre,
            palette,
            polices,
            exec,
            fin: false,
        })
    }

    fn poser(&mut self, qh: &QueueHandle<Self>, sortie: wl_output::WlOutput) {
        let info = self.sorties.info(&sortie);
        let nom = info.as_ref().and_then(|i| i.name.clone()).unwrap_or_default();
        let echelle = info.map(|i| i.scale_factor).unwrap_or(1);
        let hauteur = self.barre.config.hauteur;
        let surface = self.compositeur.create_surface(qh);
        let couche = self.couches.create_layer_surface(qh, surface, Layer::Top, Some("alpheus-barre"), Some(&sortie));
        couche.set_anchor(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT);
        couche.set_size(0, hauteur);
        couche.set_exclusive_zone(hauteur as i32);
        couche.set_keyboard_interactivity(KeyboardInteractivity::None);
        couche.commit();
        journal(&format!("barre posée sur {}", nom_journal(&nom, &sortie)));
        self.surfaces.push(Surface { sortie, nom, couche, largeur: 0, echelle, configuree: false, derniere: None });
    }

    /// Rend la sortie de la surface retirée, pour que l'appelant puisse
    /// décider de reposer la barre dessus (voir `closed`) si elle existe
    /// encore.
    fn retirer(&mut self, i: usize) -> wl_output::WlOutput {
        let s = self.surfaces.remove(i); // la surface est détruite avec `s`
        journal(&format!("barre retirée de {}", nom_journal(&s.nom, &s.sortie)));
        self.retrecir_pool_si_besoin();
        s.sortie
    }

    /// Recrée le pool à une taille ajustée aux surfaces restantes, si
    /// nettement surdimensionné (voir `pool_cible`) : sans cela, la
    /// projection de pointe d'un grand écran débranché reste en mémoire
    /// pour toujours (« tourne sur une patate »).
    fn retrecir_pool_si_besoin(&mut self) {
        let hauteur = self.barre.config.hauteur;
        let besoins: Vec<usize> = self.surfaces.iter().map(|s| taille_tampon(s.largeur, hauteur, s.echelle)).collect();
        if let Some(taille) = pool_cible(self.pool.len(), &besoins) {
            match SlotPool::new(taille, &self.shm) {
                Ok(p) => self.pool = p,
                Err(e) => journal(&format!("pool {taille} : {e}")),
            }
        }
    }

    fn indice(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.surfaces.iter().position(|s| s.couche.wl_surface() == surface)
    }

    pub fn redessiner_tout(&mut self) {
        for i in 0..self.surfaces.len() {
            self.dessiner(i);
        }
    }

    fn dessiner(&mut self, i: usize) {
        let Etat { surfaces, polices, barre, palette, pool, .. } = self;
        let s = &mut surfaces[i];
        if !s.configuree || s.largeur == 0 {
            return;
        }
        let e = s.echelle.max(1);
        let (w, h) = (s.largeur * e as u32, barre.config.hauteur * e as u32);
        let grille = barre.grille(&s.nom, peintre::colonnes(w, polices.avance(e)));
        if s.derniere.as_ref() == Some(&grille) {
            return; // rien n'a changé sur cet écran
        }
        let (tampon, pixels) = match pool.create_buffer(w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888) {
            Ok(t) => t,
            Err(err) => {
                journal(&format!("tampon {w}×{h} : {err}"));
                return;
            }
        };
        polices.peindre(&grille, palette, pixels, w, h, e);
        let surface = s.couche.wl_surface();
        surface.set_buffer_scale(e);
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if tampon.attach_to(surface).is_err() {
            return;
        }
        s.couche.commit();
        s.derniere = Some(grille);
    }

    fn cliquer(&mut self, i: usize, x: f64, b: Bouton) {
        let e = self.surfaces[i].echelle;
        let avance = self.polices.avance(e);
        let s = &self.surfaces[i];
        let Some(grille) = &s.derniere else { return };
        if let Some(argv) = actions::clic(&self.barre, &s.nom, grille, x, e, avance, b) {
            actions::executer(self.exec.as_ref(), &argv);
        }
    }
}

impl CompositorHandler for Etat {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &wl_surface::WlSurface, f: i32) {
        if let Some(i) = self.indice(surface) {
            if self.surfaces[i].echelle != f {
                self.surfaces[i].echelle = f;
                self.surfaces[i].derniere = None;
                self.dessiner(i);
            }
        }
    }
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for Etat {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.sorties
    }

    fn new_output(&mut self, _: &Connection, qh: &QueueHandle<Self>, sortie: wl_output::WlOutput) {
        self.poser(qh, sortie);
    }

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, sortie: wl_output::WlOutput) {
        // Nom annoncé ou changé après coup : les bureaux affichés en dépendent.
        let nom = self.sorties.info(&sortie).and_then(|i| i.name);
        if let Some(i) = self.surfaces.iter().position(|s| s.sortie == sortie) {
            if let Some(n) = nom.filter(|n| *n != self.surfaces[i].nom) {
                self.surfaces[i].nom = n;
                self.surfaces[i].derniere = None;
                self.dessiner(i);
            }
        }
    }

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, sortie: wl_output::WlOutput) {
        if let Some(i) = self.surfaces.iter().position(|s| s.sortie == sortie) {
            self.retirer(i);
        }
    }
}

impl LayerShellHandler for Etat {
    fn closed(&mut self, _: &Connection, qh: &QueueHandle<Self>, couche: &LayerSurface) {
        // Le compositeur a fermé la surface. D'ordinaire (écran débranché),
        // `output_destroyed` suit : on oublie cette barre, les autres
        // continuent, jamais d'arrêt ici. Mais si la sortie existe encore
        // (voir `faut_reposer`), le compositeur attend qu'on la repose —
        // sans ça, cet écran resterait sans barre jusqu'au prochain
        // changement de sortie.
        if let Some(i) = self.indice(couche.wl_surface()) {
            let sortie = self.retirer(i);
            if faut_reposer(&sortie, self.sorties.outputs()) {
                self.poser(qh, sortie);
            }
        }
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, couche: &LayerSurface, c: LayerSurfaceConfigure, _: u32) {
        if let Some(i) = self.indice(couche.wl_surface()) {
            let s = &mut self.surfaces[i];
            if s.largeur != c.new_size.0 {
                s.derniere = None;
            }
            s.largeur = c.new_size.0;
            s.configuree = true;
            self.dessiner(i);
        }
    }
}

impl SeatHandler for Etat {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.sieges
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(&mut self, _: &Connection, qh: &QueueHandle<Self>, siege: wl_seat::WlSeat, c: Capability) {
        if c == Capability::Pointer && self.pointeur.is_none() {
            let surface = self.compositeur.create_surface(qh);
            match self.sieges.get_pointer_with_theme(qh, &siege, self.shm.wl_shm(), surface, ThemeSpec::default()) {
                Ok(p) => self.pointeur = Some(p),
                Err(e) => journal(&format!("pointeur indisponible : {e}")),
            }
        }
    }

    fn remove_capability(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat, c: Capability) {
        if c == Capability::Pointer {
            self.pointeur = None;
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for Etat {
    fn pointer_frame(&mut self, conn: &Connection, _: &QueueHandle<Self>, _: &wl_pointer::WlPointer, evenements: &[PointerEvent]) {
        for ev in evenements {
            let Some(i) = self.indice(&ev.surface) else { continue };
            match &ev.kind {
                PointerEventKind::Enter { .. } => {
                    if let Some(p) = &self.pointeur {
                        let _ = p.set_cursor(conn, CursorIcon::Default);
                    }
                }
                PointerEventKind::Press { button, .. } => {
                    if let Some(b) = actions::bouton(*button) {
                        self.cliquer(i, ev.position.0, b);
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    if let Some(b) = self.molette.ajouter(vertical.absolute, vertical.discrete, vertical.value120) {
                        self.cliquer(i, ev.position.0, b);
                    }
                    // Fin de geste (molette crantée relâchée, pavé tactile
                    // qui se lève) : rien à reporter sur le prochain geste.
                    if vertical.stop {
                        self.molette.arreter();
                    }
                }
                PointerEventKind::Leave { .. } => {
                    self.molette.arreter();
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for Etat {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Etat {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registre
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Etat);
delegate_output!(Etat);
delegate_shm!(Etat);
delegate_layer!(Etat);
delegate_seat!(Etat);
delegate_pointer!(Etat);
delegate_registry!(Etat);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taille_du_tampon_tient_compte_de_l_echelle() {
        assert_eq!(taille_tampon(100, 22, 1), 100 * 22 * 4);
        assert_eq!(taille_tampon(100, 22, 2), 200 * 44 * 4);
        assert_eq!(taille_tampon(100, 22, 0), 100 * 22 * 4, "échelle 0 traitée comme 1");
    }

    #[test]
    fn pool_deja_au_plancher_sans_surface_rien_a_faire() {
        assert_eq!(pool_cible(POOL_PLANCHER, &[]), None);
        assert_eq!(pool_cible(POOL_PLANCHER / 2, &[]), None, "déjà sous le plancher : pas de recréation inutile");
    }

    #[test]
    fn plus_aucune_surface_revient_au_plancher() {
        // Un grand écran (3840 px logiques, échelle 2) a fait grossir le
        // pool bien au-delà du plancher ; une fois débranché, plus aucune
        // surface ne reste : le pool doit revenir au plancher, pas rester à
        // sa taille de pointe pour toujours.
        assert_eq!(pool_cible(4 * 1024 * 1024, &[]), Some(POOL_PLANCHER));
    }

    #[test]
    fn surdimensionnement_leger_laisse_le_pool_tel_quel() {
        // Un pool qui vient de doubler pour accueillir un écran ne doit pas
        // être recréé à la moindre réduction (churn) : seul un écart net
        // (POOL_FACTEUR_SURDIMENSIONNE) déclenche une recréation.
        let besoin = 676_480; // ~3840×44×4 (écran large, échelle 2)
        assert_eq!(pool_cible(besoin * 2, &[besoin]), None);
        assert_eq!(pool_cible(besoin * POOL_FACTEUR_SURDIMENSIONNE, &[besoin]), None, "pile au facteur : pas encore net");
    }

    #[test]
    fn surdimensionnement_net_recree_plus_petit() {
        let besoin = 676_480;
        let cible = pool_cible(besoin * POOL_FACTEUR_SURDIMENSIONNE + 1, &[besoin]);
        assert_eq!(cible, Some(besoin * 2), "{cible:?}");
        assert!(cible.unwrap() < besoin * POOL_FACTEUR_SURDIMENSIONNE, "nettement plus petit qu'avant");
    }

    #[test]
    fn plusieurs_surfaces_la_somme_decide_pas_la_plus_grande() {
        // Le pool contient le tampon de CHAQUE surface à la fois (une par
        // écran) : il lui faut la place de leur SOMME, pas seulement du
        // plus grand — sinon, trois écrans aussi grands les uns que les
        // autres ne tiendraient jamais tous leurs tampons en même temps
        // dans un pool dimensionné sur un seul.
        let (petit, grand) = (10_000, 676_480);
        let somme = petit + grand;
        assert_eq!(pool_cible(somme * POOL_FACTEUR_SURDIMENSIONNE, &[petit, grand]), None, "pile au facteur de la somme : pas encore net");
        let cible = pool_cible(somme * POOL_FACTEUR_SURDIMENSIONNE + 1, &[petit, grand]);
        assert_eq!(cible, Some(somme * 2));
        assert!(cible.unwrap() >= somme, "le pool recréé doit au moins tenir tous les tampons à la fois");
    }

    #[test]
    fn texte_journal_repli_lisible_sur_sortie_sans_nom() {
        assert_eq!(texte_journal("HDMI-A-1", 7), "HDMI-A-1");
        assert_eq!(texte_journal("", 7), "une sortie sans nom (id 7)");
    }

    #[test]
    fn faut_reposer_si_la_sortie_est_encore_connue() {
        // wl_output::WlOutput ne s'instancie pas hors d'une vraie connexion
        // Wayland (voir le commentaire de `faut_reposer`) : des entiers
        // tiennent lieu d'identifiants de sortie dans ce test, la fonction
        // étant générique sur `PartialEq`.
        assert!(faut_reposer(&2, [1, 2, 3].into_iter()), "encore dans la liste : à reposer");
        assert!(!faut_reposer(&5, [1, 2, 3].into_iter()), "plus dans la liste : vraiment débranchée");
        assert!(!faut_reposer(&1, std::iter::empty::<i32>()), "plus aucune sortie connue");
    }
}
