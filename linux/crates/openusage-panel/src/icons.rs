//! Provider marks from the Mac app's SVG set. Monochrome marks are recolored to the foreground;
//! a mark that carries its own color (Antigravity blue) is left alone.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk4::gdk;
use gtk4::prelude::*;

use openusage_core::kind::ProviderKind;

use crate::prefers_dark;

pub fn image(kind: ProviderKind, side: i32) -> gtk4::Widget {
    match texture(kind_bytes(kind), side) {
        Some(texture) => image_from(texture, side),
        None => fallback(kind.display_name(), side),
    }
}

pub fn app_icon(side: i32) -> gtk4::Widget {
    let bytes =
        include_bytes!("../../../../Sources/OpenUsage/Resources/ProviderIcons/openusage.svg");
    match texture(bytes, side) {
        Some(texture) => image_from(texture, side),
        None => fallback("OpenUsage", side),
    }
}

fn image_from(texture: gdk::MemoryTexture, side: i32) -> gtk4::Widget {
    let image = gtk4::Image::from_paintable(Some(&texture));
    image.set_pixel_size(side);
    image.upcast()
}

fn fallback(name: &str, side: i32) -> gtk4::Widget {
    let letter = name.chars().next().unwrap_or('?').to_string();
    let label = gtk4::Label::new(Some(&letter));
    label.set_size_request(side, side);
    label.add_css_class("provider");
    label.upcast()
}

fn texture(bytes: &'static [u8], side: i32) -> Option<gdk::MemoryTexture> {
    let dark = prefers_dark();
    let key = (bytes.as_ptr() as usize, dark, side);
    CACHE.with(|cache| {
        if let Some(texture) = cache.borrow().get(&key) {
            return Some(texture.clone());
        }
        let texture = rasterize(bytes, side, dark)?;
        cache.borrow_mut().insert(key, texture.clone());
        Some(texture)
    })
}

thread_local! {
    static CACHE: RefCell<HashMap<(usize, bool, i32), gdk::MemoryTexture>> = RefCell::new(HashMap::new());
}

fn rasterize(bytes: &[u8], side: i32, dark: bool) -> Option<gdk::MemoryTexture> {
    let foreground = if dark { "#f5f5f7" } else { "#1d1d1f" };
    let svg = recolor(&String::from_utf8_lossy(bytes), foreground);
    let tree =
        resvg::usvg::Tree::from_data(svg.as_bytes(), &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = (side as f32 * 4.0) / size.width().max(size.height()).max(1.0);
    let pixel = (side * 4).max(1) as u32;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(pixel, pixel)?;
    let drawn_w = size.width() * scale;
    let drawn_h = size.height() * scale;
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(
        (pixel as f32 - drawn_w) / 2.0,
        (pixel as f32 - drawn_h) / 2.0,
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let bytes = gtk4::glib::Bytes::from_owned(pixmap.take());
    Some(gdk::MemoryTexture::new(
        pixel as i32,
        pixel as i32,
        gdk::MemoryFormat::R8g8b8a8Premultiplied,
        &bytes,
        (pixel * 4) as usize,
    ))
}

fn kind_bytes(kind: ProviderKind) -> &'static [u8] {
    match kind {
        ProviderKind::Claude => {
            include_bytes!("../../../../Sources/OpenUsage/Resources/ProviderIcons/claude.svg")
        }
        ProviderKind::Codex => {
            include_bytes!("../../../../Sources/OpenUsage/Resources/ProviderIcons/codex.svg")
        }
        ProviderKind::Cursor => {
            include_bytes!("../../../../Sources/OpenUsage/Resources/ProviderIcons/cursor.svg")
        }
        ProviderKind::Antigravity => {
            include_bytes!("../../../../Sources/OpenUsage/Resources/ProviderIcons/antigravity.svg")
        }
        ProviderKind::Copilot => {
            include_bytes!("../../../../Sources/OpenUsage/Resources/ProviderIcons/copilot.svg")
        }
        ProviderKind::Devin => {
            include_bytes!("../../../../Sources/OpenUsage/Resources/ProviderIcons/devin.svg")
        }
        ProviderKind::Grok => {
            include_bytes!("../../../../Sources/OpenUsage/Resources/ProviderIcons/grok.svg")
        }
        ProviderKind::Ollama => {
            include_bytes!("../../../../Sources/OpenUsage/Resources/ProviderIcons/ollama.svg")
        }
        ProviderKind::OpenCode => {
            include_bytes!("../../../../Sources/OpenUsage/Resources/ProviderIcons/opencode.svg")
        }
        ProviderKind::OpenRouter => {
            include_bytes!("../../../../Sources/OpenUsage/Resources/ProviderIcons/openrouter.svg")
        }
        ProviderKind::Zai => {
            include_bytes!("../../../../Sources/OpenUsage/Resources/ProviderIcons/zai.svg")
        }
    }
}

/// Monochrome when every hex color is a shade of gray (or there is no hex color at all).
pub fn chromatic(svg: &str) -> bool {
    let bytes = svg.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'#' {
            let hex: String = svg[index + 1..]
                .chars()
                .take_while(|char| char.is_ascii_hexdigit())
                .collect();
            if (hex.len() == 3 || hex.len() == 6) && !is_gray(&hex) {
                return true;
            }
        }
        index += 1;
    }
    false
}

fn is_gray(hex: &str) -> bool {
    let expanded: String = if hex.len() == 3 {
        hex.chars().flat_map(|char| [char, char]).collect()
    } else {
        hex.to_string()
    };
    let Ok(red) = u8::from_str_radix(&expanded[0..2], 16) else {
        return false;
    };
    let Ok(green) = u8::from_str_radix(&expanded[2..4], 16) else {
        return false;
    };
    let Ok(blue) = u8::from_str_radix(&expanded[4..6], 16) else {
        return false;
    };
    red == green && green == blue
}

pub fn recolor(svg: &str, foreground: &str) -> String {
    if chromatic(svg) {
        return svg.to_string();
    }
    let mut colored = svg.replace("currentColor", foreground);
    for name in [
        "white", "#fff", "#FFF", "#ffffff", "#FFFFFF", "black", "#000", "#000000",
    ] {
        colored = colored.replace(
            &format!("fill=\"{name}\""),
            &format!("fill=\"{foreground}\""),
        );
        colored = colored.replace(
            &format!("stroke=\"{name}\""),
            &format!("stroke=\"{foreground}\""),
        );
    }
    colored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_keeps_its_blue_and_claude_is_recolored() {
        let antigravity =
            include_str!("../../../../Sources/OpenUsage/Resources/ProviderIcons/antigravity.svg");
        let claude =
            include_str!("../../../../Sources/OpenUsage/Resources/ProviderIcons/claude.svg");
        let copilot =
            include_str!("../../../../Sources/OpenUsage/Resources/ProviderIcons/copilot.svg");
        assert!(chromatic(antigravity));
        assert!(!chromatic(claude));
        assert!(!chromatic(copilot));
        assert!(recolor(claude, "#fff").contains("fill=\"#fff\""));
        assert!(recolor(copilot, "#fff").contains("fill=\"#fff\""));
        assert!(recolor(antigravity, "#fff").contains("#4285F4"));
    }
}
