//! Horizontal peak meter, one bar per channel: −60 to 0 dBFS, green / amber (−18) / red (−6),
//! like the macOS and Windows apps (GNOME palette colors). Ballistics: instant rise, then a fall
//! of 20 dB in 1.7 s (IEC 60268-10 type I return), animated on the frame clock.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

const BAR: f64 = 4.0;
const GAP: f64 = 2.0;
const FLOOR: f64 = -60.0;
const FALL_DB_PER_S: f64 = 20.0 / 1.7;

/// Shown levels, latest peaks, and frame time (µs) of the last animation step.
#[derive(Default)]
struct Bars {
    shown: Vec<Option<f64>>,
    target: Vec<Option<f64>>,
    frame_us: Option<i64>,
}

impl Bars {
    /// Fall by `dt` seconds toward the targets; true while a bar is still above its target.
    fn step(&mut self, dt: f64) -> bool {
        for (s, &t) in self.shown.iter_mut().zip(&self.target) {
            let fallen = s.map(|v| v - FALL_DB_PER_S * dt);
            *s = match (t, fallen) {
                (Some(t), Some(f)) => Some(t.max(f)),
                (t, f) => t.or(f),
            }
            .filter(|&v| v > FLOOR);
        }
        self.shown != self.target
    }
}

#[derive(Clone)]
pub struct Meter {
    area: gtk::DrawingArea,
    bars: Rc<RefCell<Bars>>,
}

impl Meter {
    pub fn new(channels: usize, width: i32) -> Self {
        let bars = Rc::new(RefCell::new(Bars {
            shown: vec![None; channels],
            target: vec![None; channels],
            frame_us: None,
        }));
        let area = gtk::DrawingArea::builder()
            .content_width(width)
            .content_height((channels as f64 * (BAR + GAP) - GAP).ceil() as i32)
            .valign(gtk::Align::Center)
            .build();
        let l = bars.clone();
        area.set_draw_func(move |area, cr, w, _h| {
            let fg = area.color();
            let w = f64::from(w);
            for (i, &db) in l.borrow().shown.iter().enumerate() {
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
        Self { area, bars }
    }

    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    /// Peak per channel in dBFS (`None`: silence): shown at once if higher, otherwise reached
    /// by the fall.
    pub fn show(&self, levels: &[Option<f64>]) {
        let mut b = self.bars.borrow_mut();
        b.target = (0..b.shown.len())
            .map(|i| levels.get(i).copied().flatten())
            .collect();
        let before = b.shown.clone();
        let falling = b.step(0.0);
        let idle = b.frame_us.is_none();
        if falling && idle {
            b.frame_us = Some(0);
            let bars = self.bars.clone();
            self.area.add_tick_callback(move |area, clock| {
                let mut b = bars.borrow_mut();
                let now = clock.frame_time();
                let dt = match b.frame_us {
                    Some(t) if t > 0 => ((now - t) as f64 / 1e6).min(0.1),
                    _ => 0.0,
                };
                b.frame_us = Some(now);
                let falling = b.step(dt);
                area.queue_draw();
                if falling {
                    gtk::glib::ControlFlow::Continue
                } else {
                    b.frame_us = None;
                    gtk::glib::ControlFlow::Break
                }
            });
        }
        if b.shown != before {
            drop(b);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instant_rise_then_fall_to_the_floor() {
        let mut b = Bars {
            shown: vec![None],
            target: vec![Some(-12.0)],
            frame_us: None,
        };
        assert!(!b.step(0.0));
        assert_eq!(b.shown, [Some(-12.0)]);
        b.target = vec![None];
        assert!(b.step(1.7));
        assert!(b
            .shown
            .first()
            .copied()
            .flatten()
            .is_some_and(|v| (v + 32.0).abs() < 1e-9));
        assert!(!b.step(10.0));
        assert_eq!(b.shown, [None]);
    }
}
