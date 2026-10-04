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
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
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
use std::sync::Arc;

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
        eprintln!("alpheus-demon : barre posée sur {nom}");
        self.surfaces.push(Surface { sortie, nom, couche, largeur: 0, echelle, configuree: false, derniere: None });
    }

    fn retirer(&mut self, i: usize) {
        let s = self.surfaces.remove(i); // la surface est détruite avec `s`
        eprintln!("alpheus-demon : barre retirée de {}", s.nom);
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
                eprintln!("alpheus-demon : tampon {w}×{h} : {err}");
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
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, couche: &LayerSurface) {
        // Le compositeur a retiré la surface (écran débranché) : on oublie
        // cette barre, les autres continuent. Jamais d'arrêt ici.
        if let Some(i) = self.indice(couche.wl_surface()) {
            self.retirer(i);
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
                Err(e) => eprintln!("alpheus-demon : pointeur indisponible : {e}"),
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
