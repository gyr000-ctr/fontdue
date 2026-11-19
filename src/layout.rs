pub use crate::unicode::CharacterData;

use crate::unicode::{read_utf8, LinebreakData, Linebreaker, LINEBREAK_NONE};
use crate::Font;
use crate::{
    platform::{ceil, floor},
    Metrics,
};
use alloc::vec;
use alloc::vec::*;
use core::borrow::Borrow;
use core::hash::{Hash, Hasher};
use core::ops::Range;

/// Horizontal alignment options for text when a max_width is provided.
#[derive(Copy, Clone, PartialEq)]
pub enum HorizontalAlign {
    /// Aligns text to the left of the region defined by the max_width.
    Left,
    /// Aligns text to the center of the region defined by the max_width.
    Center,
    /// Aligns text to the right of the region defined by the max_width.
    Right,
}

/// Vertical alignment options for text when a max_height is provided.
#[derive(Copy, Clone, PartialEq)]
pub enum VerticalAlign {
    /// Aligns text to the top of the region defined by the max_height.
    Top,
    /// Aligns text to the middle of the region defined by the max_height.
    Middle,
    /// Aligns text to the bottom of the region defined by the max_height.
    Bottom,
}

/// Wrap style is a hint for how strings of text should be wrapped to the next line. Line wrapping
/// can happen when the max width/height is reached.
#[derive(Copy, Clone, PartialEq)]
pub enum WrapStyle {
    /// Word will break lines by the Unicode line breaking algorithm (Standard Annex #14) This will
    /// generally break lines where you expect them to be broken at and will preserve words.
    Word,
    /// Letter will not preserve words, breaking into a new line after the nearest letter.
    Letter,
}

/// The direction that the Y coordinate increases in. Layout needs to be aware of your coordinate
/// system to place the glyphs correctly.
#[derive(Copy, Clone, PartialEq)]
pub enum CoordinateSystem {
    /// The Y coordinate increases up relative to the window or image. The higher up on the window,
    /// the more positive Y becomes.
    PositiveYUp,
    /// The Y coordinate increases down relative to the window or image. The lower down on the
    /// window, the more positive Y becomes.
    PositiveYDown,
}

/// Settings to configure how text layout is constrained. Text layout is considered best effort and
/// layout may violate the constraints defined here if they prevent text from being laid out.
#[derive(Copy, Clone, PartialEq)]
pub struct LayoutSettings {
    /// The leftmost boundary of the text region.
    pub x: f32,
    /// The topmost boundary of the text region.
    pub y: f32,
    /// An optional rightmost boundary on the text region. A line of text that exceeds the
    /// max_width is wrapped to the line below. If the width of a glyph is larger than the
    /// max_width, the glyph will overflow past the max_width. The application is responsible for
    /// handling the overflow.
    pub max_width: Option<f32>,
    /// An optional bottom boundary on the text region. This is used for positioning the
    /// vertical_align option. Text that exceeds the defined max_height will overflow past it. The
    /// application is responsible for handling the overflow.
    pub max_height: Option<f32>,
    /// The default is Left. This option does nothing if the max_width isn't set.
    pub horizontal_align: HorizontalAlign,
    /// The default is Top. This option does nothing if the max_height isn't set.
    pub vertical_align: VerticalAlign,
    /// The height of each line as a multiplier of the default.
    pub line_height: f32,
    /// The default is Word. Wrap style is a hint for how strings of text should be wrapped to the
    /// next line. Line wrapping can happen when the max width/height is reached.
    pub wrap_style: WrapStyle,
    /// The default is true. This option enables hard breaks, like new line characters, to
    /// prematurely wrap lines. If false, hard breaks will not prematurely create a new line.
    pub wrap_hard_breaks: bool,
}

impl Default for LayoutSettings {
    fn default() -> LayoutSettings {
        LayoutSettings {
            x: 0.0,
            y: 0.0,
            max_width: None,
            max_height: None,
            horizontal_align: HorizontalAlign::Left,
            vertical_align: VerticalAlign::Top,
            line_height: 1.0,
            wrap_style: WrapStyle::Word,
            wrap_hard_breaks: true,
        }
    }
}

