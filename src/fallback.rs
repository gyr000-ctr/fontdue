//! Cross-font fallback support.
//!
//! The regular layout API ([`crate::layout::Layout::append`]) lays every character of a style
//! out in a single font; characters absent from that font's `cmap` silently fall onto glyph 0
//! (the `.notdef` tofu). The types in this module let an application arrange several fonts into
//! an ordered fallback chain: each character (or grapheme cluster) is measured and positioned
//! using the *first* font in the chain whose `cmap` covers it.
//!
//! The fonts are always borrowed as slices. Nothing in this module copies a font's glyph or
//! cmap tables; each [`crate::Font`] keeps the single parsed representation it already owned.

use crate::unicode::read_utf8;
use crate::Font;
use crate::FontResult;
use alloc::vec::Vec;

/// The outcome of resolving a single character against a fallback chain.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum FallbackStatus {
    /// A font in the chain covers the character. The carried `usize` is that font's index inside
    /// the chain slice. Control characters always resolve against the primary font without a
    /// cmap lookup, matching single-font layout behavior.
    Resolved(usize),
    /// No font in the chain has a mapping for the character. The carried `usize` is the primary
    /// font index, which is what layout falls back to (producing glyph 0 there).
    Missing(usize),
}

impl FallbackStatus {
    /// The font index that was selected. For [`FallbackStatus::Missing`] this is the primary
    /// index, the font whose `.notdef` glyph will be used.
    #[inline]
    pub fn font_index(&self) -> usize {
        match *self {
            FallbackStatus::Resolved(i) | FallbackStatus::Missing(i) => i,
        }
    }

    /// Returns true if every font in the chain lacked a mapping for the character.
    #[inline]
    pub fn is_missing(&self) -> bool {
        matches!(self, FallbackStatus::Missing(_))
    }
}

/// The outcome of resolving a whole grapheme cluster against a fallback chain.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ClusterStatus {
    /// One font covers every visible scalar in the cluster (controls and default-ignorable
    /// characters like ZWJ or variation selectors do not require coverage); the carried value is
    /// its index. Clusters made solely of such invisible scalars resolve against the primary
    /// font.
    Resolved(usize),
    /// No single font in the chain covers every visible scalar of the cluster. The carried value
    /// is the primary font index, which layout places the whole cluster with (uncovered scalars
    /// take glyph 0 there). The per-scalar statuses returned by
    /// `Layout::append_fallback` still report each scalar's own coverage result.
    Missing(usize),
    /// The cluster carried no scalars (an empty input segment), so no font could be selected.
    Empty,
}

impl ClusterStatus {
    /// The selected font index for [`ClusterStatus::Resolved`] / [`ClusterStatus::Missing`], or
    /// `None` for [`ClusterStatus::Empty`].
    #[inline]
    pub fn font_index(&self) -> Option<usize> {
        match *self {
            ClusterStatus::Resolved(i) | ClusterStatus::Missing(i) => Some(i),
            ClusterStatus::Empty => None,
        }
    }

    /// Returns true if the cluster held no scalars.
    #[inline]
    pub fn is_empty(&self) -> bool {
        matches!(self, ClusterStatus::Empty)
    }

    /// Returns true if no single font covers the whole cluster.
    #[inline]
    pub fn is_missing(&self) -> bool {
        matches!(self, ClusterStatus::Missing(_))
    }
}

/// A resolved grapheme cluster: its byte span inside the queried string plus the selection made
/// by the fallback chain.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ClusterResolution {
    /// Inclusive start byte offset of the cluster within the input string.
    pub byte_start: usize,
    /// Exclusive end byte offset of the cluster within the input string.
    pub byte_end: usize,
    /// How the chain resolved the cluster.
    pub status: ClusterStatus,
}

/// Returns true if the font's `cmap` maps the character to a real glyph (a non-zero glyph id).
///
/// This is the primitive coverage query the fallback chain is built on. It only reads the
/// font's existing cmap table and never allocates.
#[inline]
pub fn covers(font: &Font, character: char) -> bool {
    font.has_glyph(character)
}

/// Resolves a single character against an ordered fallback chain.
///
/// `primary` is the index of the style's preferred font. It is consulted first, then the fonts
/// after it in chain order; fonts before it are never considered. The selected position is the
/// first covering font, so glyph metrics always come from that font.
///
/// Control characters short-circuit to the primary font: they carry no visible glyph in
/// single-font layout either, so they must not trigger a font switch.
pub fn resolve_char<T: core::borrow::Borrow<Font>>(
    fonts: &[T],
    primary: usize,
    character: char,
) -> FallbackStatus {
    let primary_font: &Font = fonts[primary].borrow();
    if is_layout_control(character) || primary_font.has_glyph(character) {
        return FallbackStatus::Resolved(primary);
    }
    for (index, candidate) in fonts.iter().enumerate().skip(primary + 1) {
        if candidate.borrow().has_glyph(character) {
            return FallbackStatus::Resolved(index);
        }
    }
    FallbackStatus::Missing(primary)
}

/// Parses each font byte slice into a [`Font`], preserving order, to form an owned fallback
/// chain.
///
/// The error case is deliberately distinct from a missing glyph: parsing is reported through
/// the [`FontResult`] error (a static reason string from the parser), whereas an uncovered
/// character resolves to [`FallbackStatus::Missing`] at layout time. On error no fonts are
/// returned.
pub fn load_font_chain(fonts: &[&[u8]], settings: crate::FontSettings) -> FontResult<Vec<Font>> {
    let mut chain = Vec::with_capacity(fonts.len());
    for data in fonts {
        chain.push(Font::from_bytes(*data, settings)?);
    }
    Ok(chain)
}

