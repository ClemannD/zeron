//! Math fonts compiled into the binary (feature `bundled`): STIX Two Math,
//! and Fira Math falling back to it. Each loads once per process and is
//! shared, so every host and thread uses the same parsed font and caches.
//!
//! [`layout`] numbers glyph fonts against one fixed table ([`FONTS`]) rather
//! than the chosen face's fallback chain, so a host can register the bundled
//! fonts once and paint formulas from either face without knowing which.

use std::sync::{Arc, OnceLock};

use crate::{Error, Formula, Item, MathFont, Options};

static STIX_TWO: &[u8] = include_bytes!("../fonts/STIXTwoMath-Regular.otf");
static FIRA: &[u8] = include_bytes!("../fonts/FiraMath-Regular.otf");

/// The bundled faces apps can pick between.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Face {
    /// STIX Two Math: the classic Times-like TeX look; covers everything.
    StixTwo,
    /// Fira Math: a sans face that sits beside UI sans-serifs, falling back
    /// to STIX Two for the alphabets and symbols it lacks.
    #[default]
    Fira,
}

/// How many fonts [`layout`] can name: 0 is Fira Math, 1 is STIX Two Math.
pub const FONTS: u8 = 2;

fn stix_arc() -> &'static Arc<MathFont> {
    static FONT: OnceLock<Arc<MathFont>> = OnceLock::new();
    FONT.get_or_init(|| Arc::new(MathFont::new(STIX_TWO).expect("bundled STIX Two Math")))
}

/// The font for `face` (with its fallback chain), loaded on first use.
pub fn font(face: Face) -> &'static MathFont {
    match face {
        Face::StixTwo => stix_arc(),
        Face::Fira => {
            static FONT: OnceLock<MathFont> = OnceLock::new();
            FONT.get_or_init(|| {
                MathFont::new(FIRA)
                    .expect("bundled Fira Math")
                    .with_fallback(stix_arc().clone())
            })
        }
    }
}

/// The font a [`layout`]ed glyph's `font` index names (see [`FONTS`]). Its
/// [`MathFont::data`] and [`MathFont::outline`] are that one font's.
pub fn font_at(index: u8) -> &'static MathFont {
    match index {
        0 => font(Face::Fira),
        _ => stix_arc(),
    }
}

/// [`crate::layout`] in a bundled face, with glyph font indices into the
/// fixed table [`font_at`] resolves — the same for both faces.
pub fn layout(source: &str, face: Face, options: &Options) -> Result<Formula, Error> {
    let mut formula = crate::layout(source, font(face), options)?;
    if face == Face::StixTwo {
        // STIX Two's chain is just itself; in the table it's font 1.
        for item in &mut formula.items {
            if let Item::Glyph { font, .. } = item {
                *font = 1;
            }
        }
    }
    Ok(formula)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_faces_share_one_font_table() {
        let fira = font_at(0).name().unwrap();
        let stix = font_at(1).name().unwrap();
        assert!(
            fira.contains("Fira") && stix.contains("STIX"),
            "{fira} / {stix}"
        );
        // Fira's chain is the table itself.
        for i in 0..FONTS {
            assert_eq!(
                font(Face::Fira).at(i).data().as_ptr(),
                font_at(i).data().as_ptr()
            );
        }
        let src = r"\mathcal{L} = \frac{x}{y}";
        let opts = Options::inline(16.0);
        for face in [Face::Fira, Face::StixTwo] {
            let f = layout(src, face, &opts).unwrap();
            let raw = crate::layout(src, font(face), &opts).unwrap();
            assert_eq!(f.items.len(), raw.items.len());
            for (item, raw) in f.items.iter().zip(&raw.items) {
                if let (
                    Item::Glyph { font: i, glyph, .. },
                    Item::Glyph {
                        font: r, glyph: g, ..
                    },
                ) = (item, raw)
                {
                    assert_eq!(glyph, g);
                    assert_eq!(
                        font_at(*i).data().as_ptr(),
                        font(face).at(*r).data().as_ptr()
                    );
                }
            }
        }
        // Fira really falls back to STIX for script letters: font 1.
        let f = layout(r"\mathcal{L}", Face::Fira, &opts).unwrap();
        assert!(matches!(f.items[0], Item::Glyph { font: 1, .. }));
    }
}
