// SPDX-License-Identifier: AGPL-3.0-or-later
//! Text for Dawkey's UI: shaping and measuring with cosmic-text, and glyph
//! rasterization for the renderer.
//!
//! Only the fonts embedded here are used, never system fonts, so text looks
//! (and screenshot tests render) the same on every machine.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

pub use cosmic_text;

use cosmic_text::{
    Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Weight,
};
use dawkey_ui::{FontFamily, FontWeight, Size, TextMeasure, TextStyle};

const SANS: &str = "Noto Sans";
const MONO: &str = "Noto Sans Mono";

static FONTS: &[&[u8]] = &[
    include_bytes!("../assets/fonts/NotoSans-Regular.ttf"),
    include_bytes!("../assets/fonts/NotoSans-SemiBold.ttf"),
    include_bytes!("../assets/fonts/NotoSansMono-Regular.ttf"),
];

/// Line height as a multiple of the font size.
const LINE_HEIGHT: f32 = 1.3;
/// Measurement cache size before it is cleared.
const MEASURE_CACHE_LIMIT: usize = 4096;
/// Shaped-line cache size before it is cleared.
const SHAPE_CACHE_LIMIT: usize = 4096;

/// A glyph positioned in physical pixels, relative to the text's origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlacedGlyph {
    pub key: CacheKey,
    pub x: i32,
    pub y: i32,
}

/// One shaped line, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct ShapedLine {
    /// Logical width.
    pub width: f32,
    /// Logical height of the line box.
    pub height: f32,
    pub glyphs: Vec<PlacedGlyph>,
}

/// A rasterized glyph: an alpha mask, placed relative to the pen position.
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphImage {
    /// Offset from the pen x to the image's left edge.
    pub left: i32,
    /// Offset from the baseline up to the image's top edge.
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// `width × height` coverage values.
    pub alpha: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct MeasureKey {
    text: String,
    size_bits: u32,
    weight: FontWeight,
    family: FontFamily,
}

pub struct TextSystem {
    fonts: FontSystem,
    rasterizer: SwashCache,
    measured: HashMap<MeasureKey, Size>,
    /// Lines shaped for drawing, by text, style and scale. Views redraw the
    /// same text every frame; shaping it again each time is slow.
    shaped: HashMap<(MeasureKey, u32), ShapedLine>,
}

impl std::fmt::Debug for TextSystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextSystem").finish_non_exhaustive()
    }
}

impl Default for TextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl TextSystem {
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        for data in FONTS {
            db.load_font_source(fontdb::Source::Binary(Arc::new(*data)));
        }
        db.set_sans_serif_family(SANS);
        db.set_monospace_family(MONO);
        Self {
            // A fixed locale keeps fallback deterministic.
            fonts: FontSystem::new_with_locale_and_db("en-US".into(), db),
            rasterizer: SwashCache::new(),
            measured: HashMap::new(),
            shaped: HashMap::new(),
        }
    }

    /// Shapes `text` on one line. Glyph positions are physical pixels at
    /// `scale`, relative to the top-left of the line box.
    pub fn shape(&mut self, text: &str, style: &TextStyle, scale: f32) -> ShapedLine {
        let key = (
            MeasureKey {
                text: text.to_owned(),
                size_bits: style.size.to_bits(),
                weight: style.weight,
                family: style.family,
            },
            scale.to_bits(),
        );
        if let Some(line) = self.shaped.get(&key) {
            return line.clone();
        }
        let line = self.shape_uncached(text, style, scale);
        if self.shaped.len() >= SHAPE_CACHE_LIMIT {
            self.shaped.clear();
        }
        self.shaped.insert(key, line.clone());
        line
    }

    fn shape_uncached(&mut self, text: &str, style: &TextStyle, scale: f32) -> ShapedLine {
        let buffer = self.buffer(text, style);
        let mut line = ShapedLine {
            width: 0.0,
            height: line_height(style),
            glyphs: Vec::new(),
        };
        for run in buffer.layout_runs() {
            line.width = line.width.max(run.line_w);
            for glyph in run.glyphs {
                let physical = glyph.physical((0.0, run.line_y * scale), scale);
                line.glyphs.push(PlacedGlyph {
                    key: physical.cache_key,
                    x: physical.x,
                    y: physical.y,
                });
            }
        }
        line
    }

    /// Rasterizes one glyph. `None` for glyphs with no pixels (spaces).
    pub fn rasterize(&mut self, key: CacheKey) -> Option<GlyphImage> {
        let image = self.rasterizer.get_image_uncached(&mut self.fonts, key)?;
        let (width, height) = (image.placement.width, image.placement.height);
        if width == 0 || height == 0 {
            return None;
        }
        let alpha = match image.content {
            SwashContent::Mask => image.data,
            // No embedded font has color glyphs; treat any as opaque coverage.
            SwashContent::Color => image
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .map(|px| px[3])
                .collect(),
            SwashContent::SubpixelMask => image
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .map(|px| ((u16::from(px[0]) + u16::from(px[1]) + u16::from(px[2])) / 3) as u8)
                .collect(),
        };
        Some(GlyphImage {
            left: image.placement.left,
            top: image.placement.top,
            width,
            height,
            alpha,
        })
    }

    fn buffer(&mut self, text: &str, style: &TextStyle) -> Buffer {
        let metrics = Metrics::new(style.size, line_height(style));
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_size(None, None);
        let family = match style.family {
            FontFamily::Sans => Family::Name(SANS),
            FontFamily::Mono => Family::Name(MONO),
        };
        let weight = match style.weight {
            FontWeight::Regular => Weight::NORMAL,
            FontWeight::SemiBold => Weight::SEMIBOLD,
        };
        let attrs = Attrs::new().family(family).weight(weight);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        buffer
    }
}