/// Configuration for rasterizing a glyph. This struct is also a hashable key that can be used to
/// uniquely identify a rasterized glyph for applications that want to cache glyphs.
#[derive(Debug, Copy, Clone)]
pub struct GlyphRasterConfig {
    /// The glyph index represented by the glyph being positioned.
    pub glyph_index: u16,
    /// The scale of the glyph being positioned in px.
    pub px: f32,
    /// The hash of the font used in layout to raster the glyph.
    pub font_hash: usize,
}

impl Hash for GlyphRasterConfig {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.glyph_index.hash(state);
        self.px.to_bits().hash(state);
        self.font_hash.hash(state);
    }
}

impl PartialEq for GlyphRasterConfig {
    fn eq(&self, other: &Self) -> bool {
        self.glyph_index == other.glyph_index && self.px == other.px && self.font_hash == other.font_hash
    }
}

impl Eq for GlyphRasterConfig {}

/// A positioned scaled glyph.
#[derive(Debug, Copy, Clone)]
pub struct GlyphPosition<U: Copy + Clone = ()> {
    /// Hashable key that can be used to uniquely identify a rasterized glyph.
    pub key: GlyphRasterConfig,
    /// The index of the font used to generate this glyph position.
    pub font_index: usize,
    /// The associated character that generated this glyph. A character may generate multiple
    /// glyphs.
    pub parent: char,
    /// The xmin of the glyph bounding box. This represents the left side of the glyph. Dimensions
    /// are in pixels, and are always whole numbers.
    pub x: f32,
    /// The ymin of the glyph bounding box. If your coordinate system is PositiveYUp, this
    /// represents the bottom side of the glyph. If your coordinate system is PositiveYDown, this
    /// represents the top side of the glyph. This is like this so that (y + height) always produces
    /// the other bound for the glyph.
    pub y: f32,
    /// The width of the glyph. Dimensions are in pixels.
    pub width: usize,
    /// The height of the glyph. Dimensions are in pixels.
    pub height: usize,
    /// The byte offset into the original string used in the append call which created
    /// this glyph.
    pub byte_offset: usize,
    /// Additional metadata associated with the character used to generate this glyph.
    pub char_data: CharacterData,
    /// Custom user data associated with the text styled used to generate this glyph.
    pub user_data: U,
}

/// Errors that can occur while applying GSUB glyph substitutions during layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// The font requires a GSUB lookup type that this engine does not implement. Contains the
    /// OpenType GSUB lookup type number (e.g. 6 for chaining contextual substitution).
    UnsupportedLookup(u16),
    /// Substitutions chained beyond the maximum supported depth. This indicates a recursive
    /// substitution cycle in the font's GSUB table.
    RecursiveSubstitution,
}

impl core::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LayoutError::UnsupportedLookup(kind) => {
                write!(f, "unsupported GSUB lookup type: {}", kind)
            }
            LayoutError::RecursiveSubstitution => {
                write!(f, "recursive GSUB substitution detected")
            }
        }
    }
}

/// A glyph produced by GSUB-aware layout, mapped back to the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutGlyph {
    /// The index of the glyph in the font after all substitutions have been applied.
    pub glyph_index: u16,
    /// The byte range in the original string that this glyph was produced from. Ranges of
    /// consecutive glyphs are continuous and never overlap. Glyphs produced by a one-to-many
    /// (multiple) substitution after the first cover an empty range at the boundary of the
    /// substituted source range.
    pub byte_range: Range<usize>,
}

/// A parsed GSUB lookup in an owned form that the layout engine can apply.
#[derive(Debug, Clone)]
pub(crate) enum GsubLookup {
    /// One-to-one substitution, sorted by input glyph index.
    Single(Vec<(u16, u16)>),
    /// One-to-many substitution, sorted by input glyph index.
    Multiple(Vec<(u16, Vec<u16>)>),
    /// Many-to-one substitution. The input sequence includes the first glyph.
    Ligature(Vec<(Vec<u16>, u16)>),
    /// A lookup type this engine does not implement. Holds the GSUB lookup type number.
    Unsupported(u16),
}

/// The maximum number of times a single glyph may be substituted before the font is considered
/// to contain a recursive substitution cycle.
const MAX_SUBSTITUTION_DEPTH: usize = 8;

