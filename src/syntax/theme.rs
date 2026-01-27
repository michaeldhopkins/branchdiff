use std::str::FromStr;

use syntect::highlighting::{
    Color, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};

/// Create a custom theme optimized for branchdiff's background colors.
/// Colors are chosen for high luminance to maintain contrast against
/// cyan, green, yellow, and red background tints.
pub fn branchdiff_theme() -> Theme {
    Theme {
        name: Some("branchdiff".to_string()),
        author: None,
        settings: ThemeSettings {
            foreground: Some(Color {
                r: 200,
                g: 200,
                b: 200,
                a: 255,
            }),
            background: None, // We use diff backgrounds
            ..Default::default()
        },
        scopes: vec![
            // Keywords: soft purple (180, 140, 200)
            theme_item("keyword", 180, 140, 200),
            theme_item("storage.type", 180, 140, 200),
            theme_item("storage.modifier", 180, 140, 200),
            // Strings: warm peach (220, 180, 140)
            theme_item("string", 220, 180, 140),
            // Comments: muted gray (128, 128, 140)
            theme_item("comment", 128, 128, 140),
            // Numbers: soft cyan (140, 200, 220)
            theme_item("constant.numeric", 140, 200, 220),
            theme_item("constant.language", 140, 200, 220),
            // Types: soft teal (140, 200, 180)
            theme_item("entity.name.type", 140, 200, 180),
            theme_item("entity.name.class", 140, 200, 180),
            theme_item("support.type", 140, 200, 180),
            theme_item("entity.name.struct", 140, 200, 180),
            theme_item("entity.name.enum", 140, 200, 180),
            theme_item("entity.name.trait", 140, 200, 180),
            // Functions: light gold (220, 200, 140)
            theme_item("entity.name.function", 220, 200, 140),
            theme_item("support.function", 220, 200, 140),
            theme_item("entity.name.method", 220, 200, 140),
            // Macros: slightly brighter gold
            theme_item("entity.name.macro", 230, 210, 150),
            // Variables/parameters: keep close to default but slightly tinted
            theme_item("variable.parameter", 190, 190, 200),
            // Punctuation: keep neutral
            theme_item("punctuation", 180, 180, 180),
        ],
    }
}

fn theme_item(scope: &str, r: u8, g: u8, b: u8) -> ThemeItem {
    ThemeItem {
        scope: ScopeSelectors::from_str(scope)
            .unwrap_or_else(|e| panic!("Invalid scope selector '{scope}': {e}")),
        style: StyleModifier {
            foreground: Some(Color { r, g, b, a: 255 }),
            ..Default::default()
        },
    }
}
