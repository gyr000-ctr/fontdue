use crate::table::parse::*;
use alloc::vec::Vec;

// Microsoft: https://learn.microsoft.com/en-us/typography/opentype/spec/gpos
// Only the pieces needed for horizontal kerning are implemented: the 'kern'
// feature, Pair Adjustment Positioning (lookup type 2) subtables in formats 1
// and 2, and the Coverage and ClassDef tables they reference.

/// The result of a GPOS kerning lookup. The non-value variants distinguish why
/// a lookup did not produce a kerning value.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum KernLookup {
    /// A kerning value was found for the pair.
    Value(i16),
    /// The font has kerning data, but no value for this pair.
    NoKerning,
    /// Kerning data is present, but only in subtable formats that aren't
    /// supported.
    UnsupportedFormat,
    /// A glyph index is out of bounds for the font.
    GlyphOutOfBounds,
}

/// Internal per-subtable result.
enum PairResult {
    Value(i16),
    Miss,
    Unsupported,
}

/// Reads a value from a fallible parse, mapping truncated or malformed data to
/// `PairResult::Unsupported`.
macro_rules! read {
    ($e:expr) => {
        match $e {
            Some(value) => value,
            None => return PairResult::Unsupported,
        }
    };
}

/// A parsed GPOS table. Only the table structure (feature and lookup lists) is
/// parsed up front; pair data is resolved per query so the table is never
/// expanded in full.
#[derive(Clone)]
pub struct TableGpos {
    data: Vec<u8>,
    /// Absolute offsets of supported PairPos subtables within `data`.
    pair_pos: Vec<u32>,
    /// Kerning data was found, but some of it used an unsupported lookup type
    /// or subtable format.
    unsupported: bool,
}

impl TableGpos {
    pub fn new(gpos: &[u8]) -> Option<TableGpos> {
        let mut stream = Stream::new(gpos);
        let major = stream.read_u16()?;
        stream.skip(2); // minor
        if major != 1 {
            return None;
        }
        stream.skip(2); // scriptListOffset
        let feature_list = usize::from(stream.read_u16()?);
        let lookup_list = usize::from(stream.read_u16()?);

        // Collect the lookup indices referenced by 'kern' features.
        let mut lookup_indices: Vec<u16> = Vec::new();
        let mut stream = Stream::new(gpos);
        stream.seek(feature_list);
        let feature_count = stream.read_u16()?;
        for _ in 0..feature_count {
            let tag = stream.read_tag()?;
            let offset = usize::from(stream.read_u16()?);
            if &tag != b"kern" {
                continue;
            }
            let mut feature = Stream::new(gpos);
            feature.seek(feature_list + offset);
            feature.skip(2); // featureParams
            let index_count = feature.read_u16()?;
            for _ in 0..index_count {
                lookup_indices.push(feature.read_u16()?);
            }
        }
        if lookup_indices.is_empty() {
            return None; // Font.gpos: No kern feature available.
        }

        // Collect the offsets of supported PairPos subtables.
        let mut pair_pos = Vec::new();
        let mut unsupported = false;
        for index in lookup_indices {
            let mut stream = Stream::new(gpos);
            stream.seek(lookup_list);
            let lookup_count = stream.read_u16()?;
            if index >= lookup_count {
                unsupported = true;
                continue;
            }
            stream.seek(lookup_list + 2 + usize::from(index) * 2);
            let lookup_offset = usize::from(stream.read_u16()?);
            let mut lookup = Stream::new(gpos);
            lookup.seek(lookup_list + lookup_offset);
            let lookup_type = lookup.read_u16()?;
            lookup.skip(2); // lookupFlag
            let sub_table_count = lookup.read_u16()?;
            if lookup_type != 2 {
                // Not a Pair Adjustment Positioning lookup.
                unsupported = true;
                continue;
            }
            for _ in 0..sub_table_count {
                let sub_table_offset = usize::from(lookup.read_u16()?);
                let absolute = lookup_list + lookup_offset + sub_table_offset;
                let mut sub_table = Stream::new(gpos);
                sub_table.seek(absolute);
                match sub_table.read_u16() {
                    Some(1) | Some(2) => pair_pos.push(absolute as u32),
                    _ => unsupported = true,
                }
            }
        }
        if pair_pos.is_empty() && !unsupported {
            return None; // Font.gpos: No supported sub-table format available.
        }

        Some(TableGpos {
            data: gpos.to_vec(),
            pair_pos,
            unsupported,
        })
    }

