//! Which corner the panel sits in. Omarchy records the bar edge in `~/.config/omarchy/shell.json`.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarPosition {
    Top,
    Bottom,
    Left,
    Right,
}

/// Anchors and the gap that clears the bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Corner {
    pub top: bool,
    pub left: bool,
    pub vertical_margin: i32,
    pub horizontal_margin: i32,
}

const GAP: i32 = 8;

pub fn read_bar_position() -> BarPosition {
    let path = openusage_core::paths::home().join(".config/omarchy/shell.json");
    match std::fs::read_to_string(path) {
        Ok(text) => position_in(&text).unwrap_or(BarPosition::Top),
        Err(_) => BarPosition::Top,
    }
}

pub fn position_in(json: &str) -> Option<BarPosition> {
    let value: Value = serde_json::from_str(json).ok()?;
    match value.get("bar")?.get("position")?.as_str()? {
        "bottom" => Some(BarPosition::Bottom),
        "left" => Some(BarPosition::Left),
        "right" => Some(BarPosition::Right),
        "top" => Some(BarPosition::Top),
        _ => Some(BarPosition::Top),
    }
}

/// Bottom bar → bottom-right, top bar → top-right. `thickness` is the bar's px size on that edge.
pub fn corner_for(position: BarPosition, thickness: i32) -> Corner {
    let clearance = thickness.max(0) + GAP;
    match position {
        BarPosition::Bottom => Corner { top: false, left: false, vertical_margin: clearance, horizontal_margin: GAP },
        BarPosition::Top => Corner { top: true, left: false, vertical_margin: clearance, horizontal_margin: GAP },
        BarPosition::Left => Corner { top: false, left: true, vertical_margin: GAP, horizontal_margin: clearance },
        BarPosition::Right => Corner { top: false, left: false, vertical_margin: GAP, horizontal_margin: clearance },
    }
}

/// Where the panel opens: the corner beside the bar, and the output that bar is on.
pub struct Placement {
    pub corner: Corner,
    pub output: Option<String>,
}

/// The Omarchy/Waybar layer's thickness and output, from `hyprctl layers`.
pub fn detect_bar(position: BarPosition) -> (i32, Option<String>) {
    let Ok(output) = std::process::Command::new("hyprctl").args(["layers", "-j"]).output() else {
        return (0, None);
    };
    if !output.status.success() {
        return (0, None);
    }
    let Ok(text) = String::from_utf8(output.stdout) else { return (0, None) };
    let Ok(value) = serde_json::from_str::<Value>(&text) else { return (0, None) };
    bar_in(&value, position)
}

fn bar_in(value: &Value, position: BarPosition) -> (i32, Option<String>) {
    let Some(monitors) = value.as_object() else { return (0, None) };
    let mut best: Option<(i64, i64, String)> = None;
    for (name, body) in monitors {
        let Some(levels) = body.get("levels").and_then(Value::as_object) else { continue };
        for layers in levels.values().filter_map(Value::as_array) {
            for layer in layers.iter().filter_map(Value::as_object) {
                let Some((coord, thick)) = bar_edge(layer, position) else { continue };
                if best.as_ref().is_none_or(|(best_coord, _, _)| coord > *best_coord) {
                    best = Some((coord, thick, name.clone()));
                }
            }
        }
    }
    match best {
        Some((_, thick, name)) => (thick as i32, Some(name)),
        None => (0, None),
    }
}

fn bar_edge(layer: &serde_json::Map<String, Value>, position: BarPosition) -> Option<(i64, i64)> {
    let namespace = layer.get("namespace").and_then(Value::as_str)?.to_ascii_lowercase();
    if !namespace.contains("bar") || namespace.contains("openusage") {
        return None;
    }
    let height = layer.get("h").and_then(Value::as_i64).unwrap_or(0);
    let width = layer.get("w").and_then(Value::as_i64).unwrap_or(0);
    let y = layer.get("y").and_then(Value::as_i64).unwrap_or(0);
    let x = layer.get("x").and_then(Value::as_i64).unwrap_or(0);
    let (coord, thick) = match position {
        BarPosition::Bottom => (y, height),
        BarPosition::Top => (-y, height),
        BarPosition::Left => (-x, width),
        BarPosition::Right => (x, width),
    };
    (1..=160).contains(&thick).then_some((coord, thick))
}

pub fn placement() -> Placement {
    let position = read_bar_position();
    let (thickness, output) = detect_bar(position);
    Placement { corner: corner_for(position, thickness), output }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_json_bottom_anchors_the_bottom_right() {
        let json = r#"{"bar":{"position":"bottom","id":"omarchy.bar"}}"#;
        assert_eq!(position_in(json), Some(BarPosition::Bottom));
        let corner = corner_for(BarPosition::Bottom, 26);
        assert!(!corner.top && !corner.left);
        assert_eq!(corner.vertical_margin, 34);
        assert_eq!(corner.horizontal_margin, GAP);
    }

    #[test]
    fn missing_position_defaults_to_the_top_right() {
        assert_eq!(position_in("{}"), None);
        let corner = corner_for(BarPosition::Top, 0);
        assert!(corner.top && !corner.left);
        assert_eq!(corner.vertical_margin, GAP);
    }

    #[test]
    fn the_bar_layer_sets_the_clearance_and_output() {
        let json = r#"{"DP-2":{"levels":{"2":[{"namespace":"omarchy-bar","x":0,"y":2134,"w":1920,"h":26}]}}}"#;
        let value: Value = serde_json::from_str(json).unwrap();
        assert_eq!(bar_in(&value, BarPosition::Bottom), (26, Some("DP-2".into())));
    }
}
