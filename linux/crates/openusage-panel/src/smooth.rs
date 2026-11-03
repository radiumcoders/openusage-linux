//! Smooth mouse-wheel scrolling. GTK moves a scrolled window by a whole step per wheel notch; here a
//! notch sets a target and the view eases toward it over a few frames, and further notches extend
//! the target. Touchpads already scroll pixel by pixel (with kinetic flicks), so GTK keeps those.

use std::cell::Cell;
use std::rc::Rc;

use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;

/// How quickly the view closes on its target: about 63% of the way every `EASE_SECONDS`.
const EASE_SECONDS: f64 = 0.055;
/// A rebuild nobody asked for waits until scrolling has been still this long.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(700);

thread_local! {
    static LAST_SCROLL: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
}

/// How long until scrolling counts as settled, or none when it already is.
pub fn settling() -> Option<std::time::Duration> {
    let last = LAST_SCROLL.with(Cell::get)?;
    SETTLE.checked_sub(last.elapsed()).filter(|left| !left.is_zero())
}

fn touched() {
    LAST_SCROLL.with(|last| last.set(Some(std::time::Instant::now())));
}

pub fn install(scroll: &gtk4::ScrolledWindow) {
    let target: Rc<Cell<Option<f64>>> = Rc::new(Cell::new(None));
    let running = Rc::new(Cell::new(false));
    let controller = gtk4::EventControllerScroll::new(gtk4::EventControllerScrollFlags::VERTICAL);
    controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let weak = scroll.downgrade();
    controller.connect_scroll(move |controller, _dx, dy| {
        touched();
        if controller.unit() == gdk::ScrollUnit::Surface {
            // A touchpad: already smooth. Drop any wheel animation so the two don't fight.
            target.set(None);
            return glib::Propagation::Proceed;
        }
        let Some(scroll) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let adjustment = scroll.vadjustment();
        let max = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        if max <= adjustment.lower() {
            return glib::Propagation::Proceed;
        }
        // GTK's own wheel step: the page size to the 2/3 power.
        let step = adjustment.page_size().powf(2.0 / 3.0);
        let from = target.get().unwrap_or_else(|| adjustment.value());
        target.set(Some((from + dy * step).clamp(adjustment.lower(), max)));
        if !running.replace(true) {
            animate(&scroll, target.clone(), running.clone());
        }
        glib::Propagation::Stop
    });
    scroll.add_controller(controller);
}

fn animate(scroll: &gtk4::ScrolledWindow, target: Rc<Cell<Option<f64>>>, running: Rc<Cell<bool>>) {
    let last = Cell::new(None::<i64>);
    scroll.add_tick_callback(move |scroll, clock| {
        let Some(goal) = target.get() else {
            running.set(false);
            return glib::ControlFlow::Break;
        };
        let now = clock.frame_time();
        let elapsed = last
            .replace(Some(now))
            .map_or(1.0 / 60.0, |before| (now - before) as f64 / 1_000_000.0);
        let adjustment = scroll.vadjustment();
        let current = adjustment.value();
        let left = goal - current;
        if left.abs() < 0.5 {
            adjustment.set_value(goal);
            target.set(None);
            running.set(false);
            return glib::ControlFlow::Break;
        }
        touched();
        let share = 1.0 - (-elapsed.clamp(0.0, 0.1) / EASE_SECONDS).exp();
        adjustment.set_value(current + left * share);
        glib::ControlFlow::Continue
    });
}
