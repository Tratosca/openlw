//! Input patch matrix: rows = sources (discovered, configured, manual), columns = device input
//! pairs. A cell sends the source to that pair (click again to release); the headphone button
//! previews the source; the cross removes a manual or unadvertised row. Same behavior as the
//! macOS and Windows grids.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

use crate::meter::Meter;
use crate::models::DiscoveredSource;

const COLUMN_WIDTH: i32 = 96;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridRow {
    pub source: DiscoveredSource,
    pub patched_column: Option<usize>,
    pub origin: String,
    pub removable: bool,
}

/// Grid actions, called with row and column indexes.
pub struct GridActions {
    pub toggle: Box<dyn Fn(usize, usize)>,
    pub listen: Box<dyn Fn(usize)>,
    pub remove: Box<dyn Fn(usize)>,
}

pub struct InputGrid {
    grid: gtk::Grid,
    actions: Rc<GridActions>,
    rows: RefCell<Vec<GridRow>>,
    pairs: RefCell<Vec<Vec<u32>>>,
    listening: Cell<Option<usize>>,
    built: Cell<bool>,
    column_meters: RefCell<Vec<Meter>>,
    column_status: RefCell<Vec<gtk::Label>>,
    listen_meter: RefCell<Option<Meter>>,
}

impl InputGrid {
    pub fn new(actions: GridActions) -> Self {
        let grid = gtk::Grid::builder()
            .column_spacing(6)
            .row_spacing(4)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        Self {
            grid,
            actions: Rc::new(actions),
            rows: RefCell::default(),
            pairs: RefCell::default(),
            listening: Cell::new(None),
            built: Cell::new(false),
            column_meters: RefCell::default(),
            column_status: RefCell::default(),
            listen_meter: RefCell::default(),
        }
    }

    pub fn widget(&self) -> &gtk::Grid {
        &self.grid
    }

    /// Replaces rows, pairs and preview row; rebuilds only when something changed.
    pub fn update(&self, rows: Vec<GridRow>, pairs: Vec<Vec<u32>>, listening: Option<usize>) {
        let same = self.built.get()
            && listening == self.listening.get()
            && *self.rows.borrow() == rows
            && *self.pairs.borrow() == pairs;
        *self.rows.borrow_mut() = rows;
        *self.pairs.borrow_mut() = pairs;
        self.listening.set(listening);
        if !same {
            self.rebuild();
        }
    }

    pub fn show_levels(
        &self,
        columns: &[Vec<Option<f64>>],
        status: &[&str],
        listen_level: Option<f64>,
    ) {
        for (m, levels) in self.column_meters.borrow().iter().zip(columns) {
            m.show(levels);
        }
        for (l, text) in self.column_status.borrow().iter().zip(status) {
            if l.label() != *text {
                l.set_label(text);
            }
        }
        if let Some(m) = self.listen_meter.borrow().as_ref() {
            m.show(&[listen_level, listen_level]);
        }
    }

