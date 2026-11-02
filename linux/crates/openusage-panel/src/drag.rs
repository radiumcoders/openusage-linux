//! Reordering by dragging inside the panel: providers, accounts, and metrics. The panel is one
//! layer surface, so a drag is a pointer gesture within it (no compositor drag-and-drop, which a layer
//! surface can't start reliably). The landing math lives in `reorder`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{glib, graphene};

use crate::reorder::{self, Band};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Provider,
    Account,
    Metric,
}

pub struct Target {
    pub kind: Kind,
    pub scope: String,
    pub id: String,
    pub widget: gtk4::Widget,
}

type OnDrop = Rc<dyn Fn(Kind, String, String, String)>;

/// A list that accepts drops: its container, its rows, and what a drop does.
struct Registered {
    list: glib::WeakRef<gtk4::Widget>,
    targets: Rc<Vec<Target>>,
    on_drop: OnDrop,
}

thread_local! {
    static LISTS: RefCell<Vec<Registered>> = const { RefCell::new(Vec::new()) };
}

/// Pixels the pointer must travel before a press becomes a drag (a shorter press stays a click).
const THRESHOLD: f64 = 6.0;

/// Makes `widget` a drag handle for the row `id` of kind `kind` in `scope`.
pub fn bind_source(widget: &impl IsA<gtk4::Widget>, kind: Kind, scope: &str, id: &str) {
    let widget = widget.upcast_ref::<gtk4::Widget>().clone();
    widget.add_css_class("drag-handle");
    let gesture = gtk4::GestureDrag::new();
    gesture.set_button(1);
    let scope = scope.to_string();
    let id = id.to_string();
    let started = Rc::new(Cell::new(false));
    let origin = Rc::new(Cell::new((0.0, 0.0)));

    let begin_origin = origin.clone();
    let begin_started = started.clone();
    gesture.connect_drag_begin(move |_, x, y| {
        begin_origin.set((x, y));
        begin_started.set(false);
    });

    let update_widget = widget.clone();
    let update_started = started.clone();
    let update_origin = origin.clone();
    let (update_scope, update_id) = (scope.clone(), id.clone());
    gesture.connect_drag_update(move |gesture, dx, dy| {
        if !update_started.get() {
            if dx.hypot(dy) < THRESHOLD {
                return;
            }
            update_started.set(true);
            gesture.set_state(gtk4::EventSequenceState::Claimed);
            update_widget.add_css_class("dragging");
        }
        let (x, y) = update_origin.get();
        with_list(kind, &update_scope, &update_id, |list, group, _| {
            if let Some(y) = list_y(&update_widget, list, x + dx, y + dy) {
                apply_mark(group, &update_id, landing_of(list, group, y, &update_id));
            }
        });
    });

    let end_widget = widget.clone();
    gesture.connect_drag_end(move |_, dx, dy| {
        if !started.replace(false) {
            return;
        }
        end_widget.remove_css_class("dragging");
        let (x, y) = origin.get();
        let mut drop: Option<(OnDrop, String)> = None;
        with_list(kind, &scope, &id, |list, group, on_drop| {
            clear_marks(group);
            let landing = list_y(&end_widget, list, x + dx, y + dy).and_then(|y| landing_of(list, group, y, &id));
            if let Some(landing) = landing {
                drop = Some((on_drop.clone(), landing.target));
            }
        });
        // Run the drop after the registry borrow ends: it rebuilds the panel and re-registers lists.
        if let Some((on_drop, target)) = drop {
            on_drop(kind, scope.clone(), id.clone(), target);
        }
    });
    widget.add_controller(gesture);
}

/// Registers `list` as accepting drops for `targets`.
pub fn bind_rows(list: &impl IsA<gtk4::Widget>, targets: Vec<Target>, on_drop: OnDrop) {
    if targets.is_empty() {
        return;
    }
    let list = list.upcast_ref::<gtk4::Widget>();
    LISTS.with(|lists| {
        let mut lists = lists.borrow_mut();
        // Lists from earlier renders are gone once the panel rebuilds.
        lists.retain(|entry| entry.list.upgrade().is_some_and(|list| list.root().is_some()));
        lists.push(Registered { list: list.downgrade(), targets: Rc::new(targets), on_drop });
    });
}

/// Calls `body` with the live list that holds row `id` of `kind` in `scope`, and that row's group.
fn with_list(kind: Kind, scope: &str, id: &str, body: impl FnOnce(&gtk4::Widget, &[Target], &OnDrop)) {
    let found = LISTS.with(|lists| {
        lists.borrow().iter().rev().find_map(|entry| {
            let list = entry.list.upgrade().filter(|list| list.root().is_some())?;
            entry
                .targets
                .iter()
                .any(|row| row.kind == kind && row.scope == scope && row.id == id)
                .then(|| (list, entry.targets.clone(), entry.on_drop.clone()))
        })
    });
    if let Some((list, targets, on_drop)) = found {
        let group = matching(&targets, kind, scope);
        body(&list, &group, &on_drop);
    }
}

/// The pointer's y in `list` coordinates, from a point in `source` coordinates.
fn list_y(source: &gtk4::Widget, list: &gtk4::Widget, x: f64, y: f64) -> Option<f64> {
    source
        .compute_point(list, &graphene::Point::new(x as f32, y as f32))
        .map(|point| point.y() as f64)
}

fn matching(targets: &[Target], kind: Kind, scope: &str) -> Vec<Target> {
    targets
        .iter()
        .filter(|row| row.kind == kind && row.scope == scope)
        .map(|row| Target { kind: row.kind, scope: row.scope.clone(), id: row.id.clone(), widget: row.widget.clone() })
        .collect()
}

fn landing_of(
    list: &gtk4::Widget,
    group: &[Target],
    y: f64,
    dragged: &str,
) -> Option<reorder::Landing> {
    let bands = bands_of(list, group);
    reorder::landing(y, &bands, dragged)
}

fn bands_of(list: &gtk4::Widget, group: &[Target]) -> Vec<Band> {
    group
        .iter()
        .filter_map(|row| {
            let bounds = row.widget.compute_bounds(list)?;
            Some(Band {
                id: row.id.clone(),
                min_y: bounds.y() as f64,
                height: bounds.height() as f64,
            })
        })
        .collect()
}

fn apply_mark(group: &[Target], dragged: &str, landing: Option<reorder::Landing>) {
    for row in group {
        row.widget.remove_css_class("drop-before");
        row.widget.remove_css_class("drop-after");
        if row.id == dragged {
            continue;
        }
        if let Some(landing) = &landing {
            if landing.target == row.id {
                row.widget.add_css_class(if landing.after {
                    "drop-after"
                } else {
                    "drop-before"
                });
            }
        }
    }
}

fn clear_marks(targets: &[Target]) {
    for row in targets {
        row.widget.remove_css_class("drop-before");
        row.widget.remove_css_class("drop-after");
    }
}