    /// Looks up the horizontal kerning value for a pair of glyph indices.
    pub fn kern(&self, left: u16, right: u16, glyph_count: u16) -> KernLookup {
        if left >= glyph_count || right >= glyph_count {
            return KernLookup::GlyphOutOfBounds;
        }
        let mut unsupported = self.unsupported;
        for &offset in &self.pair_pos {
            match self.kern_pair_pos(offset as usize, left, right) {
                PairResult::Value(value) => return KernLookup::Value(value),
                PairResult::Miss => {}
                PairResult::Unsupported => unsupported = true,
            }
        }
        if unsupported {
            KernLookup::UnsupportedFormat
        } else {
            KernLookup::NoKerning
        }
    }

    fn kern_pair_pos(&self, offset: usize, left: u16, right: u16) -> PairResult {
        let data = &self.data[..];
        let mut stream = Stream::new(data);
        stream.seek(offset);
        let format = read!(stream.read_u16());
        let coverage_offset = usize::from(read!(stream.read_u16()));
        let value_format1 = read!(stream.read_u16());
        let value_format2 = read!(stream.read_u16());
        match format {
            // Adjusts pairs of individual glyphs.
            1 => {
                let pair_set_count = read!(stream.read_u16());
                // The coverage index of the left glyph selects its pair set.
                let coverage_index = match coverage_index(data, offset + coverage_offset, left) {
                    Some(index) => index,
                    None => return PairResult::Miss,
                };
                if coverage_index >= pair_set_count {
                    return PairResult::Unsupported;
                }
                stream.seek(offset + 10 + usize::from(coverage_index) * 2);
                let pair_set_offset = usize::from(read!(stream.read_u16()));
                let mut pair_set = Stream::new(data);
                pair_set.seek(offset + pair_set_offset);
                let pair_value_count = read!(pair_set.read_u16());
                let record_size = 2 + value_record_size(value_format1) + value_record_size(value_format2);
                for _ in 0..pair_value_count {
                    let second_glyph = read!(pair_set.read_u16());
                    let record_offset = pair_set.offset();
                    pair_set.skip(record_size - 2);
                    if second_glyph == right {
                        return match x_advance(data, record_offset, value_format1) {
                            Some(value) => PairResult::Value(value),
                            None => PairResult::Miss,
                        };
                    }
                }
                PairResult::Miss
            }
            // Adjusts pairs of glyph classes.
            2 => {
                let class_def1_offset = usize::from(read!(stream.read_u16()));
                let class_def2_offset = usize::from(read!(stream.read_u16()));
                let class1_count = read!(stream.read_u16());
                let class2_count = read!(stream.read_u16());
                if coverage_index(data, offset + coverage_offset, left).is_none() {
                    return PairResult::Miss;
                }
                let class1 = class_def(data, offset + class_def1_offset, left);
                let class2 = class_def(data, offset + class_def2_offset, right);
                if class1 >= class1_count || class2 >= class2_count {
                    return PairResult::Miss;
                }
                let record_size = value_record_size(value_format1) + value_record_size(value_format2);
                let record_offset = offset
                    + 16
                    + (usize::from(class1) * usize::from(class2_count) + usize::from(class2)) * record_size;
                match x_advance(data, record_offset, value_format1) {
                    Some(value) => PairResult::Value(value),
                    None => PairResult::Miss,
                }
            }
            _ => PairResult::Unsupported,
        }
    }
}

