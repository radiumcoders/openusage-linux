//! GTK4 drag and drop for providers, accounts, and metrics. The landing math lives in `reorder`.

use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gdk, glib};

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

pub fn encode(kind: Kind, scope: &str, id: &str) -> String {
    format!("{}\t{scope}\t{id}", kind_name(kind))
}

pub fn decode(payload: &str) -> Option<(Kind, String, String)> {
    let mut parts = payload.splitn(3, '\t');
    let kind = kind_from(parts.next()?)?;
    let scope = parts.next()?.to_string();
    let id = parts.next()?.to_string();
    if id.is_empty() {
        return None;
    }
    Some((kind, scope, id))
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Provider => "provider",
        Kind::Account => "account",
        Kind::Metric => "metric",
    }
}

fn kind_from(name: &str) -> Option<Kind> {
    match name {
        "provider" => Some(Kind::Provider),
        "account" => Some(Kind::Account),
        "metric" => Some(Kind::Metric),
        _ => None,
    }
}

pub fn bind_source(widget: &impl IsA<gtk4::Widget>, kind: Kind, scope: &str, id: &str) {
    let widget = widget.upcast_ref::<gtk4::Widget>().clone();
    widget.add_css_class("drag-handle");
    let payload = encode(kind, scope, id);
    let source = gtk4::DragSource::new();
    source.set_actions(gdk::DragAction::MOVE);
    let icon_widget = widget.clone();
    source.connect_prepare(move |source, x, y| {
        let paintable = gtk4::WidgetPaintable::new(Some(&icon_widget));
        source.set_icon(Some(&paintable), x as i32, y as i32);
        Some(gdk::ContentProvider::for_value(&payload.to_value()))
    });
    widget.add_controller(source);
}

pub fn bind_rows(
    list: &impl IsA<gtk4::Widget>,
    targets: Vec<Target>,
    on_drop: Rc<dyn Fn(Kind, String, String, String)>,
) {
    if targets.is_empty() {
        return;
    }
    let list = list.upcast_ref::<gtk4::Widget>().clone();
    let drop = gtk4::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    drop.set_preload(true);
    let rows = Rc::new(targets);
    let motion_rows = rows.clone();
    let motion_list = list.clone();
    drop.connect_motion(move |target, _x, y| {
        let Some((kind, scope, dragged)) = payload_of(target) else {
            return gdk::DragAction::empty();
        };
        let group = matching(motion_rows.as_slice(), kind, &scope);
        apply_mark(
            &group,
            &dragged,
            landing_of(&motion_list, &group, y, &dragged),
        );
        if group.is_empty() {
            gdk::DragAction::empty()
        } else {
            gdk::DragAction::MOVE
        }
    });
    let leave_rows = rows.clone();
    drop.connect_leave(move |_| clear_marks(leave_rows.as_slice()));
    let drop_rows = rows.clone();
    let drop_list = list.clone();
    drop.connect_drop(move |target, _value, _x, y| {
        clear_marks(drop_rows.as_slice());
        let Some((kind, scope, dragged)) = payload_of(target) else {
            return false;
        };
        let group = matching(drop_rows.as_slice(), kind, &scope);
        let Some(landing) = landing_of(&drop_list, &group, y, &dragged) else {
            return false;
        };
        on_drop(kind, scope, dragged, landing.target);
        true
    });
    list.add_controller(drop);
}

fn payload_of(target: &gtk4::DropTarget) -> Option<(Kind, String, String)> {
    let value = target.value()?;
    let text = value.get::<String>().ok()?;
    decode(&text)
}

fn matching(targets: &[Target], kind: Kind, scope: &str) -> Vec<Target> {
    targets
        .iter()
        .filter(|row| row.kind == kind && row.scope == scope)
        .map(clone_target)
        .collect()
}

fn clone_target(row: &Target) -> Target {
    Target {
        kind: row.kind,
        scope: row.scope.clone(),
        id: row.id.clone(),
        widget: row.widget.clone(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_round_trip() {
        let payload = encode(Kind::Metric, "claude@abcd1234", "claude@abcd1234.session");
        assert_eq!(
            decode(&payload),
            Some((
                Kind::Metric,
                "claude@abcd1234".into(),
                "claude@abcd1234.session".into()
            ))
        );
        assert_eq!(decode("nope"), None);
        assert_eq!(decode("account\tlist\t"), None);
    }
}
