//! Input patch matrix: rows = sources (discovered, configured, manual), columns = channels of the
//! input device (duplex layout) or input devices (multi layout), grouped by pair or device.
//! A coupled group (stereo, the default) is one target: a click patches the source in stereo
//! (surround: 8 channels from the pair start), a click on a patched group releases it. The
//! link button of a group header uncouples it: each column then offers the left (top) and
//! right (bottom) sides of the source, both for L+R. Surround rows keep one target per group.
//! The source labels stay on the left while the cells scroll horizontally, so the window keeps
//! its width with 32 inputs. The headphone button previews the source; the cross removes a
//! manual or unadvertised row. Same behavior as the macOS grid (macos/app/Sources/Views.swift).

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;

use gtk::prelude::*;

use crate::i18n::{tr, trf};
use crate::meter::Meter;
use crate::models::DiscoveredSource;

/// Sides of a stereo source feeding an uncoupled column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GridSides {
    pub left: bool,
    pub right: bool,
}

/// Matrix row: source and its crosspoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridRow {
    pub source: DiscoveredSource,
    /// Coupled groups (or groups of a surround source) fed by this source: header index →
    /// tag (empty: plain stereo, shown as a check mark).
    pub groups: BTreeMap<usize, String>,
    /// Uncoupled columns fed by this source.
    pub sides: BTreeMap<usize, GridSides>,
    pub origin: String,
    pub removable: bool,
}

/// Column header group: a pair (duplex layout) or a device (multi layout); title, then
/// meters and state over a column range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridHeader {
    pub columns: Range<usize>,
    pub title: String,
    /// Pair number (duplex) or device number (multi), 1-based.
    pub number: u32,
    pub coupled: bool,
}

/// Matrix geometry: one column per device channel (duplex) or per device (multi).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GridLayout {
    pub multi: bool,
    pub columns: usize,
    pub headers: Vec<GridHeader>,
}

/// Grid actions, called with row, header and column indexes.
pub struct GridActions {
    /// Click on a coupled group, or on any group of a surround source (row, header).
    pub group: Box<dyn Fn(usize, usize)>,
    /// Click on one side of an uncoupled column (row, column, left side?).
    pub side: Box<dyn Fn(usize, usize, bool)>,
    /// Click on a header's link button (header).
    pub coupling: Box<dyn Fn(usize)>,
    pub listen: Box<dyn Fn(usize)>,
    pub remove: Box<dyn Fn(usize)>,
}

const CSS: &str = "
button.openlw-cell { padding: 0; min-width: 0; min-height: 26px; }
button.openlw-cell > label { font-size: smaller; font-weight: bold; }
button.openlw-side { padding: 0; min-width: 0; min-height: 11px; border-radius: 4px; }
button.openlw-side > label { font-size: x-small; font-weight: bold; }
button.openlw-link { padding: 0 2px; min-width: 16px; min-height: 16px; }
";

/// Tooltips of the sides of an uncoupled column: [duplex, multi] × [left, right].
const SIDE_TIPS: [[&str; 2]; 2] = [
    [
        "Left side (L) of channel {channel} to input {n}",
        "Right side (R) of channel {channel} to input {n}",
    ],
    [
        "Left side (L) of channel {channel} to In {n}",
        "Right side (R) of channel {channel} to In {n}",
    ],
];

/// Column width: device channels (two per pair) or devices.
const CHANNEL_WIDTH: i32 = 46;
const DEVICE_WIDTH: i32 = 88;
const SPACING: i32 = 4;

pub struct InputGrid {
    root: gtk::Box,
    /// Fixed part: preview button, channel, name, origin.
    labels: gtk::Grid,
    /// Scrolling part: header groups, cells, removal button.
    cells: gtk::Grid,
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
        let labels = gtk::Grid::builder()
            .column_spacing(SPACING)
            .row_spacing(SPACING)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(SPACING)
            .build();
        let cells = gtk::Grid::builder()
            .column_spacing(SPACING)
            .row_spacing(SPACING)
            .column_homogeneous(true)
            .halign(gtk::Align::Start)
            .margin_top(12)
            .margin_bottom(12)
            .margin_end(12)
            .build();
        // Horizontal scrolling only: the window does not grow with the input count.
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .hexpand(true)
            .child(&cells)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.append(&labels);
        root.append(&scroll);
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
            root,
            labels,
            cells,
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