/// Reads the coverage index of a glyph in a Coverage table, if it's covered.
fn coverage_index(data: &[u8], offset: usize, glyph: u16) -> Option<u16> {
    let mut stream = Stream::new(data);
    stream.seek(offset);
    match stream.read_u16()? {
        // Individual glyph indices.
        1 => {
            let glyph_count = stream.read_u16()?;
            for index in 0..glyph_count {
                if stream.read_u16()? == glyph {
                    return Some(index);
                }
            }
            None
        }
        // Ranges of glyph indices.
        2 => {
            let range_count = stream.read_u16()?;
            for _ in 0..range_count {
                let start = stream.read_u16()?;
                let end = stream.read_u16()?;
                let start_coverage_index = stream.read_u16()?;
                if glyph >= start && glyph <= end {
                    return Some(start_coverage_index + (glyph - start));
                }
            }
            None
        }
        _ => None,
    }
}

/// Reads the class of a glyph in a ClassDef table. Glyphs not assigned to a
/// class are in class 0.
fn class_def(data: &[u8], offset: usize, glyph: u16) -> u16 {
    let mut stream = Stream::new(data);
    stream.seek(offset);
    match stream.read_u16() {
        // Classes for a contiguous range of glyphs.
        Some(1) => {
            let start_glyph = match stream.read_u16() {
                Some(start_glyph) => start_glyph,
                None => return 0,
            };
            let glyph_count = match stream.read_u16() {
                Some(glyph_count) => glyph_count,
                None => return 0,
            };
            if glyph < start_glyph || glyph >= start_glyph.saturating_add(glyph_count) {
                return 0;
            }
            stream.skip(usize::from(glyph - start_glyph) * 2);
            stream.read_u16().unwrap_or(0)
        }
        // Classes for ranges of glyphs.
        Some(2) => {
            let range_count = match stream.read_u16() {
                Some(range_count) => range_count,
                None => return 0,
            };
            for _ in 0..range_count {
                let start = match stream.read_u16() {
                    Some(start) => start,
                    None => return 0,
                };
                let end = match stream.read_u16() {
                    Some(end) => end,
                    None => return 0,
                };
                let class = match stream.read_u16() {
                    Some(class) => class,
                    None => return 0,
                };
                if glyph >= start && glyph <= end {
                    return class;
                }
            }
            0
        }
        _ => 0,
    }
}

/// The size of a ValueRecord in bytes. Each bit in the value format is one
/// u16 field.
fn value_record_size(value_format: u16) -> usize {
    value_format.count_ones() as usize * 2
}

