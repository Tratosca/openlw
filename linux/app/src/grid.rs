//! Input patch matrix: rows = sources (discovered, configured, manual), columns = channels of the
//! input device (duplex layout) or input devices (multi layout). Clicking an empty cell opens the
//! patch menu (stereo, left, right, L+R); a patch is drawn as one block over its columns, with
//! its tag, and clicking it releases it. The headphone button previews the source; the cross
//! removes a manual or unadvertised row. Same behavior as the macOS grid.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

use crate::i18n::{tr, trf};
use crate::meter::Meter;
use crate::models::DiscoveredSource;

/// Patch shown on a row: first column, column count, tag (“L”, “R”, “L+R”, “8” or none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridPatch {
    pub column: usize,
    pub span: usize,
    pub tag: String,
}

impl GridPatch {
    pub fn covers(&self, c: usize) -> bool {
        c >= self.column && c < self.column + self.span
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridRow {
    pub source: DiscoveredSource,
    pub patch: Option<GridPatch>,
    pub origin: String,
    pub removable: bool,
}

/// Column header group: title, then meters and state over a column range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridHeader {
    pub columns: Range<usize>,
    pub title: String,
}

/// Matrix geometry: one column per device channel (duplex) or per device (multi).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GridLayout {
    pub multi: bool,
    pub columns: usize,
    pub headers: Vec<GridHeader>,
}

/// Cell click: row, column, clicked cell.
pub type ToggleFn = dyn Fn(usize, usize, &gtk::Widget);

/// Grid actions, called with row and column indexes; `toggle` also gets the clicked cell, to
/// anchor the patch menu.
pub struct GridActions {
    pub toggle: Box<ToggleFn>,
    pub listen: Box<dyn Fn(usize)>,
    pub remove: Box<dyn Fn(usize)>,
}

/// Patch menu entry: title and action.
pub type MenuItem = (String, Rc<dyn Fn()>);

const CSS: &str = "
button.openlw-cell { padding: 0; min-width: 0; min-height: 26px; }
button.openlw-cell > label { font-size: smaller; font-weight: bold; }
popover.openlw-menu button label { font-weight: normal; }
";

pub struct InputGrid {
    grid: gtk::Grid,
    actions: Rc<GridActions>,
    rows: RefCell<Vec<GridRow>>,
    layout: RefCell<GridLayout>,
    listening: Cell<Option<usize>>,
    built: Cell<bool>,
    column_meters: RefCell<Vec<Meter>>,
    column_status: RefCell<Vec<gtk::Label>>,
    listen_meter: RefCell<Option<Meter>>,
}