    pub fn widget(&self) -> &gtk::Box {
        &self.root
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

    fn rebuild(&self) {
        self.built.set(true);
        for grid in [&self.labels, &self.cells] {
            while let Some(child) = grid.first_child() {
                grid.remove(&child);
            }
        }
        self.column_meters.borrow_mut().clear();
        self.column_status.borrow_mut().clear();
        *self.listen_meter.borrow_mut() = None;
        let rows = self.rows.borrow();
        let layout = self.layout.borrow();
        let listening = self.listening.get();
        let width = if layout.multi {
            DEVICE_WIDTH
        } else {
            CHANNEL_WIDTH
        };
        let span_width = |span: usize| {
            let span = span.max(1) as i32;
            span * width + (span - 1) * SPACING
        };

        // Header row: the labels part keeps a placeholder of the same height.
        let header_height = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
        let placeholder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        header_height.add_widget(&placeholder);
        self.labels.attach(&placeholder, 0, 0, 4, 1);
        for (i, h) in layout.headers.iter().enumerate() {
            let span = h.columns.len().max(1);
            let head = gtk::Box::new(gtk::Orientation::Vertical, 3);
            head.set_size_request(span_width(span), -1);
            let top = gtk::Box::new(gtk::Orientation::Horizontal, 2);
            let title = caption(&h.title, true);
            // Full title: the columns widen rather than truncate it.
            title.set_ellipsize(gtk::pango::EllipsizeMode::None);
            title.set_hexpand(true);
            top.append(&title);
            top.append(&self.link_button(i, h));
            head.append(&top);
            let meter = Meter::new(2, span_width(span) - 16);
            meter.widget().set_halign(gtk::Align::Center);
            head.append(meter.widget());
            let status = caption(tr("free"), true);
            head.append(&status);
            self.column_meters.borrow_mut().push(meter);
            self.column_status.borrow_mut().push(status);
            header_height.add_widget(&head);
            self.cells
                .attach(&head, h.columns.start as i32, 0, span as i32, 1);
        }
        if rows.is_empty() {
            let empty = caption(
                tr("No sources discovered. Choose the interface, or enter a channel below."),
                false,
            );
            empty.set_wrap(true);
            empty.set_max_width_chars(40);
            empty.set_margin_top(8);
            self.labels.attach(&empty, 0, 1, 4, 1);
            return;
        }
        for (r, g) in rows.iter().enumerate() {
            let y = r as i32 + 1;
            let on_air = listening == Some(r);
            // Same height for both parts of the row.
            let height = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
            let put = |grid: &gtk::Grid, w: &gtk::Widget, x: i32, span: i32| {
                let holder = centered(w);
                height.add_widget(&holder);
                grid.attach(&holder, x, y, span, 1);
            };

            let listen = gtk::Button::from_icon_name("audio-headphones-symbolic");
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
            put(&self.labels, listen.upcast_ref(), 0, 1);

            let ch = gtk::Label::new(Some(&g.source.channel.to_string()));
            ch.add_css_class("monospace");
            ch.set_xalign(1.0);
            ch.set_width_chars(5);
            put(&self.labels, ch.upcast_ref(), 1, 1);

            let name = gtk::Box::new(gtk::Orientation::Vertical, 2);
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
            put(&self.labels, name.upcast_ref(), 2, 1);

            let kind = match g.source.patch_kind() {
                "surround" => format!(" · {}", tr("surround")),
                "return" => format!(" · {}", tr("return")),
                _ => String::new(),
            };
            let origin = caption(&format!("{}{kind}", g.origin), false);
            origin.set_max_width_chars(24);
            origin.set_size_request(150, -1);
            put(&self.labels, origin.upcast_ref(), 3, 1);

            let surround = g.source.patch_kind() == "surround";
            let channel = g.source.channel;
            for (h, head) in layout.headers.iter().enumerate() {
                let span = head.columns.len().max(1);
                if head.coupled || surround {
                    let cell = self.group_cell(r, h, head, layout.multi, channel, g.groups.get(&h));
                    cell.set_size_request(span_width(span), -1);
                    put(
                        &self.cells,
                        cell.upcast_ref(),
                        head.columns.start as i32,
                        span as i32,
                    );
                    continue;
                }
                for c in head.columns.clone() {
                    let sides = g.sides.get(&c).copied().unwrap_or_default();
                    let cell = self.side_cell(r, c, layout.multi, channel, sides);
                    cell.set_size_request(width, -1);
                    put(&self.cells, cell.upcast_ref(), c as i32, 1);
                }
            }
            if g.removable {
                let remove = gtk::Button::from_icon_name("window-close-symbolic");
                remove.add_css_class("flat");
                remove.add_css_class("circular");
                remove.set_halign(gtk::Align::Start);
                remove.set_tooltip_text(Some(tr("Remove from grid")));
                let a = self.actions.clone();
                remove.connect_clicked(move |_| (a.remove)(r));
                put(&self.cells, remove.upcast_ref(), layout.columns as i32, 1);
            }
        }
    }

    /// Link button of a header group: coupled (accent) or uncoupled (dimmed).
    fn link_button(&self, h: usize, head: &GridHeader) -> gtk::Button {
        let link = gtk::Button::from_icon_name("insert-link-symbolic");
        link.add_css_class("flat");
        link.add_css_class("openlw-link");
        link.add_css_class(if head.coupled { "accent" } else { "dim-label" });
        link.set_tooltip_text(Some(if head.coupled {
            tr("Unlink")
        } else {
            tr("Link")
        }));
        link.set_valign(gtk::Align::Center);
        let a = self.actions.clone();
        link.connect_clicked(move |_| (a.coupling)(h));
        link
    }

    /// Coupled group (or a surround source's group): one target; patched, with its tag or a
    /// check mark for plain stereo.
    fn group_cell(
        &self,
        r: usize,
        h: usize,
        head: &GridHeader,
        multi: bool,
        channel: u16,
        tag: Option<&String>,
    ) -> gtk::Button {
        let cell = gtk::Button::new();
        cell.add_css_class("openlw-cell");
        let (a, b) = (head.columns.start + 1, head.columns.end);
        let tooltip = match (tag, multi) {
            (Some(_), true) => trf("Release In {n}", &[("n", &head.number)]),
            (Some(_), false) if a == b => trf("Release input {n}", &[("n", &a)]),
            (Some(_), false) => trf("Release inputs {a}-{b}", &[("a", &a), ("b", &b)]),
            (None, true) => trf(
                "Send channel {channel} to In {n}",
                &[("channel", &channel), ("n", &head.number)],
            ),
            (None, false) if a == b => trf(
                "Send channel {channel} to input {n}",
                &[("channel", &channel), ("n", &a)],
            ),
            (None, false) => trf(
                "Send channel {channel} to inputs {a}-{b}",
                &[("channel", &channel), ("a", &a), ("b", &b)],
            ),
        };
        cell.set_tooltip_text(Some(&tooltip));
        if let Some(tag) = tag {
            cell.add_css_class("suggested-action");
            if tag.is_empty() {
                cell.set_icon_name("object-select-symbolic");
            } else {
                cell.set_label(tag);
            }
        }
        let actions = self.actions.clone();
        cell.connect_clicked(move |_| (actions.group)(r, h));
        cell
    }

    /// Uncoupled column: left side on top, right side below, each a target.
    fn side_cell(
        &self,
        r: usize,
        c: usize,
        multi: bool,
        channel: u16,
        sides: GridSides,
    ) -> gtk::Box {
        let cell = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .homogeneous(true)
            .build();
        let n = c + 1;
        for (left, on) in [(true, sides.left), (false, sides.right)] {
            let half = gtk::Button::with_label(if left { tr("L") } else { tr("R") });
            half.add_css_class("openlw-side");
            if on {
                half.add_css_class("suggested-action");
            }
            let [duplex, devices] = SIDE_TIPS;
            let [left_tip, right_tip] = if multi { devices } else { duplex };
            let tooltip = if left { left_tip } else { right_tip };
            half.set_tooltip_text(Some(&trf(tooltip, &[("channel", &channel), ("n", &n)])));
            let a = self.actions.clone();
            half.connect_clicked(move |_| (a.side)(r, c, left));
            cell.append(&half);
        }
        cell
    }
}

/// Holder of a grid cell: takes the row height, the widget stays centered.
fn centered(w: &gtk::Widget) -> gtk::Box {
    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    w.set_valign(gtk::Align::Center);
    w.set_vexpand(true);
    // The row does not claim the window's extra height.
    holder.set_vexpand(false);
    holder.append(w);
    holder
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
    fn side_tooltips_are_translated() {
        let fr = crate::i18n::french_catalog();
        for t in SIDE_TIPS.iter().flatten() {
            assert!(fr.contains_key(*t), "missing in fr.po: {t:?}");
        }
    }
}