// ------------------------------------------------------------------------------------------------
// Grapheme cluster iteration (simplified UAX #29, core-only).
//
// A full UAX #29 implementation ships large Unicode property tables that this no_std crate does
// not carry. This iterator implements the rules that matter for font fallback without any
// allocation: CR/LF handling, control boundaries, spacing-mark clusters (Extend, including
// combining marks and variation selectors) and ZWJ joiner sequences. Scalar values are taken
// straight from the UTF-8 stream, so astral characters are handled correctly.
// ------------------------------------------------------------------------------------------------

/// The control set single-font layout recognizes: ASCII C0 controls and DEL. These carry no
/// visible glyph, so they always resolve against the primary font and never trigger a fallback
/// search, matching the `CharacterData::is_control` classification used by `Layout::append`.
#[inline]
fn is_layout_control(c: char) -> bool {
    matches!(c, '\0'..='\x1F' | '\x7F')
}

/// Default-ignorable characters (Unicode's `Default_Ignorable_Code_Point` reachable without a
/// property table): zero-width joiners/non-joiners, word joiners, the BOM and variation
/// selectors. Fonts normally omit them from the cmap, and they never render a glyph of their
/// own, so cluster coverage queries skip them instead of reporting a false missing.
#[inline]
fn is_default_ignorable(c: char) -> bool {
    let cp = c as u32;
    matches!(cp, 0x200C..=0x200D) // ZWNJ, ZWJ
        || matches!(cp, 0x2028..=0x2029) // line/paragraph separator
        || matches!(cp, 0x2060..=0x2064) // word joiner and invisible operators
        || cp == 0xFEFF // BOM / zero-width no-break space
        || matches!(cp, 0xFE00..=0xFE0F) // variation selectors
        || matches!(cp, 0xE0100..=0xE01EF) // variation selectors supplement
}

/// Hard cluster boundaries for the simplified iterator: layout controls plus the format
/// characters UAX #29 places in its Control class. ZWJ is deliberately excluded: GB9 keeps it
/// inside the cluster, where the Extend rule below handles it.
#[inline]
fn is_cluster_break(c: char) -> bool {
    is_layout_control(c) || is_default_ignorable(c) && c != '\u{200D}'
}

/// UAX #29 `Extend` (approximated without property tables): combining marks, variation selectors
/// and ZWJ. These continue the current grapheme cluster, keeping a mark or joiner attached to
/// its base.
#[inline]
fn is_extend(c: char) -> bool {
    let cp = c as u32;
    matches!(cp, 0x0300..=0x036F) // Combining Diacritical Marks
        || matches!(cp, 0x1AB0..=0x1AFF) // Combining Diacritical Marks Extended
        || matches!(cp, 0x1DC0..=0x1DFF) // Combining Diacritical Marks Supplement
        || matches!(cp, 0x20D0..=0x20FF) // Combining Diacritical Marks for Symbols
        || matches!(cp, 0xFE00..=0xFE0F) // Variation Selectors
        || matches!(cp, 0xFE20..=0xFE2F) // Combining Half Marks
        || matches!(cp, 0xE0100..=0xE01EF) // Variation Selectors Supplement
        || cp == 0x200D // ZWJ (GB11 treats it as an extender for boundary purposes)
}

/// An iterator over the grapheme clusters of a UTF-8 string.
///
/// Each item is `(byte_start, byte_end)`; the cluster slice is `&text[byte_start..byte_end]`.
#[derive(Debug, Clone)]
pub struct GraphemeClusters<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> GraphemeClusters<'a> {
    /// Creates an iterator over the grapheme clusters in `text`.
    pub fn new(text: &'a str) -> GraphemeClusters<'a> {
        GraphemeClusters {
            bytes: text.as_bytes(),
            offset: 0,
        }
    }

    /// Reads one scalar and returns `(char, next_offset)`.
    fn read_at(&self, offset: usize) -> (char, usize) {
        let mut next = offset;
        let c = read_utf8(self.bytes, &mut next);
        (c, next)
    }
}

impl<'a> Iterator for GraphemeClusters<'a> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<(usize, usize)> {
        if self.offset >= self.bytes.len() {
            return None;
        }
        let start = self.offset;
        let (first, mut cursor) = self.read_at(start);
        // GB4: CR × LF stays in one cluster; GB3: CR and LF otherwise each break.
        if first == '\r' && cursor < self.bytes.len() && self.bytes[cursor] == b'\n' {
            let (_, next) = self.read_at(cursor);
            self.offset = next;
            return Some((start, next));
        }
        // GB4/GB5: controls and CR/LF are always one-scalar clusters.
        if is_cluster_break(first) || first == '\r' || first == '\n' {
            self.offset = cursor;
            return Some((start, cursor));
        }
        // GB9/GB11 (simplified): Extend scalars (including combining marks and variation
        // selectors) continue the cluster, and a ZWJ joins the following scalar as well.
        let mut previous_zwj = false;
        while cursor < self.bytes.len() {
            let (c, next) = self.read_at(cursor);
            if is_extend(c) {
                previous_zwj = c == '\u{200D}';
                cursor = next;
                continue;
            }
            if previous_zwj {
                // GB11 pairs ZWJ with an Extended_Pictographic; without property tables, treat
                // any non-control base scalar in this position as joinable.
                previous_zwj = false;
                cursor = next;
                continue;
            }
            break;
        }
        self.offset = cursor;
        Some((start, cursor))
    }
}

