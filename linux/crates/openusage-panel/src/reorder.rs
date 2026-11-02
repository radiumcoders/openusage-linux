//! Drop-target math from `ReorderGeometry.swift`, and the session undo stack.
//! A drop lands once the pointer has crossed 20% into the target row: the bottom edge when
//! moving down, the top edge when moving up.

use openusage_core::layout::LayoutState;

/// One row's band in the list's coordinate space.
#[derive(Debug, Clone, PartialEq)]
pub struct Band {
    pub id: String,
    pub min_y: f64,
    pub height: f64,
}

/// Where a drop would insert, and whether the indicator sits under the target (moving down).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landing {
    pub target: String,
    pub after: bool,
}

const CROSSING: f64 = 0.20;

pub fn landing(y: f64, bands: &[Band], dragged: &str) -> Option<Landing> {
    let target = reorder_target(y, bands, dragged)?;
    let from = bands.iter().position(|band| band.id == dragged)?;
    let to = bands.iter().position(|band| band.id == target)?;
    Some(Landing {
        target,
        after: to > from,
    })
}

pub fn reorder_target(y: f64, bands: &[Band], dragged: &str) -> Option<String> {
    let from = bands.iter().position(|band| band.id == dragged)?;
    for (to, band) in bands.iter().enumerate() {
        if band.id == dragged {
            continue;
        }
        let top = band.min_y - 2.0;
        let bottom = band.min_y + band.height + 2.0;
        if y < top || y > bottom {
            continue;
        }
        if to > from {
            if y >= band.min_y + band.height * CROSSING {
                return Some(band.id.clone());
            }
            return None;
        }
        if y <= band.min_y + band.height - band.height * CROSSING {
            return Some(band.id.clone());
        }
        return None;
    }
    None
}

/// Snapshots taken before a layout change. Undo pops the previous one. Session only.
#[derive(Debug, Default, Clone)]
pub struct UndoStack {
    steps: Vec<LayoutState>,
}

impl UndoStack {
    pub fn push(&mut self, before: LayoutState) {
        self.steps.push(before);
        if self.steps.len() > 50 {
            self.steps.remove(0);
        }
    }

    pub fn undo(&mut self) -> Option<LayoutState> {
        self.steps.pop()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bands() -> Vec<Band> {
        vec![
            Band {
                id: "a".into(),
                min_y: 0.0,
                height: 100.0,
            },
            Band {
                id: "b".into(),
                min_y: 100.0,
                height: 100.0,
            },
            Band {
                id: "c".into(),
                min_y: 200.0,
                height: 100.0,
            },
        ]
    }

    #[test]
    fn moving_down_waits_until_the_pointer_crosses_the_row() {
        let bands = bands();
        assert_eq!(reorder_target(110.0, &bands, "a"), None);
        assert_eq!(reorder_target(120.0, &bands, "a").as_deref(), Some("b"));
        let land = landing(120.0, &bands, "a").unwrap();
        assert_eq!(land.target, "b");
        assert!(land.after);
    }

    #[test]
    fn moving_up_lands_before_the_target() {
        let bands = bands();
        assert_eq!(reorder_target(190.0, &bands, "c"), None);
        assert_eq!(reorder_target(180.0, &bands, "c").as_deref(), Some("b"));
        let land = landing(180.0, &bands, "c").unwrap();
        assert!(!land.after);
    }

    #[test]
    fn a_missing_drag_or_a_gap_is_not_a_target() {
        let bands = bands();
        assert_eq!(reorder_target(50.0, &bands, "missing"), None);
        assert_eq!(reorder_target(50.0, &bands, "a"), None);
        assert_eq!(reorder_target(1000.0, &bands, "a"), None);
    }

    #[test]
    fn the_first_band_that_contains_the_pointer_wins() {
        let bands = vec![
            Band {
                id: "a".into(),
                min_y: 0.0,
                height: 100.0,
            },
            Band {
                id: "b".into(),
                min_y: 10.0,
                height: 100.0,
            },
        ];
        assert_eq!(reorder_target(40.0, &bands, "b").as_deref(), Some("a"));
    }

    #[test]
    fn undo_returns_the_newest_snapshot_and_stops_when_empty() {
        let mut stack = UndoStack::default();
        assert!(stack.undo().is_none());
        let mut newer = LayoutState::default();
        newer.provider_order = vec!["codex".into()];
        stack.push(LayoutState::default());
        stack.push(newer.clone());
        assert_eq!(stack.undo(), Some(newer));
        assert_eq!(stack.undo(), Some(LayoutState::default()));
        assert!(stack.is_empty());
    }
}