/// Maps a string to positioned-independent glyphs, applying the font's GSUB substitutions for
/// the default ligature features. Each resulting glyph carries the byte range of the source
/// text it was produced from. Text that triggers no substitution is mapped one-to-one, with
/// each glyph covering exactly the bytes of its source character.
pub fn layout_gsub(font: &Font, text: &str) -> Result<Vec<LayoutGlyph>, LayoutError> {
    let mut glyphs = Vec::new();
    let mut byte_offset = 0;
    while byte_offset < text.len() {
        let prev_byte_offset = byte_offset;
        let character = read_utf8(text.as_bytes(), &mut byte_offset);
        glyphs.push(LayoutGlyph {
            glyph_index: font.lookup_glyph_index(character),
            byte_range: prev_byte_offset..byte_offset,
        });
    }
    apply_gsub(font.gsub_lookups(), &mut glyphs)?;
    Ok(glyphs)
}

/// Applies GSUB lookups to a sequence of glyphs, merging byte ranges along substitution chains.
/// The byte ranges of the sequence always remain continuous and non-overlapping:
///
/// * Ligatures (many-to-one) cover the union of the consumed glyphs' ranges.
/// * Multiple substitutions (one-to-many) assign the source range to the first produced glyph
///   and empty boundary ranges to the rest.
/// * Single substitutions (one-to-one) keep the source range unchanged.
pub(crate) fn apply_gsub(lookups: &[GsubLookup], glyphs: &mut Vec<LayoutGlyph>) -> Result<(), LayoutError> {
    for lookup in lookups {
        if let GsubLookup::Unsupported(kind) = lookup {
            return Err(LayoutError::UnsupportedLookup(*kind));
        }
    }
    if glyphs.is_empty() {
        return Ok(());
    }

    // Tracks how many times the glyph at each position has been substituted. A glyph that is
    // substituted too many times indicates a substitution cycle in the font.
    let mut depths = vec![0usize; glyphs.len()];
    loop {
        let mut changed = false;
        for lookup in lookups {
            match lookup {
                GsubLookup::Single(map) => {
                    for (i, glyph) in glyphs.iter_mut().enumerate() {
                        if let Ok(p) = map.binary_search_by_key(&glyph.glyph_index, |e| e.0) {
                            if depths[i] >= MAX_SUBSTITUTION_DEPTH {
                                return Err(LayoutError::RecursiveSubstitution);
                            }
                            glyph.glyph_index = map[p].1;
                            depths[i] += 1;
                            changed = true;
                        }
                    }
                }
                GsubLookup::Multiple(map) => {
                    let mut i = 0;
                    while i < glyphs.len() {
                        if let Ok(p) = map.binary_search_by_key(&glyphs[i].glyph_index, |e| e.0) {
                            let substitutes = &map[p].1;
                            if !substitutes.is_empty() {
                                if depths[i] >= MAX_SUBSTITUTION_DEPTH {
                                    return Err(LayoutError::RecursiveSubstitution);
                                }
                                let end = glyphs[i].byte_range.end;
                                let depth = depths[i] + 1;
                                glyphs[i].glyph_index = substitutes[0];
                                depths[i] = depth;
                                for (j, &substitute) in substitutes[1..].iter().enumerate() {
                                    glyphs.insert(
                                        i + 1 + j,
                                        LayoutGlyph {
                                            glyph_index: substitute,
                                            byte_range: end..end,
                                        },
                                    );
                                    depths.insert(i + 1 + j, depth);
                                }
                                changed = true;
                            }
                        }
                        i += 1;
                    }
                }
                GsubLookup::Ligature(rules) => {
                    let mut i = 0;
                    while i < glyphs.len() {
                        // Greedily match the longest ligature starting at this position.
                        let mut best: Option<&(Vec<u16>, u16)> = None;
                        for rule in rules {
                            let sequence = &rule.0;
                            if sequence.len() < 2 || i + sequence.len() > glyphs.len() {
                                continue;
                            }
                            if let Some(b) = best {
                                if sequence.len() <= b.0.len() {
                                    continue;
                                }
                            }
                            if sequence
                                .iter()
                                .enumerate()
                                .all(|(k, &g)| glyphs[i + k].glyph_index == g)
                            {
                                best = Some(rule);
                            }
                        }
                        if let Some(rule) = best {
                            let len = rule.0.len();
                            let depth = depths[i..i + len].iter().copied().max().unwrap_or(0);
                            if depth >= MAX_SUBSTITUTION_DEPTH {
                                return Err(LayoutError::RecursiveSubstitution);
                            }
                            let merged = LayoutGlyph {
                                glyph_index: rule.1,
                                byte_range: glyphs[i].byte_range.start..glyphs[i + len - 1].byte_range.end,
                            };
                            glyphs.splice(i..i + len, core::iter::once(merged));
                            depths.splice(i..i + len, core::iter::once(depth + 1));
                            changed = true;
                        }
                        i += 1;
                    }
                }
                GsubLookup::Unsupported(_) => {}
            }
        }
        if !changed {
            return Ok(());
        }
    }
}