/// Resolves a grapheme cluster against the fallback chain.
///
/// A cluster selects one font: the first font (primary first, then the rest of the chain in
/// order) that covers *every* non-control scalar in the cluster. This keeps combining sequences
/// and emoji ZWJ sequences in a single font instead of splitting marks across typefaces. If no
/// single font covers the whole cluster, [`ClusterStatus::Missing`] is reported; an empty
/// cluster reports [`ClusterStatus::Empty`].
pub fn resolve_cluster<T: core::borrow::Borrow<Font>>(
    fonts: &[T],
    primary: usize,
    cluster: &str,
) -> ClusterStatus {
    if cluster.is_empty() {
        return ClusterStatus::Empty;
    }
    // First pass: determine whether the cluster holds any visible scalar. Clusters made solely
    // of controls and default-ignorable characters take the primary font, the same shortcut
    // resolve_char uses.
    let mut rest = cluster.as_bytes();
    let mut has_visible = false;
    while !rest.is_empty() {
        let mut offset = 0;
        let c = read_utf8(rest, &mut offset);
        if !is_layout_control(c) && !is_default_ignorable(c) {
            has_visible = true;
            break;
        }
        rest = &rest[offset..];
    }
    if !has_visible {
        return ClusterStatus::Resolved(primary);
    }
    // Second pass per candidate font, straight over the UTF-8 bytes so no scalar buffer is
    // allocated: the first font covering every visible scalar wins.
    for (index, candidate) in fonts.iter().enumerate().skip(primary) {
        let font: &Font = candidate.borrow();
        let mut rest = cluster.as_bytes();
        let mut covers_all = true;
        while !rest.is_empty() {
            let mut offset = 0;
            let c = read_utf8(rest, &mut offset);
            if !is_layout_control(c) && !is_default_ignorable(c) && !font.has_glyph(c) {
                covers_all = false;
                break;
            }
            rest = &rest[offset..];
        }
        if covers_all {
            return ClusterStatus::Resolved(index);
        }
    }
    ClusterStatus::Missing(primary)
}

/// Iterates every grapheme cluster in `text` and resolves each against the fallback chain.
///
/// Empty input yields an empty vec, which is how an "empty cluster" condition is observable
/// alongside the per-cluster [`ClusterStatus::Empty`].
pub fn resolve_clusters<T: core::borrow::Borrow<Font>>(
    fonts: &[T],
    primary: usize,
    text: &str,
) -> Vec<ClusterResolution> {
    GraphemeClusters::new(text)
        .map(|(start, end)| ClusterResolution {
            byte_start: start,
            byte_end: end,
            status: resolve_cluster(fonts, primary, &text[start..end]),
        })
        .collect()
}

#[cfg(test)]
mod font_fallback_tests {
    use super::*;
    use crate::layout::{CoordinateSystem, Layout, TextStyle};
    use crate::{Font, FontSettings};
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    // ---------------------------------------------------------------------------------------------
    // Minimal synthetic TrueType font builder.
    //
    // Real font files are large and ship only in the dev crate; the library's own unit tests must
    // stay self-contained. The builder below emits the smallest sfnt ttf-parser accepts: head,
    // hhea, maxp, hmtx plus a format-4 cmap with exactly the requested cmap coverage. Glyphs have
    // empty outlines (zero bounds) and caller-chosen advances, which is enough to exercise cmap
    // lookup and metrics-driven layout deterministically.
    // ---------------------------------------------------------------------------------------------

    struct Table {
        tag: [u8; 4],
        data: Vec<u8>,
    }

    fn push_u16(buf: &mut Vec<u8>, v: u16) {
        buf.extend_from_slice(&v.to_be_bytes());
    }

    fn push_i16(buf: &mut Vec<u8>, v: i16) {
        buf.extend_from_slice(&v.to_be_bytes());
    }

    fn push_u32(buf: &mut Vec<u8>, v: u32) {
        buf.extend_from_slice(&v.to_be_bytes());
    }

