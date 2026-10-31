//! Colors, fonts and the shared panel look, matching the earlier web UI.

use std::borrow::Cow;

use wgpui::{AssetSource, BoxShadow, Div, Rgba, SharedString, Styled, div, point, prelude::*, px, rgba};

pub const MONO: &str = "JetBrains Mono";
pub const UI: &str = ".SystemUIFont";

pub fn text() -> Rgba {
    rgba(0xe7e9efff)
}
pub fn muted() -> Rgba {
    rgba(0x8a90a0ff)
}
pub fn faint() -> Rgba {
    rgba(0x5c6170ff)
}
pub fn accent() -> Rgba {
    rgba(0x5fbaa8ff)
}
pub fn danger() -> Rgba {
    rgba(0xf07a6aff)
}
/// Measurement amber, for values and labels.
pub fn amber() -> Rgba {
    rgba(0xf5c454ff)
}
pub fn border() -> Rgba {
    rgba(0xffffff14)
}
/// White at `alpha` (0–255), for the translucent fills of buttons and chips.
pub fn white_a(alpha: u8) -> Rgba {
    rgba(0xffffff00 | alpha as u32)
}
/// The accent at `alpha` (0–255).
pub fn accent_a(alpha: u8) -> Rgba {
    rgba(0x5fbaa800 | alpha as u32)
}

/// A floating panel: dark translucent fill, hairline border, rounded corners, soft shadow.
/// It occludes the viewport, so clicks on it never reach the molecule.
pub fn panel() -> Div {
    div()
        .bg(rgba(0x101217e0))
        .border_1()
        .border_color(border())
        .rounded(px(12.0))
        .shadow(vec![BoxShadow {
            color: wgpui::hsla(0.0, 0.0, 0.0, 0.35),
            offset: point(px(0.0), px(8.0)),
            blur_radius: px(32.0),
            spread_radius: px(0.0),
            inset: false,
        }])
        .occlude()
}

/// Section headings in the panels: small, spaced capitals.
pub fn heading(label: &'static str) -> Div {
    div().mt(px(14.0)).mb(px(6.0)).text_size(px(11.0)).font_weight(wgpui::FontWeight::SEMIBOLD).text_color(faint()).child(label.to_uppercase())
}

/// `1234567` as `1,234,567`.
pub fn fmt(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Icons and fonts compiled into the binary.
pub struct Assets;

impl Assets {
    const FILES: &[(&str, &[u8])] = &[
        ("icons/undo.svg", include_bytes!("../assets/icons/undo.svg")),
        ("icons/redo.svg", include_bytes!("../assets/icons/redo.svg")),
        ("icons/save.svg", include_bytes!("../assets/icons/save.svg")),
        ("icons/measure.svg", include_bytes!("../assets/icons/measure.svg")),
        ("icons/eye.svg", include_bytes!("../assets/icons/eye.svg")),
        ("icons/eye-off.svg", include_bytes!("../assets/icons/eye-off.svg")),
        ("icons/chevron-up.svg", include_bytes!("../assets/icons/chevron-up.svg")),
        ("icons/chevron-down.svg", include_bytes!("../assets/icons/chevron-down.svg")),
        ("icons/arc.svg", include_bytes!("../assets/icons/arc.svg")),
    ];

    /// The bundled fonts, for `TextSystem::add_fonts`.
    pub fn fonts() -> Vec<Cow<'static, [u8]>> {
        vec![
            Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/JetBrainsMono-SemiBold.ttf")),
        ]
    }
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> wgpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(Self::FILES.iter().find(|(p, _)| *p == path).map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> wgpui::Result<Vec<SharedString>> {
        Ok(Self::FILES.iter().filter(|(p, _)| p.starts_with(path)).map(|(p, _)| SharedString::from(*p)).collect())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn thousands() {
        assert_eq!(super::fmt(0), "0");
        assert_eq!(super::fmt(999), "999");
        assert_eq!(super::fmt(1686), "1,686");
        assert_eq!(super::fmt(1234567), "1,234,567");
    }
}