impl InputGrid {
    pub fn new(actions: GridActions) -> Self {
        let grid = gtk::Grid::builder()
            .column_spacing(4)
            .row_spacing(4)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        Self {
            grid,
            actions: Rc::new(actions),
            rows: RefCell::default(),
            layout: RefCell::default(),
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

    /// Replaces rows, geometry and preview row; rebuilds only when something changed.
    pub fn update(&self, rows: Vec<GridRow>, layout: GridLayout, listening: Option<usize>) {
        let same = self.built.get()
            && listening == self.listening.get()
            && *self.rows.borrow() == rows
            && *self.layout.borrow() == layout;
        *self.rows.borrow_mut() = rows;
        *self.layout.borrow_mut() = layout;
        self.listening.set(listening);
        if !same {
            self.rebuild();
        }
    }

    /// Header meters (first two channels of each group) and states, then preview level.
    pub fn show_levels(
        &self,
        groups: &[Vec<Option<f64>>],
        status: &[&str],
        listen_level: Option<f64>,
    ) {
        for (m, levels) in self.column_meters.borrow().iter().zip(groups) {
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

    /// Patch menu under `anchor` (a cell of this grid).
    pub fn popup(&self, anchor: &gtk::Widget, items: Vec<MenuItem>) {
        let popover = gtk::Popover::new();
        popover.add_css_class("menu");
        popover.add_css_class("openlw-menu");
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for (title, action) in items {
            let label = gtk::Label::builder().label(&title).xalign(0.0).build();
            let item = gtk::Button::builder().child(&label).build();
            item.add_css_class("flat");
            let p = popover.downgrade();
            item.connect_clicked(move |_| {
                if let Some(p) = p.upgrade() {
                    p.popdown();
                }
                action();
            });
            list.append(&item);
        }
        popover.set_child(Some(&list));
        // Attached to the cell, like a menu button's popover.
        popover.set_parent(anchor);
        // The cell tooltip would cover the menu while it is open.
        anchor.set_has_tooltip(false);
        let p = popover.downgrade();
        // A grid rebuild while the menu is open destroys the cell: release the popover first.
        let destroyed = anchor.connect_destroy(move |_| {
            if let Some(p) = p.upgrade() {
                p.unparent();
            }
        });
        let cell = anchor.downgrade();
        let destroyed = RefCell::new(Some(destroyed));
        // Unparented once closed, outside its own signal handlers.
        popover.connect_closed(move |p| {
            if let Some(c) = cell.upgrade() {
                c.set_has_tooltip(true);
                if let Some(id) = destroyed.take() {
                    c.disconnect(id);
                }
            }
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        popover.popup();
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
        let layout = self.layout.borrow();
        let listening = self.listening.get();
        let width = if layout.multi { 88 } else { 40 };
        // Columns: preview, channel, name, origin, one per device channel or device, removal.
        let first_col = 4;
        let x = |c: usize| first_col + c as i32;
        for h in &layout.headers {
            let span = h.columns.len().max(1) as i32;
            let head = gtk::Box::new(gtk::Orientation::Vertical, 3);
            head.append(&caption(&h.title, true));
            let meter = Meter::new(2, span * width + (span - 1) * 4 - 16);
            meter.widget().set_halign(gtk::Align::Center);
            head.append(meter.widget());
            let status = caption(tr("free"), true);
            head.append(&status);
            self.column_meters.borrow_mut().push(meter);
            self.column_status.borrow_mut().push(status);
            self.grid.attach(&head, x(h.columns.start), 0, span, 1);
        }
        if rows.is_empty() {
            let empty = caption(
                tr("No sources discovered. Choose the interface, or enter a channel below."),
                false,
            );
            empty.set_wrap(true);
            empty.set_margin_top(8);
            self.grid.attach(&empty, 0, 1, x(layout.columns.max(1)), 1);
            return;
        }
        for (r, g) in rows.iter().enumerate() {
            let y = r as i32 + 1;
            let on_air = listening == Some(r);

            let listen = gtk::Button::from_icon_name("audio-headphones-symbolic");
            listen.set_valign(gtk::Align::Center);
            listen.set_tooltip_text(Some(if on_air {
                tr("Stop listening")
            } else {
                tr("Listen on the computer's audio output")
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
                "surround" => tr(" · surround"),
                "backfeed" => tr(" · backfeed"),
                _ => "",
            };
            let origin = caption(&format!("{}{kind}", g.origin), false);
            origin.set_max_width_chars(24);
            origin.set_size_request(150, -1);
            self.grid.attach(&origin, 3, y, 1, 1);

            let channel = g.source.channel;
            for c in 0..layout.columns {
                if g.patch.as_ref().is_some_and(|p| p.covers(c)) {
                    continue;
                }
                let cell = gtk::Button::new();
                cell.add_css_class("openlw-cell");
                cell.set_size_request(width, -1);
                cell.set_tooltip_text(Some(&if layout.multi {
                    trf(
                        "Send channel {channel} to In {n}",
                        &[("channel", &channel), ("n", &(c + 1))],
                    )
                } else {
                    trf(
                        "Send channel {channel} to input {n}",
                        &[("channel", &channel), ("n", &(c + 1))],
                    )
                }));
                let a = self.actions.clone();
                cell.connect_clicked(move |b| (a.toggle)(r, c, b.upcast_ref()));
                self.grid.attach(&cell, x(c), y, 1, 1);
            }
            // Patch: one block over its columns, with its tag (or a check mark for stereo).
            if let Some(p) = g.patch.as_ref().filter(|p| p.column < layout.columns) {
                let span = p.span.min(layout.columns - p.column).max(1);
                let cell = gtk::Button::new();
                cell.add_css_class("openlw-cell");
                cell.add_css_class("suggested-action");
                if p.tag.is_empty() {
                    cell.set_icon_name("object-select-symbolic");
                } else {
                    cell.set_label(&p.tag);
                }
                let (a, b) = (p.column + 1, p.column + span);
                cell.set_tooltip_text(Some(&if layout.multi {
                    trf("Release In {n}", &[("n", &a)])
                } else if a == b {
                    trf("Release input {n}", &[("n", &a)])
                } else {
                    trf("Release inputs {a}-{b}", &[("a", &a), ("b", &b)])
                }));
                let column = p.column;
                let actions = self.actions.clone();
                cell.connect_clicked(move |b| (actions.toggle)(r, column, b.upcast_ref()));
                self.grid.attach(&cell, x(p.column), y, span as i32, 1);
            }
            if g.removable {
                let remove = gtk::Button::from_icon_name("window-close-symbolic");
                remove.add_css_class("flat");
                remove.add_css_class("circular");
                remove.set_valign(gtk::Align::Center);
                remove.set_tooltip_text(Some(tr("Remove from grid")));
                let a = self.actions.clone();
                remove.connect_clicked(move |_| (a.remove)(r));
                self.grid.attach(&remove, x(layout.columns), y, 1, 1);
            }
        }
    }
}

fn caption(text: &str, center: bool) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("caption");
    l.add_css_class("dim-label");
    l.set_xalign(if center { 0.5 } else { 0.0 });
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    l
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_span() {
        let p = GridPatch {
            column: 2,
            span: 8,
            tag: "8".into(),
        };
        assert!(!p.covers(1) && p.covers(2) && p.covers(9) && !p.covers(10));
    }
}