    /// Builds a font whose cmap maps exactly `codepoints` (plus implicit glyph 0). Each mapped
    /// codepoint gets its own glyph id starting at 1, with horizontal advance `advance` font
    /// units. `ascent`/`descent`/`line_gap` drive the line metrics so mixed-font lines can be
    /// asserted.
    fn synth_font(codepoints: &[char], advance: u16, ascent: i16, descent: i16, line_gap: i16) -> Vec<u8> {
        let glyph_count = (codepoints.len() + 1) as u16;

        // head (54 bytes): indexToLocationFormat = 0 (short), no glyf/loca tables are emitted.
        let mut head = Vec::with_capacity(54);
        push_u32(&mut head, 0x00010000); // version
        push_u32(&mut head, 0x00010000); // font revision
        push_u32(&mut head, 0); // checksum adjustment
        push_u32(&mut head, 0x5F0F3CF5); // magic number
        push_u16(&mut head, 0x000B); // flags
        push_u16(&mut head, 1000); // units per em
        head.extend_from_slice(&[0u8; 16]); // created / modified
        push_i16(&mut head, 0); // xMin
        push_i16(&mut head, 0); // yMin
        push_i16(&mut head, 0); // xMax
        push_i16(&mut head, 0); // yMax
        push_u16(&mut head, 0); // mac style
        push_u16(&mut head, 8); // lowest rec PPEM
        push_i16(&mut head, 2); // font direction hint
        push_i16(&mut head, 0); // indexToLocationFormat: short
        while head.len() < 54 {
            head.push(0);
        }

        // hhea (36 bytes): one long horizontal metric.
        let mut hhea = Vec::with_capacity(36);
        push_u32(&mut hhea, 0x00010000); // version
        push_i16(&mut hhea, ascent);
        push_i16(&mut hhea, descent);
        push_i16(&mut hhea, line_gap);
        push_u16(&mut hhea, advance); // advance width max
        push_i16(&mut hhea, 0); // min left side bearing
        push_i16(&mut hhea, 0); // min right side bearing
        push_i16(&mut hhea, 0); // x max extent
        push_i16(&mut hhea, 1); // caret slope rise
        push_i16(&mut hhea, 0); // caret slope run
        push_i16(&mut hhea, 0); // caret offset
        for _ in 0..4 {
            push_i16(&mut hhea, 0); // reserved
        }
        push_i16(&mut hhea, 0); // metric data format
        push_u16(&mut hhea, glyph_count); // number of hmetrics
        while hhea.len() < 36 {
            hhea.push(0);
        }

        // maxp (version 0.5, 6 bytes).
        let mut maxp = Vec::with_capacity(6);
        push_u32(&mut maxp, 0x00005000);
        push_u16(&mut maxp, glyph_count);

        // hmtx: every glyph (including .notdef) is a long metric with the same advance.
        let mut hmtx = Vec::with_capacity(glyph_count as usize * 4);
        for _ in 0..glyph_count {
            push_u16(&mut hmtx, advance);
            push_i16(&mut hmtx, 0); // left side bearing
        }

        // cmap: one platform-3/encoding-1 record pointing at a format-4 subtable.
        let mut sorted: Vec<char> = codepoints.to_vec();
        sorted.sort();
        // Format-4 segments are [start, end] ranges with a terminating 0xFFFF segment.
        let mut starts: Vec<u16> = Vec::new();
        let mut ends: Vec<u16> = Vec::new();
        let mut i = 0;
        while i < sorted.len() {
            let start = sorted[i] as u16;
            let mut end = start;
            while i + 1 < sorted.len() && sorted[i + 1] as u16 == end + 1 {
                end += 1;
                i += 1;
            }
            starts.push(start);
            ends.push(end);
            i += 1;
        }
        starts.push(0xFFFF);
        ends.push(0xFFFF);
        let seg_count = starts.len() as u16;

        let mut subtable = Vec::new();
        let length_pos;
        push_u16(&mut subtable, 4); // format
        length_pos = subtable.len();
        push_u16(&mut subtable, 0); // length placeholder
        push_u16(&mut subtable, 0); // language
        push_u16(&mut subtable, seg_count * 2); // segCountX2
        let mut search_range = 2;
        while search_range * 2 <= seg_count {
            search_range *= 2;
        }
        push_u16(&mut subtable, search_range * 2);
        let mut entry_selector = 0u16;
        let mut sr = search_range;
        while sr > 1 {
            entry_selector += 1;
            sr /= 2;
        }
        push_u16(&mut subtable, entry_selector);
        push_u16(&mut subtable, seg_count * 2 - search_range * 2); // rangeShift
        for e in &ends {
            push_u16(&mut subtable, *e);
        }
        push_u16(&mut subtable, 0); // reserved pad
        for s in &starts {
            push_u16(&mut subtable, *s);
        }
        // idDelta: glyph ids are assigned in ascending codepoint order starting at 1, so the delta
        // for a segment is (first_glyph_id - segment_start) mod 2^16.
        let mut glyph_id = 1u16;
        let mut seg = 0;
        i = 0;
        while i < sorted.len() {
            let seg_start = starts[seg];
            let seg_end = ends[seg];
            let delta = glyph_id.wrapping_sub(seg_start);
            push_i16(&mut subtable, delta as i16);
            glyph_id += seg_end - seg_start + 1;
            i += (seg_end - seg_start + 1) as usize;
            seg += 1;
        }
        push_i16(&mut subtable, 1); // terminating segment delta (arbitrary)
        for _ in 0..seg_count {
            push_u16(&mut subtable, 0); // idRangeOffset = 0 -> delta mapping
        }
        let sub_len = subtable.len() as u16;
        subtable[length_pos..length_pos + 2].copy_from_slice(&sub_len.to_be_bytes());

        let mut cmap = Vec::new();
        push_u16(&mut cmap, 0); // version
        push_u16(&mut cmap, 1); // num subtables
        push_u16(&mut cmap, 3); // platform ID: Windows
        push_u16(&mut cmap, 1); // encoding ID: Unicode BMP
        push_u32(&mut cmap, 12); // offset to subtable
        cmap.extend_from_slice(&subtable);

        let tables = vec![
            Table {
                tag: *b"cmap",
                data: cmap,
            },
            Table {
                tag: *b"head",
                data: head,
            },
            Table {
                tag: *b"hhea",
                data: hhea,
            },
            Table {
                tag: *b"hmtx",
                data: hmtx,
            },
            Table {
                tag: *b"maxp",
                data: maxp,
            },
        ];

        // sfnt wrapper with a searchable table directory.
        let mut out = Vec::new();
        push_u32(&mut out, 0x00010000); // sfnt version
        push_u16(&mut out, tables.len() as u16);
        let mut search_range = 16u16;
        let mut entry_selector = 0u16;
        while search_range * 2 <= tables.len() as u16 * 16 {
            search_range *= 2;
            entry_selector += 1;
        }
        push_u16(&mut out, search_range);
        push_u16(&mut out, entry_selector);
        push_u16(&mut out, tables.len() as u16 * 16 - search_range);

        let data_offset = 12 + tables.len() * 16;
        let mut offsets = Vec::with_capacity(tables.len());
        let mut cursor = data_offset;
        for t in &tables {
            offsets.push(cursor as u32);
            cursor += t.data.len();
            if cursor % 4 != 0 {
                cursor += 4 - cursor % 4; // 4-byte aligned table data
            }
        }
        for (idx, t) in tables.iter().enumerate() {
            out.extend_from_slice(&t.tag);
            push_u32(&mut out, 0); // checksum (not validated by the parser path used here)
            push_u32(&mut out, offsets[idx]);
            push_u32(&mut out, t.data.len() as u32);
        }
        for (idx, t) in tables.iter().enumerate() {
            while out.len() < offsets[idx] as usize {
                out.push(0);
            }
            out.extend_from_slice(&t.data);
            let _ = idx;
        }
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out
    }

    fn parse(bytes: &[u8]) -> Font {
        Font::from_bytes(bytes, FontSettings::default()).expect("synthetic font must parse")
    }