/// A style description for a segment of text.
pub struct TextStyle<'a, U: Copy + Clone = ()> {
    /// The text to layout.
    pub text: &'a str,
    /// The scale of the text in pixel units. The units of the scale are pixels per Em unit.
    pub px: f32,
    /// The font to layout the text in.
    pub font_index: usize,
    /// Additional user data to associate with glyphs produced by this text style.
    pub user_data: U,
}

impl<'a> TextStyle<'a> {
    pub fn new(text: &'a str, px: f32, font_index: usize) -> TextStyle<'a> {
        TextStyle {
            text,
            px,
            font_index,
            user_data: (),
        }
    }
}

impl<'a, U: Copy + Clone> TextStyle<'a, U> {
    pub fn with_user_data(text: &'a str, px: f32, font_index: usize, user_data: U) -> TextStyle<'a, U> {
        TextStyle {
            text,
            px,
            font_index,
            user_data,
        }
    }
}

/// Metrics about a positioned line.
#[derive(Debug, Copy, Clone)]
pub struct LinePosition {
    /// The y coordinate of the baseline of this line, in pixels.
    pub baseline_y: f32,
    /// How much empty space is left at the end of the line before any alignment. If no max width is
    /// specified, f32::MAX is used.
    pub padding: f32,
    /// The highest point that any glyph in the font extends to above the baseline. Typically
    /// positive. If there are multiple styles on this line, this is their max value.
    pub max_ascent: f32,
    /// The lowest point that any glyph in the font extends to below the baseline. Typically
    /// negative. If there are multiple styles on this line, this is their min value.
    pub min_descent: f32,
    /// The gap to leave between the descent of one line and the ascent of the next. This is of
    /// course only a guideline given by the font's designers. If there are multiple styles on this
    /// line, this is their max value.
    pub max_line_gap: f32,
    /// A precalculated value for the of the line depending. It's calculated by: ascent - descent +
    /// line_gap. If there are multiple styles on this line, this is their max value.
    pub max_new_line_size: f32,
    /// The GlyphPosition index of the first glyph in the line.
    pub glyph_start: usize,
    /// The GlyphPosition index of the last glyph in the line.
    pub glyph_end: usize,
    /// The x offset into the first layout pass.
    tracking_x: f32,
}

impl Default for LinePosition {
    fn default() -> Self {
        LinePosition {
            baseline_y: 0.0,
            padding: 0.0,
            max_ascent: 0.0,
            min_descent: 0.0,
            max_line_gap: 0.0,
            max_new_line_size: 0.0,
            glyph_start: 0,
            glyph_end: 0,
            tracking_x: 0.0,
        }
    }
}

/// Text layout requires a small amount of heap usage which is contained in the Layout struct. This
/// context is reused between layout calls. Reusing the Layout struct will greatly reduce memory
/// allocations and is advisable for performance.
pub struct Layout<U: Copy + Clone = ()> {
    /// Marks if layout should be performed as if the Y axis is flipped (Positive Y incrementing
    /// down instead of up).
    flip: bool,
    /// Origin position. Left side of the region text is being laid out in.
    x: f32,
    /// Origin position. Top side of the region text is being laid out in.
    y: f32,
    /// A mask to filter only linebreak types being requested.
    wrap_mask: LinebreakData,
    /// The max width of the region text is being laid out in.
    max_width: f32,
    /// The max height of the region text is being laid out in.
    max_height: f32,
    /// A multiplier for how text fills unused vertical space.
    vertical_align: f32,
    /// A multiplier for how text fills unused horizontal space.
    horizontal_align: f32,
    /// A multiplier for the amount of space between lines.
    line_height: f32,
    /// The current height of all laid out text.
    height: f32,

    /// Finalized glyph state.
    output: Vec<GlyphPosition<U>>,
    /// Intermediate glyph state.
    glyphs: Vec<GlyphPosition<U>>,

