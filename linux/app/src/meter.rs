//! Horizontal peak meter, one bar per channel: −60 to 0 dBFS, green / amber (−18) / red (−6),
//! like the macOS and Windows apps (GNOME palette colors).

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

const BAR: f64 = 4.0;
const GAP: f64 = 2.0;

#[derive(Clone)]
pub struct Meter {
    area: gtk::DrawingArea,
    levels: Rc<RefCell<Vec<Option<f64>>>>,
}

impl Meter {
    pub fn new(channels: usize, width: i32) -> Self {
        let levels = Rc::new(RefCell::new(vec![None; channels]));
        let area = gtk::DrawingArea::builder()
            .content_width(width)
            .content_height((channels as f64 * (BAR + GAP) - GAP).ceil() as i32)
            .valign(gtk::Align::Center)
            .build();
        let l = levels.clone();
        area.set_draw_func(move |area, cr, w, _h| {
            let fg = area.color();
            let w = f64::from(w);
            for (i, &db) in l.borrow().iter().enumerate() {
                let y = i as f64 * (BAR + GAP);
                cr.set_source_rgba(
                    f64::from(fg.red()),
                    f64::from(fg.green()),
                    f64::from(fg.blue()),
                    0.15,
                );
                rounded(cr, 0.0, y, w, BAR);
                let _ = cr.fill();
                let frac = db.map_or(0.0, |d: f64| ((d + 60.0) / 60.0).clamp(0.0, 1.0));
                if frac > 0.0 {
                    let (r, g, b) = match db {
                        Some(d) if d >= -6.0 => (0xe0, 0x1b, 0x24),
                        Some(d) if d >= -18.0 => (0xe5, 0xa5, 0x0a),
                        _ => (0x2e, 0xc2, 0x7e),
                    };
                    cr.set_source_rgb(
                        f64::from(r) / 255.0,
                        f64::from(g) / 255.0,
                        f64::from(b) / 255.0,
                    );
                    rounded(cr, 0.0, y, (frac * w).max(BAR), BAR);
                    let _ = cr.fill();
                }
            }
        });
        Self { area, levels }
    }

    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    /// Peak per channel in dBFS (`None`: silence).
    pub fn show(&self, levels: &[Option<f64>]) {
        let mut l = self.levels.borrow_mut();
        let next: Vec<Option<f64>> = (0..l.len())
            .map(|i| levels.get(i).copied().flatten())
            .collect();
        if *l != next {
            *l = next;
            drop(l);
            self.area.queue_draw();
        }
    }
}

fn rounded(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64) {
    let r = h / 2.0;
    cr.new_sub_path();
    cr.arc(
        x + w - r,
        y + r,
        r,
        -std::f64::consts::FRAC_PI_2,
        std::f64::consts::FRAC_PI_2,
    );
    cr.arc(
        x + r,
        y + r,
        r,
        std::f64::consts::FRAC_PI_2,
        3.0 * std::f64::consts::FRAC_PI_2,
    );
    cr.close_path();
}
