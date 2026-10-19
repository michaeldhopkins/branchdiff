//! The stylesheet of the `--html` page: one palette per theme, every colour a
//! CSS variable, so a theme swap is a variable block and nothing else.

use ratatui::style::Color;

/// The dark palette, the page's default (`:root`).
pub const DARK_PALETTE: &str = "\
  --bg:      #16181d;
  --panel:   #1e2127;
  --bg-hl:   #262a32;
  --border:  #343944;
  --fg:      #e6e8ec;
  --fg-em:   #ffffff;
  --fg-sec:  #b6bcc7;
  --c-added:    #3fb950;
  --c-staged:   #22d3ee;
  --c-unstaged: #f5a524;
  --c-deleted:  #ff6b66;
  --c-moved:    #8b93ff;
  --c-canceled: #f472b6;
  --red:     var(--c-deleted);
  --green:   var(--c-added);
  --cyan:    var(--c-staged);
  --tk-keyword: #d2a8ff;
  --tk-string:  #f0c090;
  --tk-comment: #adb3c0;
  --tk-number:  #8fd8f0;
  --tk-type:    #7fe0c0;
  --tk-func:    #f0d890;
  --tk-macro:   #f5dc9a;
  --tk-param:   #d0d4e0;
  --tk-punct:   #c0c4cc;";

/// The light palette, shared by the `data-theme="light"` rule and the
/// script-less fallback so the two cannot drift.
pub const LIGHT_PALETTE: &str = "\
  --bg:      #ffffff;
  --panel:   #ffffff;
  --bg-hl:   #f1f3f6;
  --border:  #d0d5dd;
  --fg:      #14171c;
  --fg-em:   #000000;
  --fg-sec:  #4a5160;
  --c-added:    #1a7f37;
  --c-staged:   #0e7490;
  --c-unstaged: #b45309;
  --c-deleted:  #cf222e;
  --c-moved:    #4f46e5;
  --c-canceled: #be185d;
  --red:     var(--c-deleted);
  --green:   var(--c-added);
  --cyan:    var(--c-staged);
  --tk-keyword: #6f2dbd;
  --tk-string:  #7f4505;
  --tk-comment: #565d6b;
  --tk-number:  #0a5a80;
  --tk-type:    #0a6350;
  --tk-func:    #6e5200;
  --tk-macro:   #6e5000;
  --tk-param:   #2b3040;
  --tk-punct:   #3c414d;";

/// The CSS class for a highlighter token colour. The fixed RGB values are the
/// ones `syntax::theme` yields; any other colour returns `None` and the caller
/// falls back to an inline style.
pub fn token_class(color: Color) -> Option<&'static str> {
    match color {
        Color::Rgb(180, 140, 200) => Some("tk-keyword"),
        Color::Rgb(220, 180, 140) => Some("tk-string"),
        Color::Rgb(128, 128, 140) => Some("tk-comment"),
        Color::Rgb(140, 200, 220) => Some("tk-number"),
        Color::Rgb(140, 200, 180) => Some("tk-type"),
        Color::Rgb(220, 200, 140) => Some("tk-func"),
        Color::Rgb(230, 210, 150) => Some("tk-macro"),
        Color::Rgb(190, 190, 200) => Some("tk-param"),
        Color::Rgb(180, 180, 180) => Some("tk-punct"),
        _ => None,
    }
}