    // Two non-overlapping coverage sets with deliberately different typography.
    const LATIN: &[char] = &['a', 'b', 'c', 'A', ' '];
    const GREEK: &[char] = &['α', 'β', 'γ'];
    const CJK: &[char] = &['中', '文'];

    fn latin_font() -> Font {
        // Narrow advance, shallow ascent.
        parse(&synth_font(LATIN, 500, 800, -200, 0))
    }

    fn greek_font() -> Font {
        // Wide advance, tall ascent so line-metric mixing is observable.
        parse(&synth_font(GREEK, 900, 1200, -400, 100))
    }

    fn cjk_font() -> Font {
        parse(&synth_font(CJK, 1000, 1000, -250, 0))
    }

    #[test]
    fn font_fallback_synthetic_fonts_parse_with_requested_coverage() {
        let latin = latin_font();
        for c in LATIN {
            assert!(latin.has_glyph(*c), "latin font must cover {}", c);
            assert!(crate::fallback::covers(&latin, *c));
        }
        for c in GREEK.iter().chain(CJK.iter()) {
            assert!(!latin.has_glyph(*c), "latin font must not cover {}", c);
        }
        let greek = greek_font();
        for c in GREEK {
            assert!(greek.has_glyph(*c));
        }
        for c in LATIN.iter().chain(CJK.iter()) {
            assert!(!greek.has_glyph(*c));
        }
        // Glyph 0 is never a successful coverage result.
        assert_eq!(latin.lookup_glyph_index('α'), 0);
        assert_ne!(latin.lookup_glyph_index('a'), 0);
    }

    #[test]
    fn font_fallback_covers_is_cmap_only_and_allocates_nothing() {
        let latin = latin_font();
        assert!(crate::fallback::covers(&latin, 'a'));
        assert!(!crate::fallback::covers(&latin, '中'));
        // The coverage query reads the existing cmap; units per em come straight from head.
        assert_eq!(latin.units_per_em() as u16, 1000);
    }

    #[test]
    fn font_fallback_resolve_char_prefers_primary_then_chain_order() {
        let fonts = [latin_font(), greek_font(), cjk_font()];

        // Primary covers it: no switch.
        assert_eq!(resolve_char(&fonts, 0, 'a'), FallbackStatus::Resolved(0));
        // Primary misses, second font covers: first covering font wins.
        assert_eq!(resolve_char(&fonts, 0, 'α'), FallbackStatus::Resolved(1));
        assert_eq!(resolve_char(&fonts, 0, '中'), FallbackStatus::Resolved(2));
        // A font earlier in the chain than the primary is never consulted.
        assert_eq!(resolve_char(&fonts, 1, 'a'), FallbackStatus::Missing(1));
        // No font covers it: distinguishable missing status carrying the primary index.
        assert_eq!(resolve_char(&fonts, 0, '日'), FallbackStatus::Missing(0));
        assert!(resolve_char(&fonts, 0, '日').is_missing());
        // Control characters short-circuit to the primary font.
        assert_eq!(resolve_char(&fonts, 1, '\n'), FallbackStatus::Resolved(1));
        assert_eq!(resolve_char(&fonts, 2, '\t'), FallbackStatus::Resolved(2));
    }

    #[test]
    fn font_fallback_layout_switches_fonts_per_cluster_and_uses_covering_metrics() {
        let fonts = [latin_font(), greek_font()];
        let mut layout = Layout::new(CoordinateSystem::PositiveYUp);
        let style = TextStyle::new("aαbβ", 20.0, 0);
        let statuses = layout.append_fallback(&fonts, &style);

        assert_eq!(
            statuses,
            vec![
                FallbackStatus::Resolved(0),
                FallbackStatus::Resolved(1),
                FallbackStatus::Resolved(0),
                FallbackStatus::Resolved(1),
            ]
        );

        let glyphs = layout.glyphs();
        assert_eq!(glyphs.len(), 4); // 4 scalars
        let font_indices: Vec<usize> = glyphs.iter().map(|g| g.font_index).collect();
        assert_eq!(font_indices, vec![0, 1, 0, 1]);

        // Raster keys point at the covering font's hash and that font's local glyph id.
        assert_eq!(glyphs[0].key.font_hash, fonts[0].file_hash());
        assert_eq!(glyphs[1].key.font_hash, fonts[1].file_hash());
        assert_ne!(glyphs[1].key.glyph_index, 0);
        assert_eq!(glyphs[1].key.glyph_index, fonts[1].lookup_glyph_index('α') as u16);

        // Positions follow the *covering* font's advance:
        // 'a' advances ceil(500 * 20/1000) = 10px, then 'α' advances ceil(900 * 20/1000) = 18px.
        let advance_a = fonts[0].metrics('a', 20.0).advance_width.ceil();
        let advance_alpha = fonts[1].metrics('α', 20.0).advance_width.ceil();
        assert_eq!(advance_a, 10.0);
        assert_eq!(advance_alpha, 18.0);
        let x_alpha = glyphs[1].x;
        let x_b = glyphs[2].x;
        assert_eq!((x_b - x_alpha) as i32, advance_alpha as i32);
        let x_beta = glyphs[3].x;
        assert_eq!((x_beta - x_b) as i32, advance_a as i32);
    }