/// Reads the xAdvance field of a ValueRecord, if the value format has one.
fn x_advance(data: &[u8], offset: usize, value_format: u16) -> Option<i16> {
    const X_PLACEMENT: u16 = 0x0001;
    const Y_PLACEMENT: u16 = 0x0002;
    const X_ADVANCE: u16 = 0x0004;
    if value_format & X_ADVANCE == 0 {
        return None;
    }
    let mut offset = offset;
    if value_format & X_PLACEMENT != 0 {
        offset += 2;
    }
    if value_format & Y_PLACEMENT != 0 {
        offset += 2;
    }
    let mut stream = Stream::new(data);
    stream.seek(offset);
    stream.read_i16()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be16(v: &mut Vec<u8>, x: u16) {
        v.extend_from_slice(&x.to_be_bytes());
    }

    fn patch16(v: &mut [u8], offset: usize, x: u16) {
        v[offset..offset + 2].copy_from_slice(&x.to_be_bytes());
    }

    /// Builds a minimal GPOS table (v1.0, empty script list) from a set of
    /// features and lookups.
    fn build_gpos(features: &[([u8; 4], Vec<u16>)], lookups: &[(u16, Vec<Vec<u8>>)]) -> Vec<u8> {
        let mut out = Vec::new();
        be16(&mut out, 1); // major
        be16(&mut out, 0); // minor
        be16(&mut out, 10); // scriptListOffset
        let feature_list_offset = 12u16;
        be16(&mut out, feature_list_offset);
        let lookup_list_offset = feature_list_offset
            + 2
            + 6 * features.len() as u16
            + features.iter().map(|f| 4 + 2 * f.1.len() as u16).sum::<u16>();
        be16(&mut out, lookup_list_offset);
        // ScriptList: no scripts.
        be16(&mut out, 0);
        // FeatureList.
        be16(&mut out, features.len() as u16);
        let mut offset = 2 + 6 * features.len() as u16;
        for (tag, indices) in features {
            out.extend_from_slice(tag);
            be16(&mut out, offset);
            offset += 4 + 2 * indices.len() as u16;
        }
        for (_, indices) in features {
            be16(&mut out, 0); // featureParams
            be16(&mut out, indices.len() as u16);
            for index in indices {
                be16(&mut out, *index);
            }
        }
        // LookupList.
        be16(&mut out, lookups.len() as u16);
        let mut offset = 2 + 2 * lookups.len() as u16;
        for (_, sub_tables) in lookups {
            be16(&mut out, offset);
            offset += 6 + 2 * sub_tables.len() as u16 + sub_tables.iter().map(|s| s.len() as u16).sum::<u16>();
        }
        for (lookup_type, sub_tables) in lookups {
            be16(&mut out, *lookup_type);
            be16(&mut out, 0); // lookupFlag
            be16(&mut out, sub_tables.len() as u16);
            let mut sub_offset = 6 + 2 * sub_tables.len() as u16;
            for sub_table in sub_tables {
                be16(&mut out, sub_offset);
                sub_offset += sub_table.len() as u16;
            }
            for sub_table in sub_tables {
                out.extend_from_slice(sub_table);
            }
        }
        out
    }

    /// Builds a PairPos format 1 subtable. Each pair set is a list of
    /// (second glyph, value1 fields, value2 fields).
    fn pair_pos_format1(
        coverage: &[u16],
        value_format1: u16,
        value_format2: u16,
        pair_sets: &[Vec<(u16, Vec<i16>, Vec<i16>)>],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        be16(&mut out, 1); // posFormat
        be16(&mut out, 0); // coverageOffset, patched later
        be16(&mut out, value_format1);
        be16(&mut out, value_format2);
        be16(&mut out, pair_sets.len() as u16);
        let offsets_at = out.len();
        for _ in pair_sets {
            be16(&mut out, 0); // pairSetOffsets, patched later
        }
        let mut offsets = Vec::new();
        for pair_set in pair_sets {
            offsets.push(out.len());
            be16(&mut out, pair_set.len() as u16);
            for (second, value1, value2) in pair_set {
                be16(&mut out, *second);
                for field in value1 {
                    be16(&mut out, *field as u16);
                }
                for field in value2 {
                    be16(&mut out, *field as u16);
                }
            }
        }
        let coverage_offset = out.len();
        be16(&mut out, 1); // coverage format 1
        be16(&mut out, coverage.len() as u16);
        for glyph in coverage {
            be16(&mut out, *glyph);
        }
        patch16(&mut out, 2, coverage_offset as u16);
        for (index, offset) in offsets.iter().enumerate() {
            patch16(&mut out, offsets_at + index * 2, *offset as u16);
        }
        out
    }

    /// Builds a PairPos format 2 subtable from raw Coverage and ClassDef
    /// tables. `records` holds class1Count * class2Count records, each the
    /// concatenation of the value1 and value2 fields.
    fn pair_pos_format2(
        coverage: &[u8],
        class_def1: &[u8],
        class_def2: &[u8],
        value_format1: u16,
        value_format2: u16,
        class1_count: u16,
        class2_count: u16,
        records: &[Vec<i16>],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        be16(&mut out, 2); // posFormat
        be16(&mut out, 0); // coverageOffset, patched later
        be16(&mut out, value_format1);
        be16(&mut out, value_format2);
        be16(&mut out, 0); // classDef1Offset, patched later
        be16(&mut out, 0); // classDef2Offset, patched later
        be16(&mut out, class1_count);
        be16(&mut out, class2_count);
        for record in records {
            for field in record {
                be16(&mut out, *field as u16);
            }
        }
        let coverage_offset = out.len();
        out.extend_from_slice(coverage);
        let class_def1_offset = out.len();
        out.extend_from_slice(class_def1);
        let class_def2_offset = out.len();
        out.extend_from_slice(class_def2);
        patch16(&mut out, 2, coverage_offset as u16);
        patch16(&mut out, 8, class_def1_offset as u16);
        patch16(&mut out, 10, class_def2_offset as u16);
        out
    }

    fn coverage_format2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        be16(&mut out, 2);
        be16(&mut out, ranges.len() as u16);
        for (start, end, index) in ranges {
            be16(&mut out, *start);
            be16(&mut out, *end);
            be16(&mut out, *index);
        }
        out
    }

    fn class_def_format1(start_glyph: u16, classes: &[u16]) -> Vec<u8> {
        let mut out = Vec::new();
        be16(&mut out, 1);
        be16(&mut out, start_glyph);
        be16(&mut out, classes.len() as u16);
        for class in classes {
            be16(&mut out, *class);
        }
        out
    }

    fn class_def_format2(ranges: &[(u16, u16, u16)]) -> Vec<u8> {
        let mut out = Vec::new();
        be16(&mut out, 2);
        be16(&mut out, ranges.len() as u16);
        for (start, end, class) in ranges {
            be16(&mut out, *start);
            be16(&mut out, *end);
            be16(&mut out, *class);
        }
        out
    }

    fn gpos_with_lookups(lookups: &[(u16, Vec<Vec<u8>>)]) -> Vec<u8> {
        let indices = (0..lookups.len() as u16).collect();
        build_gpos(&[(*b"kern", indices)], lookups)
    }

    #[test]
    fn gpos_kerning_pairpos_format1() {
        // Coverage format 1, xAdvance-only value records, and a nonzero
        // valueFormat2 to exercise record striding.
        let sub_table = pair_pos_format1(
            &[1],
            0x0004,
            0x0004,
            &[vec![(5, vec![-40], vec![3]), (7, vec![25], vec![-2])]],
        );
        let gpos = TableGpos::new(&gpos_with_lookups(&[(2, vec![sub_table])])).unwrap();
        assert_eq!(gpos.kern(1, 5, 10), KernLookup::Value(-40));
        assert_eq!(gpos.kern(1, 7, 10), KernLookup::Value(25));
        // Right glyph not in the pair set.
        assert_eq!(gpos.kern(1, 9, 10), KernLookup::NoKerning);
        // Left glyph not covered.
        assert_eq!(gpos.kern(2, 5, 10), KernLookup::NoKerning);
        // Reversed pair isn't kerned.
        assert_eq!(gpos.kern(5, 1, 10), KernLookup::NoKerning);
        // Glyph indices out of bounds are distinguishable from a miss.
        assert_eq!(gpos.kern(10, 5, 10), KernLookup::GlyphOutOfBounds);
        assert_eq!(gpos.kern(1, 10, 10), KernLookup::GlyphOutOfBounds);
    }

    #[test]
    fn gpos_kerning_pairpos_format2_classdef() {
        // Coverage format 2, ClassDef format 1 for the left glyphs, ClassDef
        // format 2 for the right glyphs. valueFormat1 has xPlacement before
        // xAdvance to exercise field offsets.
        let coverage = coverage_format2(&[(2, 4, 0)]);
        let class_def1 = class_def_format1(2, &[1, 0, 1]);
        let class_def2 = class_def_format2(&[(5, 6, 1), (8, 8, 2)]);
        // valueFormat1 = xPlacement | xAdvance, 2x2 classes with a third
        // (out of range) class2 unused.
        let records = vec![
            vec![0, 5],   // class1 0, class2 0
            vec![0, 0],   // class1 0, class2 1
            vec![0, 0],   // class1 0, class2 2
            vec![0, 0],   // class1 1, class2 0
            vec![7, -30], // class1 1, class2 1
            vec![0, 0],   // class1 1, class2 2
        ];
        let sub_table = pair_pos_format2(&coverage, &class_def1, &class_def2, 0x0005, 0, 2, 3, &records);
        let gpos = TableGpos::new(&gpos_with_lookups(&[(2, vec![sub_table])])).unwrap();
        // Glyph 2 is class 1, glyph 5 is class 1: xAdvance is the second field.
        assert_eq!(gpos.kern(2, 5, 10), KernLookup::Value(-30));
        // Glyph 3 is class 0, glyph 3 (unassigned) is class 0.
        assert_eq!(gpos.kern(3, 3, 10), KernLookup::Value(5));
        // Class pairs with a zero record resolve to a zero value.
        assert_eq!(gpos.kern(4, 8, 10), KernLookup::Value(0));
        // Left glyph not covered.
        assert_eq!(gpos.kern(9, 5, 10), KernLookup::NoKerning);
        assert_eq!(gpos.kern(2, 5, 3), KernLookup::GlyphOutOfBounds);
    }

    #[test]
    fn gpos_kerning_unsupported_format() {
        // PairPos format 3 doesn't exist; it must be reported as unsupported
        // rather than as a miss.
        let mut sub_table = pair_pos_format1(&[1], 0x0004, 0, &[vec![(5, vec![-40], vec![])]]);
        patch16(&mut sub_table, 0, 3);
        let gpos = TableGpos::new(&gpos_with_lookups(&[(2, vec![sub_table])])).unwrap();
        assert_eq!(gpos.kern(1, 5, 10), KernLookup::UnsupportedFormat);

        // A lookup type other than PairPos is unsupported.
        let sub_table = pair_pos_format1(&[1], 0x0004, 0, &[vec![(5, vec![-40], vec![])]]);
        let gpos = TableGpos::new(&gpos_with_lookups(&[(9, vec![sub_table])])).unwrap();
        assert_eq!(gpos.kern(1, 5, 10), KernLookup::UnsupportedFormat);

        // Truncated data is unsupported, not a miss.
        let sub_table = pair_pos_format1(&[1], 0x0004, 0, &[vec![(5, vec![-40], vec![])]]);
        let truncated = sub_table[..6].to_vec();
        let gpos = TableGpos::new(&gpos_with_lookups(&[(2, vec![truncated])])).unwrap();
        assert_eq!(gpos.kern(1, 5, 10), KernLookup::UnsupportedFormat);
    }

    #[test]
    fn gpos_kerning_no_kern_feature() {
        // A GPOS table without a 'kern' feature yields no table.
        let sub_table = pair_pos_format1(&[1], 0x0004, 0, &[vec![(5, vec![-40], vec![])]]);
        assert!(TableGpos::new(&build_gpos(&[(*b"mark", vec![0])], &[(2, vec![sub_table])])).is_none());
        // Garbage input yields no table.
        assert!(TableGpos::new(&[0, 1, 0]).is_none());
    }

    #[test]
    fn gpos_kerning_value_format_without_xadvance() {
        // A pair record without an xAdvance field is no horizontal kerning.
        let sub_table = pair_pos_format1(&[1], 0x0001, 0, &[vec![(5, vec![100], vec![])]]);
        let gpos = TableGpos::new(&gpos_with_lookups(&[(2, vec![sub_table])])).unwrap();
        assert_eq!(gpos.kern(1, 5, 10), KernLookup::NoKerning);
    }
}