    /// Linebreak state. Used to derive linebreaks from past glyphs.
    linebreaker: Linebreaker,
    /// The current highest priority linebreak (Hard > Soft > None).
    linebreak_prev: LinebreakData,
    /// The x position that the glyph that has the current highest priority linebreak status starts
    /// at.
    linebreak_pos: f32,
    /// The index of the glyph that has the current highest priority linebreak status. This glyph is
    /// the last glyph on a line if a linebreak is required.
    linebreak_idx: usize,

    /// Layout state of each line currently laid out. This always has at least 1 element.
    line_metrics: Vec<LinePosition>,
    /// The x position the next glyph starts at.
    current_pos: f32,
    /// The ceil(ascent) of the current style.
    current_ascent: f32,
    /// The ceil(descent) of the current style.
    current_descent: f32,
    /// The ceil(line_gap) of the current style.
    current_line_gap: f32,
    /// The ceil(new_line_size) of the current style.
    current_new_line: f32,
    /// The x position the current line starts at.
    start_pos: f32,

    /// The settings currently being used for layout.
    settings: LayoutSettings,
}

impl<'a, U: Copy + Clone> Layout<U> {
    /// Creates a layout instance. This requires the direction that the Y coordinate increases in.
    /// Layout needs to be aware of your coordinate system to place the glyphs correctly.
    pub fn new(coordinate_system: CoordinateSystem) -> Layout<U> {
        let settings = LayoutSettings::default();

        let mut layout = Layout {
            flip: coordinate_system == CoordinateSystem::PositiveYDown,
            x: 0.0,
            y: 0.0,
            wrap_mask: LINEBREAK_NONE,
            max_width: 0.0,
            max_height: 0.0,
            vertical_align: 0.0,
            horizontal_align: 0.0,
            line_height: 1.0,
            output: Vec::new(),
            glyphs: Vec::new(),
            line_metrics: Vec::new(),
            linebreaker: Linebreaker::new(),
            linebreak_prev: LINEBREAK_NONE,
            linebreak_pos: 0.0,
            linebreak_idx: 0,
            current_pos: 0.0,
            current_ascent: 0.0,
            current_descent: 0.0,
            current_line_gap: 0.0,
            current_new_line: 0.0,
            start_pos: 0.0,
            height: 0.0,
            settings,
        };
        layout.reset(&settings);
        layout
    }

    /// Resets the current layout settings and clears all appended text.
    pub fn reset(&mut self, settings: &LayoutSettings) {
        self.settings = *settings;
        self.x = settings.x;
        self.y = settings.y;
        self.wrap_mask = LinebreakData::from_mask(
            settings.wrap_style == WrapStyle::Word,
            settings.wrap_hard_breaks,
            settings.max_width.is_some(),
        );
        self.max_width = settings.max_width.unwrap_or(f32::MAX);
        self.max_height = settings.max_height.unwrap_or(f32::MAX);
        self.vertical_align = if settings.max_height.is_none() {
            0.0
        } else {
            match settings.vertical_align {
                VerticalAlign::Top => 0.0,
                VerticalAlign::Middle => 0.5,
                VerticalAlign::Bottom => 1.0,
            }
        };
        self.horizontal_align = if settings.max_width.is_none() {
            0.0
        } else {
            match settings.horizontal_align {
                HorizontalAlign::Left => 0.0,
                HorizontalAlign::Center => 0.5,
                HorizontalAlign::Right => 1.0,
            }
        };
        self.line_height = settings.line_height;
        self.clear();
    }

    /// Keeps current layout settings but clears all appended text.
    pub fn clear(&mut self) {
        self.glyphs.clear();
        self.output.clear();
        self.line_metrics.clear();
        self.line_metrics.push(LinePosition::default());

        self.linebreaker.reset();
        self.linebreak_prev = LINEBREAK_NONE;
        self.linebreak_pos = 0.0;
        self.linebreak_idx = 0;
        self.current_pos = 0.0;
        self.current_ascent = 0.0;
        self.current_descent = 0.0;
        self.current_line_gap = 0.0;
        self.current_new_line = 0.0;
        self.start_pos = 0.0;
        self.height = 0.0;
    }

    /// Gets the current height of the appended text.
    pub fn height(&self) -> f32 {
        if let Some(line) = self.line_metrics.last() {
            self.height + line.max_new_line_size
        } else {
            0.0
        }
    }