    #[test]
    fn font_fallback_single_font_layout_is_bit_identical_to_append() {
        let fonts = [latin_font()];
        let text = "a b A c";
        let px = 17.0;

        let mut expected = Layout::new(CoordinateSystem::PositiveYUp);
        expected.append(&fonts, &TextStyle::new(text, px, 0));
        let mut actual = Layout::new(CoordinateSystem::PositiveYUp);
        let statuses = actual.append_fallback(&fonts, &TextStyle::new(text, px, 0));

        // Every scalar resolves to the sole font.
        assert!(statuses.iter().all(|s| *s == FallbackStatus::Resolved(0)));
        assert_eq!(statuses.len(), text.chars().count());

        let a = expected.glyphs();
        let b = actual.glyphs();
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.font_index, y.font_index);
            assert_eq!(x.parent, y.parent);
            assert_eq!(x.byte_offset, y.byte_offset);
            assert_eq!(x.x, y.x);
            assert_eq!(x.y, y.y);
            assert_eq!(x.width, y.width);
            assert_eq!(x.height, y.height);
            assert_eq!(x.key.glyph_index, y.key.glyph_index);
            assert_eq!(x.key.font_hash, y.key.font_hash);
            assert_eq!(x.key.px, y.key.px);
            assert_eq!(x.char_data, y.char_data);
        }
        assert_eq!(expected.height(), actual.height());

        // And identical under the flipped coordinate system.
        let mut e2 = Layout::new(CoordinateSystem::PositiveYDown);
        e2.append(&fonts, &TextStyle::new(text, px, 0));
        let mut a2 = Layout::new(CoordinateSystem::PositiveYDown);
        a2.append_fallback(&fonts, &TextStyle::new(text, px, 0));
        for (x, y) in e2.glyphs().iter().zip(a2.glyphs().iter()) {
            assert_eq!(x.x, y.x);
            assert_eq!(x.y, y.y);
        }
    }

    #[test]
    fn font_fallback_missing_characters_are_distinguishable_and_keep_notdef_behavior() {
        let fonts = [latin_font(), greek_font()];
        let mut layout = Layout::new(CoordinateSystem::PositiveYUp);
        let statuses = layout.append_fallback(&fonts, &TextStyle::new("a日α", 16.0, 0));

        assert_eq!(
            statuses,
            vec![FallbackStatus::Resolved(0), FallbackStatus::Missing(0), FallbackStatus::Resolved(1),]
        );
        let glyphs = layout.glyphs();
        // The missing scalar stays on the primary font and resolves to glyph 0, like append().
        let missing = &glyphs[1];
        assert_eq!(missing.font_index, 0);
        assert_eq!(missing.key.font_hash, fonts[0].file_hash());
        assert_eq!(missing.key.glyph_index, 0);
        assert!(missing.char_data.is_missing());

        // The missing scalar matches what the primary font alone produces for glyph 0: the
        // fallback path must not invent metrics for a character no font covers.
        let notdef_metrics = fonts[0].metrics_indexed(0, 16.0);
        assert_eq!(missing.width, notdef_metrics.width);
        assert_eq!(missing.height, notdef_metrics.height);
        // And its raster key is exactly what a single-font layout would emit for it.
        let mut legacy = Layout::new(CoordinateSystem::PositiveYUp);
        legacy.append(&fonts, &TextStyle::new("日", 16.0, 0));
        let legacy_missing = &legacy.glyphs()[0];
        assert_eq!(legacy_missing.key.glyph_index, 0);
        assert_eq!(legacy_missing.key.font_hash, missing.key.font_hash);
        assert_eq!(legacy_missing.key.px, missing.key.px);
        assert_eq!(legacy_missing.char_data, missing.char_data);
    }

    #[test]
    fn font_fallback_all_missing_is_separable_from_parse_failure() {
        // Parse failure surfaces as an Err with a reason string, not a layout-time Missing.
        let junk = vec![0u8, 1, 2, 3, 4, 5, 6, 7];
        let parse_err = Font::from_bytes(&junk[..], FontSettings::default());
        assert!(parse_err.is_err());
        let chain_err = crate::fallback::load_font_chain(&[&junk[..]], FontSettings::default());
        assert!(chain_err.is_err());
        // A valid chain whose coverage misses the whole text reports Missing per scalar instead.
        let fonts = [latin_font()];
        let statuses = {
            let mut layout = Layout::new(CoordinateSystem::PositiveYUp);
            layout.append_fallback(&fonts, &TextStyle::new("日文", 12.0, 0))
        };
        assert_eq!(statuses, vec![FallbackStatus::Missing(0), FallbackStatus::Missing(0)]);
    }

    #[test]
    fn font_fallback_empty_input_and_empty_clusters_are_distinguishable() {
        let fonts = [latin_font(), greek_font()];
        let mut layout = Layout::new(CoordinateSystem::PositiveYUp);

        // Empty style text: no glyphs, empty status vec (nothing to resolve).
        let empty_statuses = layout.append_fallback(&fonts, &TextStyle::new("", 12.0, 0));
        assert!(empty_statuses.is_empty());
        assert!(layout.glyphs().is_empty());
        assert!(layout.lines().is_none());

        // Cluster-level resolution distinguishes an empty cluster explicitly.
        assert_eq!(resolve_cluster(&fonts, 0, ""), ClusterStatus::Empty);
        assert!(resolve_cluster(&fonts, 0, "").is_empty());

        // resolve_clusters over empty text yields nothing; over real text one entry per cluster.
        assert!(resolve_clusters(&fonts, 0, "").is_empty());
        let resolutions = resolve_clusters(&fonts, 0, "a α");
        assert_eq!(resolutions.len(), 3);
        assert_eq!(resolutions[0].status, ClusterStatus::Resolved(0));
    }

    #[test]
    fn font_fallback_layout_switches_fonts_per_whole_grapheme_cluster() {
        // Font 0 covers the base 'a' but not the combining acute; font 1 covers the acute but
        // not the base. The cluster must still be placed with ONE font (the base's), proving
        // layout switches per cluster rather than splitting mark and base across typefaces.
        let base_only = parse(&synth_font(&['a'], 500, 800, -200, 0));
        let mark_only = parse(&synth_font(&['\u{0301}'], 500, 800, -200, 0));
        let fonts = [base_only, mark_only];

        let mut layout = Layout::new(CoordinateSystem::PositiveYUp);
        let statuses = layout.append_fallback(&fonts, &TextStyle::new("a\u{0301}", 16.0, 0));
        // Per-scalar status stays truthful: base covered by font 0, mark by font 1.
        assert_eq!(statuses, vec![FallbackStatus::Resolved(0), FallbackStatus::Resolved(1)]);

        let glyphs = layout.glyphs();
        assert_eq!(glyphs.len(), 2);
        // But both glyph positions are placed with the font covering the whole cluster's base:
        // font 0, i.e. the cluster-level selection. The mark never jumps to font 1's metrics.
        assert_eq!(glyphs[0].font_index, 0);
        assert_eq!(glyphs[1].font_index, 0);
        assert_eq!(glyphs[0].key.font_hash, fonts[0].file_hash());
        assert_eq!(glyphs[1].key.font_hash, fonts[0].file_hash());
        // The mark has no glyph in font 0, so it renders as glyph 0 there, matching append().
        assert_eq!(glyphs[1].key.glyph_index, 0);

        // When a single font DOES cover both scalars, the whole cluster selects that font and
        // both glyphs resolve to real (non-zero) indices.
        let mut both: Vec<char> = vec!['a'];
        both.push('\u{0301}');
        let covering = parse(&synth_font(&both, 500, 800, -200, 0));
        let mark_only_2 = parse(&synth_font(&['\u{0301}'], 500, 800, -200, 0));
        let fonts = [covering, mark_only_2];
        let mut layout2 = Layout::new(CoordinateSystem::PositiveYUp);
        layout2.append_fallback(&fonts, &TextStyle::new("a\u{0301}", 16.0, 0));
        let g2 = layout2.glyphs();
        assert_eq!(g2[0].font_index, 0);
        assert_eq!(g2[1].font_index, 0);
        assert_ne!(g2[1].key.glyph_index, 0);
        assert_eq!(g2[1].key.glyph_index, fonts[0].lookup_glyph_index('\u{0301}') as u16);
    }

    #[test]
    fn font_fallback_grapheme_clusters_keep_marks_with_their_base() {
        // 'a' plus a combining acute: the cluster must resolve as one unit against one font.
        let cluster = "a\u{0301}";
        let fonts = [latin_font(), greek_font()];
        // Neither font maps the combining mark, so no single font covers the whole cluster.
        let status = resolve_cluster(&fonts, 0, cluster);
        assert_eq!(status, ClusterStatus::Missing(0));

        // Teach the latin font the combining mark by rebuilding coverage.
        let mut coverage: Vec<char> = LATIN.to_vec();
        coverage.push('\u{0301}');
        let extended = parse(&synth_font(&coverage, 500, 800, -200, 0));
        let fonts = [extended, greek_font()];
        assert_eq!(resolve_cluster(&fonts, 0, cluster), ClusterStatus::Resolved(0));

        // The iterator keeps the two scalars in a single cluster and reports correct byte spans.
        let clusters: Vec<(usize, usize)> = GraphemeClusters::new(cluster).collect();
        assert_eq!(clusters, vec![(0, cluster.len())]);
        let multi = GraphemeClusters::new("a α").collect::<Vec<_>>();
        assert_eq!(multi, vec![(0, 1), (1, 2), (2, 4)]);
    }

    #[test]
    fn font_fallback_control_and_format_chars_follow_layout_rules() {
        let fonts = [latin_font(), greek_font(), cjk_font()];

        // ASCII controls short-circuit to the primary font (they have no visible glyph).
        assert_eq!(resolve_char(&fonts, 1, '\n'), FallbackStatus::Resolved(1));
        assert_eq!(resolve_char(&fonts, 2, '\t'), FallbackStatus::Resolved(2));
        assert_eq!(resolve_char(&fonts, 0, '\u{7F}'), FallbackStatus::Resolved(0));

        // A format character (ZWJ) is not a layout control: absent from every cmap it resolves
        // as Missing rather than being silently pinned to the primary font.
        assert_eq!(resolve_char(&fonts, 0, '\u{200D}'), FallbackStatus::Missing(0));

        // Invisible format characters inside a cluster do not block coverage of the visible base.
        let zwj_seq = "a\u{200D}b";
        assert_eq!(resolve_cluster(&fonts, 0, zwj_seq), ClusterStatus::Resolved(0));
        // The iterator keeps a ZWJ joiner sequence in one cluster.
        assert_eq!(GraphemeClusters::new(zwj_seq).collect::<Vec<_>>(), vec![(0, zwj_seq.len())]);

        // CRLF stays in one cluster; lone CR and LF each break.
        assert_eq!(GraphemeClusters::new("\r\n").collect::<Vec<_>>(), vec![(0, 2)]);
        assert_eq!(GraphemeClusters::new("\r\n\r").collect::<Vec<_>>(), vec![(0, 2), (2, 3)]);

        // Variation selectors are default-ignorable: a base alone keeps its coverage.
        let vs_seq = "a\u{FE0F}";
        assert_eq!(resolve_cluster(&fonts, 0, vs_seq), ClusterStatus::Resolved(0));
        assert_eq!(GraphemeClusters::new(vs_seq).collect::<Vec<_>>(), vec![(0, vs_seq.len())]);

        // ZWNJ breaks the cluster but is ignored when the two visible halves are covered.
        let zwnj = "a\u{200C}b";
        assert_eq!(GraphemeClusters::new(zwnj).collect::<Vec<_>>(), vec![(0, 1), (1, 4), (4, 5)]);
        assert_eq!(resolve_cluster(&fonts, 0, zwnj), ClusterStatus::Resolved(0));

        // A cluster made only of invisible characters still resolves to the primary font.
        assert_eq!(resolve_cluster(&fonts, 1, "\u{200D}\u{FE0F}"), ClusterStatus::Resolved(1));

        // But a default-ignorable character between two fonts' coverage still picks one font
        // covering every visible scalar: 'a' is latin-only, 'α' is greek-only, so no one font
        // covers the whole cluster.
        assert_eq!(resolve_cluster(&fonts, 0, "a\u{200D}α"), ClusterStatus::Missing(0));
    }

    #[test]
    fn font_fallback_line_metrics_mix_takes_maxima_across_fonts() {
        let fonts = [latin_font(), greek_font()];
        let mut layout = Layout::new(CoordinateSystem::PositiveYUp);
        layout.append_fallback(&fonts, &TextStyle::new("aα", 20.0, 0));

        let lines = layout.lines().expect("one line");
        assert_eq!(lines.len(), 1);
        let line = lines[0];
        let greek = fonts[1].horizontal_line_metrics(20.0).unwrap();
        let latin = fonts[0].horizontal_line_metrics(20.0).unwrap();
        // Max ascent / new-line size and min descent across the fonts used on the line.
        assert_eq!(line.max_ascent, greek.ascent.ceil().max(latin.ascent.ceil()));
        assert_eq!(line.min_descent, greek.descent.ceil().min(latin.descent.ceil()));
        assert_eq!(line.max_new_line_size, greek.new_line_size.ceil().max(latin.new_line_size.ceil()));
        assert!(line.max_ascent >= latin.ascent.ceil());
    }

    #[test]
    fn font_fallback_wraps_mixed_font_text_on_covering_advances() {
        use crate::layout::{LayoutSettings, WrapStyle};

        let fonts = [latin_font(), greek_font()];
        // Latin advances 10px, greek 18px at px=20. A 32px region wraps after "aα" (28px) since
        // adding another 10px latin glyph would reach 38px.
        let settings = LayoutSettings {
            max_width: Some(32.0),
            wrap_style: WrapStyle::Letter,
            ..LayoutSettings::default()
        };
        let mut layout = Layout::new(CoordinateSystem::PositiveYUp);
        layout.reset(&settings);
        let statuses = layout.append_fallback(&fonts, &TextStyle::new("aαbβ", 20.0, 0));
        assert_eq!(statuses.len(), 4);

        let lines = layout.lines().expect("lines present");
        assert_eq!(lines.len(), 2, "mixed-font text must wrap into two lines");
        let glyphs = layout.glyphs();
        // First line holds "aα" (indices 0..2), the second line "bβ".
        assert_eq!(lines[0].glyph_start, 0);
        assert_eq!(lines[0].glyph_end, 1);
        assert_eq!(lines[1].glyph_start, 2);
        assert_eq!(lines[1].glyph_end, 3);
        // The second line starts back at the region's x origin (padding aside), not at 28px.
        assert!(glyphs[2].x < glyphs[1].x);
        // Font assignments survive the wrap.
        assert_eq!(glyphs[0].font_index, 0);
        assert_eq!(glyphs[1].font_index, 1);
        assert_eq!(glyphs[2].font_index, 0);
        assert_eq!(glyphs[3].font_index, 1);
    }

    #[test]
    fn font_fallback_does_not_copy_font_tables() {
        // The chain stores Fonts by value once; layout takes borrows and never clones.
        let chain = crate::fallback::load_font_chain(
            &[&synth_font(LATIN, 500, 800, -200, 0)[..], &synth_font(GREEK, 900, 1200, -400, 100)[..]],
            FontSettings::default(),
        )
        .expect("both synthetic fonts parse");
        assert_eq!(chain.len(), 2);

        let refs: Vec<&Font> = chain.iter().collect();
        let mut layout = Layout::new(CoordinateSystem::PositiveYUp);
        let statuses = layout.append_fallback(&refs, &TextStyle::new("aα", 14.0, 0));
        assert_eq!(statuses.len(), 2);
        // Reusing the same borrowed chain for another append still works (state lives in Layout).
        let mut layout2 = Layout::new(CoordinateSystem::PositiveYUp);
        let again = layout2.append_fallback(&refs, &TextStyle::new("βb中", 14.0, 0));
        assert_eq!(
            again,
            vec![FallbackStatus::Resolved(1), FallbackStatus::Resolved(0), FallbackStatus::Missing(0),]
        );
    }

    #[test]
    fn font_fallback_chain_order_and_primary_index() {
        let fonts = [latin_font(), greek_font(), cjk_font()];
        // Starting the chain at index 2 means latin/greek are unreachable even if they cover.
        assert_eq!(resolve_char(&fonts, 2, 'a'), FallbackStatus::Missing(2));
        assert_eq!(resolve_char(&fonts, 2, '中'), FallbackStatus::Resolved(2));

        let resolutions = resolve_clusters(&fonts, 1, "α中a");
        assert_eq!(resolutions.len(), 3);
        assert_eq!(resolutions[0].status, ClusterStatus::Resolved(1));
        assert_eq!(resolutions[1].status, ClusterStatus::Resolved(2));
        assert_eq!(resolutions[2].status, ClusterStatus::Missing(1));
    }

    #[test]
    fn font_fallback_byte_offsets_and_order_are_preserved() {
        let fonts = [latin_font(), greek_font(), cjk_font()];
        let mut layout = Layout::new(CoordinateSystem::PositiveYUp);
        let text = "a中α";
        layout.append_fallback(&fonts, &TextStyle::new(text, 13.0, 0));
        let glyphs = layout.glyphs();
        let parents: String = glyphs.iter().map(|g| g.parent).collect();
        assert_eq!(parents, text);
        // Byte offsets point back into the original UTF-8 string, astral/multibyte aware.
        assert_eq!(glyphs[0].byte_offset, 0);
        assert_eq!(&text[glyphs[1].byte_offset..glyphs[1].byte_offset + 3], "中");
        assert_eq!(glyphs[2].byte_offset, 4);
        // Monotonic non-decreasing x positions.
        for w in glyphs.windows(2) {
            assert!(w[1].x >= w[0].x);
        }
    }
}