fn line_height(style: &TextStyle) -> f32 {
    (style.size * LINE_HEIGHT).ceil()
}

impl TextMeasure for TextSystem {
    fn measure(&mut self, text: &str, style: &TextStyle) -> Size {
        let key = MeasureKey {
            text: text.to_owned(),
            size_bits: style.size.to_bits(),
            weight: style.weight,
            family: style.family,
        };
        if let Some(size) = self.measured.get(&key) {
            return *size;
        }
        let width = self
            .buffer(text, style)
            .layout_runs()
            .map(|run| run.line_w)
            .fold(0.0, f32::max);
        let size = Size::new(width.ceil(), line_height(style));
        if self.measured.len() >= MEASURE_CACHE_LIMIT {
            self.measured.clear();
        }
        self.measured.insert(key, size);
        size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(family: FontFamily, weight: FontWeight) -> TextStyle {
        TextStyle {
            size: 13.0,
            weight,
            family,
        }
    }

    #[test]
    fn measures_with_embedded_fonts() {
        let mut text = TextSystem::new();
        let sans = style(FontFamily::Sans, FontWeight::Regular);
        let hello = text.measure("Hello, Dawkey", &sans);
        assert!(hello.w > 60.0 && hello.w < 120.0, "{hello:?}");
        assert_eq!(hello.h, (13.0f32 * LINE_HEIGHT).ceil());
        assert_eq!(text.measure("", &sans).w, 0.0);

        // Monospace: every digit has the same advance.
        let mono = style(FontFamily::Mono, FontWeight::Regular);
        assert_eq!(text.measure("1111", &mono).w, text.measure("8888", &mono).w);
        // Semibold is wider than regular.
        let bold = style(FontFamily::Sans, FontWeight::SemiBold);
        assert!(text.measure("Hello, Dawkey", &bold).w > hello.w);
    }

    #[test]
    fn shapes_and_rasterizes() {
        let mut text = TextSystem::new();
        let sans = style(FontFamily::Sans, FontWeight::Regular);
        let line = text.shape("Ab c", &sans, 2.0);
        assert_eq!(line.glyphs.len(), 4);
        // Physical positions are scaled; x increases along the line.
        assert!(line.glyphs.windows(2).all(|w| w[1].x > w[0].x));
        let baseline = line.glyphs[0].y;
        assert!(
            baseline > 13 && baseline < 2 * line.height as i32,
            "{baseline}"
        );

        let a = text.rasterize(line.glyphs[0].key).expect("A has pixels");
        assert_eq!(a.alpha.len(), (a.width * a.height) as usize);
        assert!(a.alpha.iter().any(|&v| v > 200));
        assert!(
            text.rasterize(line.glyphs[2].key).is_none(),
            "space is empty"
        );
    }
}