    /// Gets the currently positioned lines. If there are no lines positioned, this returns none.
    pub fn lines(&'a self) -> Option<&'a Vec<LinePosition>> {
        if self.glyphs.is_empty() {
            None
        } else {
            Some(&self.line_metrics)
        }
    }

    /// Performs layout for text horizontally, and wrapping vertically. This makes a best effort
    /// attempt at laying out the text defined in the given styles with the provided layout
    /// settings. Text may overflow out of the bounds defined in the layout settings and it's up
    /// to the application to decide how to deal with this.
    ///
    /// Characters from the input string can only be omitted from the output, they are never
    /// reordered. The output buffer will always contain characters in the order they were defined
    /// in the styles.
    pub fn append<T: Borrow<Font>>(&mut self, fonts: &[T], style: &TextStyle<U>) {
        // The first layout pass requires some text.
        if style.text.is_empty() {
            return;
        }

        let font: &Font = &fonts[style.font_index].borrow();

        if let Some(metrics) = font.horizontal_line_metrics(style.px) {
            self.current_ascent = ceil(metrics.ascent);
            self.current_new_line = ceil(metrics.new_line_size);
            self.current_descent = ceil(metrics.descent);
            self.current_line_gap = ceil(metrics.line_gap);
            if let Some(line) = self.line_metrics.last_mut() {
                if self.current_ascent > line.max_ascent {
                    line.max_ascent = self.current_ascent;
                }
                if self.current_descent < line.min_descent {
                    line.min_descent = self.current_descent;
                }
                if self.current_line_gap > line.max_line_gap {
                    line.max_line_gap = self.current_line_gap;
                }
                if self.current_new_line > line.max_new_line_size {
                    line.max_new_line_size = self.current_new_line;
                }
            }
        }

        let mut byte_offset = 0;
        while byte_offset < style.text.len() {
            let prev_byte_offset = byte_offset;
            let character = read_utf8(style.text.as_bytes(), &mut byte_offset);
            let linebreak = self.linebreaker.next(character).mask(self.wrap_mask);
            let glyph_index = font.lookup_glyph_index(character);
            let char_data = CharacterData::classify(character, glyph_index);
            let metrics = if !char_data.is_control() {
                font.metrics_indexed(glyph_index, style.px)
            } else {
                Metrics::default()
            };
            let advance = ceil(metrics.advance_width);

            if linebreak >= self.linebreak_prev {
                self.linebreak_prev = linebreak;
                self.linebreak_pos = self.current_pos;
                self.linebreak_idx = self.glyphs.len().saturating_sub(1); // Mark the previous glyph
            }

            // Perform a linebreak
            if linebreak.is_hard() || (self.current_pos - self.start_pos + advance > self.max_width) {
                self.linebreak_prev = LINEBREAK_NONE;
                let mut next_glyph_start = self.glyphs().len();
                if let Some(line) = self.line_metrics.last_mut() {
                    line.glyph_end = self.linebreak_idx;
                    line.padding = self.max_width - (self.linebreak_pos - self.start_pos);
                    self.height += line.max_new_line_size * self.line_height;
                    next_glyph_start = self.linebreak_idx + 1;
                }
                self.line_metrics.push(LinePosition {
                    baseline_y: 0.0,
                    padding: 0.0,
                    max_ascent: self.current_ascent,
                    min_descent: self.current_descent,
                    max_line_gap: self.current_line_gap,
                    max_new_line_size: self.current_new_line,
                    glyph_start: next_glyph_start,
                    glyph_end: 0,
                    tracking_x: self.linebreak_pos,
                });
                self.start_pos = self.linebreak_pos;
            }

            let y = if self.flip {
                floor(-metrics.bounds.height - metrics.bounds.ymin) // PositiveYDown
            } else {
                floor(metrics.bounds.ymin) // PositiveYUp
            };

            self.glyphs.push(GlyphPosition {
                key: GlyphRasterConfig {
                    glyph_index: glyph_index as u16,
                    px: style.px,
                    font_hash: font.file_hash(),
                },
                font_index: style.font_index,
                parent: character,
                byte_offset: prev_byte_offset,
                x: floor(self.current_pos + metrics.bounds.xmin),
                y,
                width: metrics.width,
                height: metrics.height,
                char_data,
                user_data: style.user_data,
            });
            self.current_pos += advance;
        }

        if let Some(line) = self.line_metrics.last_mut() {
            line.padding = self.max_width - (self.current_pos - self.start_pos);
            line.glyph_end = self.glyphs.len().saturating_sub(1);
        }

        self.finalize();
    }

    fn finalize(&mut self) {
        // The second layout pass requires at least 1 glyph to layout.
        if self.glyphs.is_empty() {
            return;
        }

        unsafe { self.output.set_len(0) };
        self.output.reserve(self.glyphs.len());

        let dir = if self.flip {
            -1.0 // PositiveYDown
        } else {
            1.0 // PositiveYUp
        };

        let mut baseline_y = self.y - dir * floor((self.max_height - self.height()) * self.vertical_align);
        let mut idx = 0;
        for line in &mut self.line_metrics {
            let x_padding = self.x - line.tracking_x + floor(line.padding * self.horizontal_align);
            baseline_y -= dir * line.max_ascent;
            line.baseline_y = baseline_y;
            while idx <= line.glyph_end {
                let mut glyph = self.glyphs[idx];
                glyph.x += x_padding;
                glyph.y += baseline_y;
                self.output.push(glyph);
                idx += 1;
            }
            baseline_y -= dir * (line.max_new_line_size * self.line_height - line.max_ascent);
        }
    }

    /// Gets the currently laid out glyphs.
    pub fn glyphs(&'a self) -> &'a Vec<GlyphPosition<U>> {
        &self.output
    }

    /// Gets the settings currently being used for layout.
    pub fn settings(&self) -> &LayoutSettings {
        &self.settings
    }
}

#[cfg(test)]
mod gsub_clusters {
    use super::*;
    use crate::FontSettings;

    fn glyph(index: u16, start: usize, end: usize) -> LayoutGlyph {
        LayoutGlyph {
            glyph_index: index,
            byte_range: start..end,
        }
    }

    /// Asserts the byte ranges are sorted, non-overlapping, and cover `start..end` with no gaps.
    fn assert_continuous_coverage(glyphs: &[LayoutGlyph], start: usize, end: usize) {
        let mut cursor = start;
        for g in glyphs {
            assert!(g.byte_range.start <= g.byte_range.end, "inverted range: {:?}", g);
            assert!(g.byte_range.end >= cursor, "overlap before {:?}", g);
            if g.byte_range.end > cursor {
                assert_eq!(g.byte_range.start, cursor, "gap before {:?}", g);
                cursor = g.byte_range.end;
            } else {
                assert_eq!(g.byte_range.start, g.byte_range.end, "non-empty range regresses: {:?}", g);
            }
        }
        assert_eq!(cursor, end, "ranges do not cover the full text");
    }

    #[test]
    fn no_substitution_leaves_output_unchanged() {
        let input = vec![glyph(10, 0, 1), glyph(11, 1, 2), glyph(12, 2, 4)];

        let mut out = input.clone();
        apply_gsub(&[], &mut out).unwrap();
        assert_eq!(out, input);

        // Lookups that match nothing must also leave every field unchanged.
        let lookups = [
            GsubLookup::Single(vec![(99, 100)]),
            GsubLookup::Multiple(vec![(98, vec![1, 2])]),
            GsubLookup::Ligature(vec![(vec![97, 96], 95)]),
        ];
        let mut out = input.clone();
        apply_gsub(&lookups, &mut out).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn single_substitution_chain_preserves_byte_range() {
        let lookups = [
            GsubLookup::Single(vec![(10, 11)]),
            GsubLookup::Single(vec![(11, 12)]),
        ];
        let mut glyphs = vec![glyph(10, 3, 7), glyph(5, 7, 8)];
        apply_gsub(&lookups, &mut glyphs).unwrap();
        assert_eq!(glyphs, vec![glyph(12, 3, 7), glyph(5, 7, 8)]);
        assert_continuous_coverage(&glyphs, 3, 8);
    }

    #[test]
    fn ligature_merges_byte_ranges() {
        let lookups = [GsubLookup::Ligature(vec![(vec![10, 11, 12], 20)])];
        let mut glyphs = vec![glyph(10, 0, 1), glyph(11, 1, 2), glyph(12, 2, 3), glyph(5, 3, 4)];
        apply_gsub(&lookups, &mut glyphs).unwrap();
        assert_eq!(glyphs, vec![glyph(20, 0, 3), glyph(5, 3, 4)]);
        assert_continuous_coverage(&glyphs, 0, 4);
    }

    #[test]
    fn ligature_prefers_longest_match() {
        let lookups = [GsubLookup::Ligature(vec![
            (vec![10, 11], 20),
            (vec![10, 11, 12], 21),
        ])];
        let mut glyphs = vec![glyph(10, 0, 1), glyph(11, 1, 2), glyph(12, 2, 3)];
        apply_gsub(&lookups, &mut glyphs).unwrap();
        assert_eq!(glyphs, vec![glyph(21, 0, 3)]);
    }

    #[test]
    fn multiple_substitution_keeps_coverage_continuous() {
        let lookups = [GsubLookup::Multiple(vec![(10, vec![30, 31, 32])])];
        let mut glyphs = vec![glyph(10, 0, 2), glyph(5, 2, 3)];
        apply_gsub(&lookups, &mut glyphs).unwrap();
        assert_eq!(
            glyphs,
            vec![glyph(30, 0, 2), glyph(31, 2, 2), glyph(32, 2, 2), glyph(5, 2, 3)]
        );
        assert_continuous_coverage(&glyphs, 0, 3);
    }

    #[test]
    fn chained_multiple_then_ligature_merges_ranges() {
        let lookups = [
            GsubLookup::Multiple(vec![(10, vec![30, 31])]),
            GsubLookup::Ligature(vec![(vec![31, 5], 40)]),
        ];
        let mut glyphs = vec![glyph(10, 0, 2), glyph(5, 2, 3)];
        apply_gsub(&lookups, &mut glyphs).unwrap();
        assert_eq!(glyphs, vec![glyph(30, 0, 2), glyph(40, 2, 3)]);
        assert_continuous_coverage(&glyphs, 0, 3);
    }

    #[test]
    fn unsupported_lookup_error_is_distinct() {
        let lookups = [GsubLookup::Unsupported(6)];
        let mut glyphs = vec![glyph(1, 0, 1)];
        let err = apply_gsub(&lookups, &mut glyphs).unwrap_err();
        assert!(matches!(err, LayoutError::UnsupportedLookup(6)));
        assert!(!matches!(err, LayoutError::RecursiveSubstitution));
    }

    #[test]
    fn recursive_substitution_error_is_distinct() {
        // 1 -> 2 -> 1 -> ... cycles forever without a depth limit.
        let lookups = [
            GsubLookup::Single(vec![(1, 2)]),
            GsubLookup::Single(vec![(2, 1)]),
        ];
        let mut glyphs = vec![glyph(1, 0, 1)];
        let err = apply_gsub(&lookups, &mut glyphs).unwrap_err();
        assert!(matches!(err, LayoutError::RecursiveSubstitution));
        assert!(!matches!(err, LayoutError::UnsupportedLookup(_)));
    }

    #[test]
    fn roboto_ligature_maps_back_to_source_range() {
        let data = include_bytes!("../dev/resources/fonts/Roboto-Regular.ttf");
        let font = Font::from_bytes(data as &[u8], FontSettings::default()).unwrap();

        // "fi" forms a single ligature glyph covering both source bytes.
        let glyphs = layout_gsub(&font, "fi").unwrap();
        assert_eq!(glyphs.len(), 1);
        assert_eq!(glyphs[0].byte_range, 0..2);
        assert_ne!(glyphs[0].glyph_index, font.lookup_glyph_index('f'));
        assert_ne!(glyphs[0].glyph_index, font.lookup_glyph_index('i'));

        // Text without substitutions maps one-to-one, fields unchanged.
        let glyphs = layout_gsub(&font, "ab").unwrap();
        assert_eq!(glyphs.len(), 2);
        assert_eq!(glyphs[0].glyph_index, font.lookup_glyph_index('a'));
        assert_eq!(glyphs[0].byte_range, 0..1);
        assert_eq!(glyphs[1].glyph_index, font.lookup_glyph_index('b'));
        assert_eq!(glyphs[1].byte_range, 1..2);

        // Multibyte characters map to their full UTF-8 byte range.
        let glyphs = layout_gsub(&font, "aé").unwrap();
        assert_eq!(glyphs.len(), 2);
        assert_eq!(glyphs[0].byte_range, 0..1);
        assert_eq!(glyphs[1].byte_range, 1..3);
    }
}