/// The page's `<style>` body.
pub fn stylesheet() -> String {
    format!(
        r#":root {{
{DARK_PALETTE}
}}
[data-theme="light"] {{
{LIGHT_PALETTE}
}}
* {{ margin: 0; padding: 0; box-sizing: border-box; }}
body {{
  background: var(--bg);
  color: var(--fg);
  font-family: 'SF Mono','Menlo','Consolas','Liberation Mono',monospace;
  font-size: 14px;
  line-height: 1.5;
  -webkit-text-size-adjust: 100%;
  padding: 16px;
}}
.header {{
  position: relative;
  color: var(--cyan);
  padding: 12px 0;
  border-bottom: 1px solid var(--border);
  margin-bottom: 16px;
}}
.header h1 {{ font-size: 16px; font-weight: 600; }}
.header .stats {{ font-size: 13px; color: var(--fg-sec); margin-top: 4px; }}
.theme-toggle {{
  position: absolute; right: 0; top: 4px;
  background: none; border: 1px solid var(--border);
  border-radius: 6px; padding: 8px 14px;
  color: var(--fg-sec); cursor: pointer; font-size: 24px;
  line-height: 1; min-width: 44px; min-height: 44px;
}}
.theme-toggle:hover {{ color: var(--fg-em); }}
.toc {{
  margin-bottom: 20px;
  padding: 12px;
  background: var(--bg-hl);
  border-radius: 6px;
}}
.toc-title {{ font-size: 13px; color: var(--fg-sec); margin-bottom: 8px; }}
.toc a {{
  display: block;
  color: var(--cyan);
  text-decoration: none;
  padding: 2px 0;
  font-size: 13px;
}}
.toc a:hover {{ text-decoration: underline; }}
.adds {{ color: var(--green); }}
.dels {{ color: var(--red); }}
details {{
  margin-bottom: 12px;
  border: 1px solid var(--border);
  border-radius: 6px;
  overflow: hidden;
  background: var(--panel);
}}
summary {{
  padding: 8px 12px;
  background: var(--bg-hl);
  cursor: pointer;
  font-weight: 600;
  color: var(--fg-em);
  font-size: 13px;
  user-select: none;
}}
summary:hover {{ background: var(--border); }}
summary .adds {{ font-weight: 400; }}
summary .dels {{ font-weight: 400; }}
table {{
  width: 100%;
  border-collapse: collapse;
  table-layout: fixed;
  background: var(--panel);
}}
td {{ vertical-align: top; white-space: pre-wrap; word-break: break-all; }}
.ln {{
  color: var(--fg-sec);
  user-select: none;
  text-align: right;
  padding: 0 8px 0 4px;
  width: 4em;
  min-width: 4em;
  white-space: nowrap;
}}
.gutter {{
  width: 1.5em;
  text-align: center;
  color: var(--fg-sec);
  user-select: none;
  padding: 0;
}}
.content {{
  padding: 0 8px;
  overflow-x: auto;
}}
.line-base {{ }}
.line-committed {{ background: color-mix(in srgb, var(--c-added) 19%, transparent); }}
.line-staged {{ background: color-mix(in srgb, var(--c-staged) 19%, transparent); }}
.line-unstaged {{ background: color-mix(in srgb, var(--c-unstaged) 19%, transparent); }}
.line-del-base {{ background: color-mix(in srgb, var(--c-deleted) 19%, transparent); }}
.line-del-committed {{ background: color-mix(in srgb, var(--c-deleted) 19%, transparent); }}
.line-del-staged {{ background: color-mix(in srgb, var(--c-deleted) 19%, transparent); }}
.line-moved {{ background: color-mix(in srgb, var(--c-moved) 19%, transparent); }}
.line-canceled {{ background: color-mix(in srgb, var(--c-canceled) 19%, transparent); }}
.line-committed td:first-child {{ border-left: 3px solid var(--c-added); }}
.line-staged td:first-child {{ border-left: 3px solid var(--c-staged); }}
.line-unstaged td:first-child {{ border-left: 3px solid var(--c-unstaged); }}
.line-del-base td:first-child {{ border-left: 3px solid var(--c-deleted); }}
.line-del-committed td:first-child {{ border-left: 3px solid var(--c-deleted); }}
.line-del-staged td:first-child {{ border-left: 3px solid var(--c-deleted); }}
.line-moved td:first-child {{ border-left: 3px solid var(--c-moved); }}
.line-canceled td:first-child {{ border-left: 3px solid var(--c-canceled); }}
.line-header td {{ padding: 8px 12px; color: var(--fg-em); font-weight: bold; }}
.line-elided td {{ color: var(--fg-sec); padding: 2px 12px; font-style: italic; }}
/* Word highlights outline rather than fill: the fill alternative — `background:
   <hue> at 0.35 alpha` — drops some token colours to 2.9-4.5:1 on the tint,
   while the outline and underline leave the tokens' own contrast intact. */