    fn rebuild(&self) {
        self.built.set(true);
        while let Some(child) = self.grid.first_child() {
            self.grid.remove(&child);
        }
        self.column_meters.borrow_mut().clear();
        self.column_status.borrow_mut().clear();
        *self.listen_meter.borrow_mut() = None;
        let rows = self.rows.borrow();
        let pairs = self.pairs.borrow();
        let listening = self.listening.get();
        // Columns: preview, channel, name, origin, one per pair, removal.
        let first_pair = 4;
        for (c, pair) in pairs.iter().enumerate() {
            let head = gtk::Box::new(gtk::Orientation::Vertical, 3);
            head.set_size_request(COLUMN_WIDTH, -1);
            head.append(&caption(&pair_label("Entrées", pair), true));
            let meter = Meter::new(2, COLUMN_WIDTH - 16);
            meter.widget().set_halign(gtk::Align::Center);
            head.append(meter.widget());
            let status = caption("libre", true);
            head.append(&status);
            self.column_meters.borrow_mut().push(meter);
            self.column_status.borrow_mut().push(status);
            self.grid.attach(&head, first_pair + c as i32, 0, 1, 1);
        }
        if rows.is_empty() {
            let empty = caption(
                "Aucune source découverte. Choisissez l'interface, ou saisissez un canal ci-dessous.",
                false,
            );
            empty.set_wrap(true);
            empty.set_margin_top(8);
            self.grid
                .attach(&empty, 0, 1, first_pair + pairs.len().max(1) as i32, 1);
            return;
        }
        for (r, g) in rows.iter().enumerate() {
            let y = r as i32 + 1;
            let on_air = listening == Some(r);

            let listen = gtk::Button::from_icon_name("audio-headphones-symbolic");
            listen.set_valign(gtk::Align::Center);
            listen.set_tooltip_text(Some(if on_air {
                "Arrêter l'écoute"
            } else {
                "Écouter sur la sortie audio de l'ordinateur"
            }));
            if on_air {
                listen.add_css_class("suggested-action");
            } else {
                listen.add_css_class("flat");
            }
            let a = self.actions.clone();
            listen.connect_clicked(move |_| (a.listen)(r));
            self.grid.attach(&listen, 0, y, 1, 1);

            let ch = gtk::Label::new(Some(&g.source.channel.to_string()));
            ch.add_css_class("monospace");
            ch.set_xalign(1.0);
            ch.set_width_chars(5);
            self.grid.attach(&ch, 1, y, 1, 1);

            let name = gtk::Box::new(gtk::Orientation::Vertical, 2);
            name.set_valign(gtk::Align::Center);
            name.set_size_request(150, -1);
            let label = gtk::Label::new(Some(if g.source.name.is_empty() {
                "—"
            } else {
                &g.source.name
            }));
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(22);
            name.append(&label);
            if on_air {
                let m = Meter::new(2, 120);
                name.append(m.widget());
                *self.listen_meter.borrow_mut() = Some(m);
            }
            self.grid.attach(&name, 2, y, 1, 1);

            let kind = match g.source.patch_kind() {
                "surround" => " · surround",
                "backfeed" => " · retour",
                _ => "",
            };
            let origin = caption(&format!("{}{kind}", g.origin), false);
            origin.set_max_width_chars(24);
            origin.set_size_request(150, -1);
            self.grid.attach(&origin, 3, y, 1, 1);

            for (c, pair) in pairs.iter().enumerate() {
                let on = g.patched_column == Some(c);
                let cell = gtk::ToggleButton::new();
                cell.set_active(on);
                if on {
                    cell.set_icon_name("object-select-symbolic");
                }
                cell.set_tooltip_text(Some(&if on {
                    pair_label("Libérer les entrées", pair)
                } else {
                    pair_label(
                        &format!("Envoyer le canal {} sur les entrées", g.source.channel),
                        pair,
                    )
                }));
                let a = self.actions.clone();
                cell.connect_clicked(move |_| (a.toggle)(r, c));
                self.grid.attach(&cell, first_pair + c as i32, y, 1, 1);
            }
            if g.removable {
                let remove = gtk::Button::from_icon_name("window-close-symbolic");
                remove.add_css_class("flat");
                remove.add_css_class("circular");
                remove.set_valign(gtk::Align::Center);
                remove.set_tooltip_text(Some("Retirer de la grille"));
                let a = self.actions.clone();
                remove.connect_clicked(move |_| (a.remove)(r));
                self.grid
                    .attach(&remove, first_pair + pairs.len() as i32, y, 1, 1);
            }
        }
    }
}

/// “Entrées 1-2” (first and last channel of a pair or a surround block).
pub fn pair_label(prefix: &str, pair: &[u32]) -> String {
    format!(
        "{prefix} {}-{}",
        pair.first().copied().unwrap_or(0),
        pair.last().copied().unwrap_or(0)
    )
}

fn caption(text: &str, center: bool) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("caption");
    l.add_css_class("dim-label");
    l.set_xalign(if center { 0.5 } else { 0.0 });
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    l
}