.hl-committed {{ outline: 1px solid var(--c-added); outline-offset: 0; border-radius: 2px; text-decoration: underline 2px var(--c-added); text-underline-offset: 3px; }}
.hl-staged {{ outline: 1px solid var(--c-staged); outline-offset: 0; border-radius: 2px; text-decoration: underline 2px var(--c-staged); text-underline-offset: 3px; }}
.hl-unstaged {{ outline: 1px solid var(--c-unstaged); outline-offset: 0; border-radius: 2px; text-decoration: underline 2px var(--c-unstaged); text-underline-offset: 3px; }}
.hl-del-base {{ outline: 1px solid var(--c-deleted); outline-offset: 0; border-radius: 2px; text-decoration: underline 2px var(--c-deleted); text-underline-offset: 3px; }}
.hl-del-committed {{ outline: 1px solid var(--c-deleted); outline-offset: 0; border-radius: 2px; text-decoration: underline 2px var(--c-deleted); text-underline-offset: 3px; }}
.hl-del-staged {{ outline: 1px solid var(--c-deleted); outline-offset: 0; border-radius: 2px; text-decoration: underline 2px var(--c-deleted); text-underline-offset: 3px; }}
.hl-canceled {{ outline: 1px solid var(--c-canceled); outline-offset: 0; border-radius: 2px; text-decoration: underline 2px var(--c-canceled); text-underline-offset: 3px; }}
.del {{ text-decoration-line: line-through; text-decoration-thickness: 1px; }}
.hl-del-base.del, .hl-del-committed.del, .hl-del-staged.del {{ text-decoration-line: underline line-through; text-decoration-thickness: 2px; }}
.tk-keyword {{ color: var(--tk-keyword); }}
.tk-string {{ color: var(--tk-string); }}
.tk-comment {{ color: var(--tk-comment); }}
.tk-number {{ color: var(--tk-number); }}
.tk-type {{ color: var(--tk-type); }}
.tk-func {{ color: var(--tk-func); }}
.tk-macro {{ color: var(--tk-macro); }}
.tk-param {{ color: var(--tk-param); }}
.tk-punct {{ color: var(--tk-punct); }}
.image-container {{
  display: flex;
  gap: 16px;
  padding: 12px;
  flex-wrap: wrap;
  justify-content: center;
}}
.image-panel {{ text-align: center; }}
.image-panel img {{
  max-width: 100%;
  border: 1px solid var(--border);
  border-radius: 4px;
}}
.image-panel .meta {{
  font-size: 12px;
  color: var(--fg-sec);
  margin-top: 4px;
}}
.image-label {{
  font-size: 12px;
  color: var(--fg-sec);
  margin-bottom: 4px;
  text-transform: uppercase;
  letter-spacing: 0.5px;
}}
.footer {{
  margin-top: 24px;
  padding-top: 12px;
  border-top: 1px solid var(--border);
  font-size: 12px;
  color: var(--fg-sec);
}}
.footer a {{ color: var(--cyan); }}
@media (max-width: 768px) {{
  body {{ padding: 8px; font-size: 13px; }}
  .ln {{ width: 3em; min-width: 3em; font-size: 12px; }}
}}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn palette_vars(palette: &str) -> HashMap<&str, &str> {
        palette
            .lines()
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim(), value.trim().trim_end_matches(';').trim()))
            .collect()
    }

    fn parse_hex(value: &str) -> (u8, u8, u8) {
        let n = u32::from_str_radix(&value[1..], 16).expect("palette value is #rrggbb");
        ((n >> 16) as u8, (n >> 8) as u8, n as u8)
    }

    fn channel(c: u8) -> f64 {
        let c = f64::from(c) / 255.0;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }

    fn relative_luminance(rgb: (u8, u8, u8)) -> f64 {
        0.2126 * channel(rgb.0) + 0.7152 * channel(rgb.1) + 0.0722 * channel(rgb.2)
    }

    fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let (la, lb) = (relative_luminance(a), relative_luminance(b));
        let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn over(hue: (u8, u8, u8), alpha: f64, panel: (u8, u8, u8)) -> (u8, u8, u8) {
        let mix =
            |c: u8, p: u8| (f64::from(c) * alpha + f64::from(p) * (1.0 - alpha)).round() as u8;
        (mix(hue.0, panel.0), mix(hue.1, panel.1), mix(hue.2, panel.2))
    }

    #[test]
    fn the_light_palette_defines_every_variable_the_dark_one_does() {
        let dark = palette_vars(DARK_PALETTE);
        let light = palette_vars(LIGHT_PALETTE);
        assert_eq!(dark.len(), light.len(), "the two palettes cover the same variables");
        for name in dark.keys() {
            assert!(light.contains_key(name), "the light palette is missing {name}");
        }
    }

    #[test]
    fn the_stylesheet_carries_each_palette_exactly_once() {
        let css = stylesheet();
        assert_eq!(css.matches(DARK_PALETTE).count(), 1);
        assert_eq!(css.matches(LIGHT_PALETTE).count(), 1);
    }

    #[test]
    fn every_theme_token_maps_to_one_class() {
        for (color, class) in [
            (Color::Rgb(180, 140, 200), "tk-keyword"),
            (Color::Rgb(220, 180, 140), "tk-string"),
            (Color::Rgb(128, 128, 140), "tk-comment"),
            (Color::Rgb(140, 200, 220), "tk-number"),
            (Color::Rgb(140, 200, 180), "tk-type"),
            (Color::Rgb(220, 200, 140), "tk-func"),
            (Color::Rgb(230, 210, 150), "tk-macro"),
            (Color::Rgb(190, 190, 200), "tk-param"),
            (Color::Rgb(180, 180, 180), "tk-punct"),
        ] {
            assert_eq!(token_class(color), Some(class));
        }
        assert_eq!(token_class(Color::Rgb(255, 0, 128)), None, "an unknown colour has no class");
        assert_eq!(token_class(Color::Reset), None);
    }

    #[test]
    fn every_colour_the_syntax_theme_emits_has_a_token_class() {
        let theme = crate::syntax::theme::branchdiff_theme();
        let default = Color::Rgb(200, 200, 200);
        let mut colours: Vec<Color> = theme
            .scopes
            .iter()
            .filter_map(|item| item.style.foreground)
            .map(|c| Color::Rgb(c.r, c.g, c.b))
            .collect();
        colours.extend(theme.settings.foreground.map(|c| Color::Rgb(c.r, c.g, c.b)));
        for colour in colours {
            assert!(
                colour == default || token_class(colour).is_some(),
                "{colour:?} would fall back to an inline dark-theme colour"
            );
        }
    }

    #[test]
    fn every_token_has_a_rule_and_a_value_in_both_themes() {
        let css = stylesheet();
        let names =
            ["keyword", "string", "comment", "number", "type", "func", "macro", "param", "punct"];
        for name in names {
            assert!(css.contains(&format!(".tk-{name} {{ color: var(--tk-{name}); }}")), "{name}");
            for palette in [DARK_PALETTE, LIGHT_PALETTE] {
                assert!(
                    palette_vars(palette).contains_key(format!("--tk-{name}").as_str()),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn a_deleted_word_keeps_its_underline_beside_the_strike_through() {
        let css = stylesheet();
        assert!(css.contains(".hl-del-base.del, .hl-del-committed.del, .hl-del-staged.del { text-decoration-line: underline line-through;"));
    }

    #[test]
    fn both_palettes_hold_their_wcag_contrast() {
        for (theme, palette) in [("dark", DARK_PALETTE), ("light", LIGHT_PALETTE)] {
            let vars = palette_vars(palette);
            let var = |name: &str| parse_hex(vars[name]);
            let panel = var("--panel");
            assert!(
                contrast_ratio(var("--fg"), panel) >= 12.0,
                "{theme}: --fg on --panel is below 12:1"
            );
            assert!(
                contrast_ratio(var("--fg-sec"), panel) >= 7.0,
                "{theme}: --fg-sec on --panel is below 7:1"
            );
            for hue in [
                "--c-added", "--c-staged", "--c-unstaged", "--c-deleted", "--c-moved",
                "--c-canceled",
            ] {
                let tint = over(var(hue), 0.19, panel);
                for token in [
                    "--tk-keyword", "--tk-string", "--tk-comment", "--tk-number", "--tk-type",
                    "--tk-func", "--tk-macro", "--tk-param", "--tk-punct",
                ] {
                    assert!(
                        contrast_ratio(var(token), tint) >= 4.5,
                        "{theme}: {token} on {hue} at 0.19 over --panel is below 4.5:1"
                    );
                }
            }
        }
    }
}
