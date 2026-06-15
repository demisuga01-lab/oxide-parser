use std::cmp::Ordering;

use unicode_bidi::{BidiInfo, Level};

use super::collector::{is_rtl_char, is_rtl_dominant, TextChunk};

#[derive(Debug, Clone)]
pub struct TextLine {
    /// Full text of the line (chunks joined with appropriate spacing).
    pub text: String,

    /// Y coordinate of the line's baseline in user space (points).
    /// This is the representative y of all chunks on the line.
    /// PDF y increases upward, so higher y means higher on the page.
    pub y: f64,

    /// X coordinate of the leftmost chunk on this line.
    pub x_min: f64,

    /// X coordinate of the right edge of the rightmost chunk (x + width).
    pub x_max: f64,

    /// Representative font size for this line (median of all chunk font sizes).
    pub font_size: f64,

    /// True when this line starts a new paragraph.
    pub is_paragraph_break: bool,

    /// Column index this line belongs to (0 = left or single column, 1 = right).
    pub column: usize,
}

impl TextLine {
    /// True if the line contains only whitespace or is empty.
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// True if the line appears to be a heading.
    pub fn is_heading(&self, avg_font_size: f64) -> bool {
        self.font_size > avg_font_size * 1.2
    }
}

#[derive(Debug, Clone)]
pub struct ReadingOrderReconstructor {
    /// Chunks whose y values differ by less than this factor x font_size are
    /// considered part of the same line.
    pub line_y_tolerance_factor: f64,

    /// A same-line gap greater than this factor x font_size inserts a space.
    pub word_gap_factor: f64,

    /// A same-line gap greater than this factor x font_size inserts two spaces.
    pub wide_gap_factor: f64,

    /// Minimum gap width between x-clusters before considering two columns.
    pub column_gap_min_points: f64,

    /// Whether to attempt multi-column detection.
    pub detect_columns: bool,

    /// Vertical gap factor for paragraph detection.
    pub paragraph_gap_factor: f64,

    /// Whether to join likely hyphenated line breaks.
    pub join_hyphens: bool,
}

impl Default for ReadingOrderReconstructor {
    fn default() -> Self {
        Self {
            line_y_tolerance_factor: 0.5,
            word_gap_factor: 0.25,
            wide_gap_factor: 2.0,
            column_gap_min_points: 40.0,
            detect_columns: true,
            paragraph_gap_factor: 1.5,
            join_hyphens: false,
        }
    }
}

impl ReadingOrderReconstructor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Convert a flat list of TextChunks into ordered TextLines.
    pub fn reconstruct(&self, chunks: Vec<TextChunk>) -> Vec<TextLine> {
        if chunks.is_empty() {
            return vec![];
        }

        // Partition by writing mode (driven by each chunk's font WMode, set in the
        // collector). Vertical and horizontal runs are reconstructed by their own
        // logic, so a page that mixes vertical body text with horizontal page
        // numbers/headings handles each run correctly. The horizontal path is
        // unchanged from the LTR/RTL implementation (zero behaviour change for
        // documents with no vertical text). When both are present, vertical
        // columns are emitted first, then horizontal lines.
        let (vertical, horizontal): (Vec<TextChunk>, Vec<TextChunk>) =
            chunks.into_iter().partition(|c| c.is_vertical);

        let mut result_lines: Vec<TextLine> = Vec::new();
        if !vertical.is_empty() {
            result_lines.extend(self.reconstruct_vertical(vertical));
        }
        if !horizontal.is_empty() {
            result_lines.extend(self.reconstruct_horizontal(horizontal));
        }
        result_lines
    }

    /// Reconstruct horizontal (LTR/RTL) text — the original reading-order pass.
    fn reconstruct_horizontal(&self, chunks: Vec<TextChunk>) -> Vec<TextLine> {
        let raw_lines = self.group_into_lines(chunks);

        let (col0_lines, col1_lines) = if self.detect_columns {
            self.split_columns(raw_lines)
        } else {
            (raw_lines, vec![])
        };

        let mut result_lines: Vec<TextLine> = Vec::new();
        for (col_idx, col_lines) in [col0_lines, col1_lines].iter().enumerate() {
            let mut col_text_lines = self.build_text_lines(col_lines, col_idx);
            result_lines.append(&mut col_text_lines);
        }

        result_lines.sort_by(|a, b| {
            a.column
                .cmp(&b.column)
                .then(b.y.partial_cmp(&a.y).unwrap_or(Ordering::Equal))
        });

        self.mark_paragraph_breaks(&mut result_lines);
        if self.join_hyphens {
            self.join_hyphenated_lines(&mut result_lines);
        }

        result_lines
    }

    /// Reconstruct vertical (CJK top-to-bottom) writing mode.
    ///
    /// Vertical text reads top-to-bottom within a column, and columns are read
    /// right-to-left (the rightmost column first). This is the vertical analog of
    /// the horizontal line/column clustering: chunks are grouped into columns by
    /// x-proximity, glyphs within a column are ordered by descending y (top to
    /// bottom), and the columns themselves are ordered by descending x (right to
    /// left). Each column becomes one `TextLine`.
    ///
    /// BiDi reordering is intentionally bypassed: vertical CJK is not RTL-script
    /// in the UAX#9 sense, and its right-to-left ordering is a column-layout
    /// property handled here, not a character-level bidi property.
    fn reconstruct_vertical(&self, chunks: Vec<TextChunk>) -> Vec<TextLine> {
        let columns = self.group_into_columns(chunks);

        columns
            .into_iter()
            .enumerate()
            .map(|(col_idx, group)| {
                let repr_y = group.iter().map(|c| c.y).fold(f64::NEG_INFINITY, f64::max);
                let repr_y = if repr_y.is_finite() { repr_y } else { 0.0 };

                let mut sizes: Vec<f64> = group.iter().map(|c| c.font_size).collect();
                sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
                let font_size = sizes[sizes.len() / 2].max(1.0);

                let x_min = group.iter().map(|c| c.x).fold(f64::INFINITY, f64::min);
                let x_min = if x_min.is_finite() { x_min } else { 0.0 };
                let x_max = group
                    .iter()
                    .map(|c| c.x + c.font_size)
                    .fold(f64::NEG_INFINITY, f64::max);
                let x_max = if x_max.is_finite() { x_max } else { 0.0 };

                // Glyphs top-to-bottom: in PDF user space y increases upward, so a
                // higher y is higher on the page and comes first.
                let mut ordered = group;
                ordered.sort_by(|a, b| b.y.partial_cmp(&a.y).unwrap_or(Ordering::Equal));

                let mut text = String::new();
                for (i, chunk) in ordered.iter().enumerate() {
                    if i > 0 {
                        let prev = &ordered[i - 1];
                        // prev sits above chunk; the vertical gap is prev.y minus
                        // chunk.y minus prev's glyph height (≈ its font size).
                        let gap = prev.y - chunk.y - prev.font_size;
                        let ref_size = prev.font_size.min(chunk.font_size).max(1.0);
                        if gap > ref_size * self.wide_gap_factor {
                            text.push(' ');
                        }
                    }
                    text.push_str(&chunk.text);
                }

                TextLine {
                    text,
                    y: repr_y,
                    x_min,
                    x_max,
                    font_size,
                    is_paragraph_break: false,
                    column: col_idx,
                }
            })
            .collect()
    }

    /// Group vertical-text chunks into columns by x-proximity, ordering the
    /// columns right-to-left (rightmost first). Mirrors `group_into_lines` but on
    /// the x axis: two chunks belong to the same column when their x origins are
    /// within a font-size-scaled tolerance.
    fn group_into_columns(&self, mut chunks: Vec<TextChunk>) -> Vec<Vec<TextChunk>> {
        // Process rightmost-first so column ordering is right-to-left.
        chunks.sort_by(|a, b| b.x.partial_cmp(&a.x).unwrap_or(Ordering::Equal));

        let mut groups: Vec<(f64, Vec<TextChunk>)> = Vec::new();

        for chunk in chunks {
            let tolerance = (chunk.font_size.max(12.0) * self.line_y_tolerance_factor).max(1.0);

            let best = groups
                .iter_mut()
                .filter(|(repr_x, _)| (chunk.x - *repr_x).abs() <= tolerance)
                .min_by(|(a, _), (b, _)| {
                    (chunk.x - *a)
                        .abs()
                        .partial_cmp(&(chunk.x - *b).abs())
                        .unwrap_or(Ordering::Equal)
                });

            match best {
                Some((repr_x, group)) => {
                    group.push(chunk);
                    *repr_x = group.iter().map(|c| c.x).sum::<f64>() / group.len() as f64;
                }
                None => {
                    groups.push((chunk.x, vec![chunk]));
                }
            }
        }

        // Order columns right-to-left by representative x.
        groups.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
        groups.into_iter().map(|(_, chunks)| chunks).collect()
    }

    /// Group chunks into raw lines using y-proximity clustering.
    pub fn group_into_lines(&self, mut chunks: Vec<TextChunk>) -> Vec<Vec<TextChunk>> {
        chunks.sort_by(|a, b| b.y.partial_cmp(&a.y).unwrap_or(Ordering::Equal));

        let mut groups: Vec<(f64, Vec<TextChunk>)> = Vec::new();

        for chunk in chunks {
            let tolerance = (chunk.font_size.max(12.0) * self.line_y_tolerance_factor).max(1.0);

            let best = groups
                .iter_mut()
                .enumerate()
                .filter(|(_, (repr_y, _))| (chunk.y - *repr_y).abs() <= tolerance)
                .min_by(|(_, (a, _)), (_, (b, _))| {
                    (chunk.y - *a)
                        .abs()
                        .partial_cmp(&(chunk.y - *b).abs())
                        .unwrap_or(Ordering::Equal)
                });

            match best {
                Some((_, (repr_y, group))) => {
                    group.push(chunk);
                    *repr_y = group.iter().map(|c| c.y).sum::<f64>() / group.len() as f64;
                }
                None => {
                    groups.push((chunk.y, vec![chunk]));
                }
            }
        }

        for (_, group) in groups.iter_mut() {
            // Horizontal lines (LTR, RTL, or mixed) are assembled in pure VISUAL
            // order — left-to-right by x — exactly like LTR text. The conversion
            // from visual to logical reading order for any RTL content is done
            // once on the fully assembled line string in `build_text_lines`,
            // using the Unicode Bidirectional Algorithm (UAX#9). Sorting chunks
            // by x here keeps chunk grouping and the inter-chunk spacing logic
            // identical to the LTR path; only the final string is reordered.
            //
            // Vertical (CJK top-to-bottom) chunks never reach this path: they are
            // partitioned out in `reconstruct` and handled by
            // `reconstruct_vertical`, which clusters by column instead of line.
            group.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal));
        }

        let mut result: Vec<(f64, Vec<TextChunk>)> = groups;
        result.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
        result.into_iter().map(|(_, chunks)| chunks).collect()
    }

    /// Find the x coordinate of the gap between two columns.
    pub fn find_column_split_x(&self, lines: &[Vec<TextChunk>]) -> Option<f64> {
        let all_chunks: Vec<&TextChunk> = lines.iter().flatten().collect();
        if all_chunks.len() < 6 {
            return None;
        }

        let x_min = all_chunks.iter().map(|c| c.x).fold(f64::INFINITY, f64::min);
        let x_max = all_chunks
            .iter()
            .map(|c| c.right())
            .fold(f64::NEG_INFINITY, f64::max);
        let x_range = x_max - x_min;
        if x_range < self.column_gap_min_points * 2.0 {
            return None;
        }

        let bin_width = 5.0_f64;
        let n_bins = ((x_range / bin_width).ceil() as usize).max(1);
        let mut histogram = vec![0u32; n_bins];
        for chunk in &all_chunks {
            let bin = ((chunk.x - x_min) / bin_width) as usize;
            if bin < n_bins {
                histogram[bin] += 1;
            }

            let end_bin = ((chunk.right() - x_min) / bin_width) as usize;
            if end_bin < n_bins {
                histogram[end_bin] += 1;
            }
        }

        struct Gap {
            start_x: f64,
            end_x: f64,
        }

        let mut gaps: Vec<Gap> = Vec::new();
        let mut gap_start: Option<usize> = None;

        for (i, &count) in histogram.iter().enumerate() {
            if count == 0 {
                if gap_start.is_none() {
                    gap_start = Some(i);
                }
            } else if let Some(start) = gap_start.take() {
                let gx_start = x_min + start as f64 * bin_width;
                let gx_end = x_min + i as f64 * bin_width;
                gaps.push(Gap {
                    start_x: gx_start,
                    end_x: gx_end,
                });
            }
        }
        if let Some(start) = gap_start {
            let gx_start = x_min + start as f64 * bin_width;
            gaps.push(Gap {
                start_x: gx_start,
                end_x: x_max,
            });
        }

        let middle_low = x_min + x_range / 3.0;
        let middle_high = x_min + 2.0 * x_range / 3.0;
        let mut valid_gaps: Vec<Gap> = gaps
            .into_iter()
            .filter(|g| {
                let width = g.end_x - g.start_x;
                let center = (g.start_x + g.end_x) / 2.0;
                width >= self.column_gap_min_points && center >= middle_low && center <= middle_high
            })
            .collect();

        if valid_gaps.is_empty() {
            return None;
        }

        valid_gaps.sort_by(|a, b| {
            let wa = a.end_x - a.start_x;
            let wb = b.end_x - b.start_x;
            wb.partial_cmp(&wa).unwrap_or(Ordering::Equal)
        });
        let best_gap = &valid_gaps[0];
        let split_x = (best_gap.start_x + best_gap.end_x) / 2.0;

        let left_lines = lines
            .iter()
            .filter(|g| {
                g.iter().any(|c| {
                    ((c.x + c.right()) / 2.0).is_finite() && (c.x + c.right()) / 2.0 < split_x
                })
            })
            .count();
        let right_lines = lines
            .iter()
            .filter(|g| {
                g.iter().any(|c| {
                    ((c.x + c.right()) / 2.0).is_finite() && (c.x + c.right()) / 2.0 >= split_x
                })
            })
            .count();
        let threshold = (lines.len() as f64 * 0.2).ceil() as usize;

        if left_lines >= threshold && right_lines >= threshold {
            Some(split_x)
        } else {
            None
        }
    }

    fn split_columns(
        &self,
        lines: Vec<Vec<TextChunk>>,
    ) -> (Vec<Vec<TextChunk>>, Vec<Vec<TextChunk>>) {
        match self.find_column_split_x(&lines) {
            Some(split_x) => {
                let mut col0: Vec<Vec<TextChunk>> = Vec::new();
                let mut col1: Vec<Vec<TextChunk>> = Vec::new();

                for group in lines {
                    let mut left: Vec<TextChunk> = Vec::new();
                    let mut right: Vec<TextChunk> = Vec::new();

                    for chunk in group {
                        let center_x = (chunk.x + chunk.right()) / 2.0;
                        if center_x < split_x {
                            left.push(chunk);
                        } else {
                            right.push(chunk);
                        }
                    }

                    if !left.is_empty() && !right.is_empty() {
                        col0.push(left);
                        col1.push(right);
                    } else if !left.is_empty() {
                        col0.push(left);
                    } else if !right.is_empty() {
                        col1.push(right);
                    }
                }

                (col0, col1)
            }
            None => (lines, vec![]),
        }
    }

    fn build_text_lines(&self, raw_lines: &[Vec<TextChunk>], column: usize) -> Vec<TextLine> {
        raw_lines
            .iter()
            .map(|group| {
                if group.is_empty() {
                    return TextLine {
                        text: String::new(),
                        y: 0.0,
                        x_min: 0.0,
                        x_max: 0.0,
                        font_size: 0.0,
                        is_paragraph_break: false,
                        column,
                    };
                }

                let repr_y = group.iter().map(|c| c.y).sum::<f64>() / group.len() as f64;

                let mut sizes: Vec<f64> = group.iter().map(|c| c.font_size).collect();
                sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
                let font_size = sizes[sizes.len() / 2].max(1.0);

                let x_min = group.iter().map(|c| c.x).fold(f64::INFINITY, f64::min);
                let x_min = if x_min.is_finite() { x_min } else { 0.0 };
                let x_max = group
                    .iter()
                    .map(|c| c.right())
                    .fold(f64::NEG_INFINITY, f64::max);
                let x_max = if x_max.is_finite() { x_max } else { 0.0 };

                let mut text = String::new();
                for (i, chunk) in group.iter().enumerate() {
                    if i > 0 {
                        let prev = &group[i - 1];
                        let gap = chunk.x - prev.right();
                        let ref_size = prev.font_size.min(chunk.font_size).max(1.0);

                        if gap > ref_size * self.wide_gap_factor {
                            text.push_str("  ");
                        } else if gap > ref_size * self.word_gap_factor {
                            text.push(' ');
                        }
                    }
                    text.push_str(&chunk.text);
                }

                // The line was assembled in visual (left-to-right) order. If it
                // contains any right-to-left characters, convert that visual
                // sequence into logical reading order with the Unicode
                // Bidirectional Algorithm. Pure-LTR lines take a fast path and
                // are returned unchanged (zero behaviour change, no allocation).
                let text = visual_to_logical(text);

                TextLine {
                    text,
                    y: repr_y,
                    x_min,
                    x_max,
                    font_size,
                    is_paragraph_break: false,
                    column,
                }
            })
            .collect()
    }

    fn mark_paragraph_breaks(&self, lines: &mut [TextLine]) {
        if lines.len() < 2 {
            return;
        }

        for col in 0..=1usize {
            let col_indices: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, l)| l.column == col)
                .map(|(i, _)| i)
                .collect();

            if col_indices.len() < 2 {
                continue;
            }

            let gaps: Vec<f64> = col_indices
                .windows(2)
                .map(|w| {
                    let prev_y = lines[w[0]].y;
                    let curr_y = lines[w[1]].y;
                    (prev_y - curr_y).abs()
                })
                .filter(|&g| g > 0.1)
                .collect();

            if gaps.is_empty() {
                continue;
            }

            let mut sorted_gaps = gaps.clone();
            sorted_gaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
            let median_gap = sorted_gaps[sorted_gaps.len() / 2];
            let threshold = median_gap * self.paragraph_gap_factor;

            for window in col_indices.windows(2) {
                let prev_y = lines[window[0]].y;
                let curr_y = lines[window[1]].y;
                let gap = (prev_y - curr_y).abs();
                if gap > threshold {
                    lines[window[1]].is_paragraph_break = true;
                }
            }
        }
    }

    fn join_hyphenated_lines(&self, lines: &mut Vec<TextLine>) {
        let mut i = 0;
        while i + 1 < lines.len() {
            let ends_with_hyphen = lines[i].text.trim_end().ends_with('-');
            let next_lower = lines[i + 1]
                .text
                .chars()
                .next()
                .map(|c| c.is_lowercase())
                .unwrap_or(false);
            let same_column = lines[i].column == lines[i + 1].column;
            let not_para_break = !lines[i + 1].is_paragraph_break;

            if ends_with_hyphen && next_lower && same_column && not_para_break {
                let prefix = lines[i].text.trim_end().trim_end_matches('-').to_string();
                let suffix = lines.remove(i + 1);
                lines[i].text = prefix + &suffix.text;
                lines[i].x_max = lines[i].x_max.max(suffix.x_max);
            } else {
                i += 1;
            }
        }
    }
}

/// Convert a line of text from VISUAL order (the left-to-right order in which
/// glyphs were placed on the page) into LOGICAL reading order (the order a human
/// reads, and the order `pdftotext` and other extractors emit), using the
/// Unicode Bidirectional Algorithm (UAX#9, via the `unicode-bidi` crate).
///
/// # Why this is the inverse of the usual BiDi call
///
/// `unicode-bidi` is normally fed a *logical* string and asked to produce the
/// *visual* order for display. Here we have the opposite problem: PDF content
/// streams position glyphs in visual order, and we must recover logical order
/// for extraction. For text with at most one level of RTL embedding — i.e. an
/// RTL line that may contain embedded LTR runs (Latin words, numbers), which
/// covers essentially all extracted prose — the BiDi reordering (UAX#9 rule L2,
/// a sequence of run reversals) is an *involution*: applying it to the visual
/// string recovers the logical string. So we run the same `reorder_line` over
/// the visually-ordered text. (For pathological lines with ≥2 nested embedding
/// levels this is an approximation, but such lines do not arise from simple text
/// extraction; documented as a known limitation.)
///
/// # Base direction
///
/// Per UAX#9 P2/P3 the base direction is set by the first strong character of
/// the paragraph. We only have a single extracted line, so we use a line-level
/// heuristic: if the line is RTL-dominant (more than half its alphabetic
/// characters are RTL — the same test `TextChunk::is_rtl` uses) we force an RTL
/// base level; otherwise we force LTR. A mixed line that is mostly Latin with a
/// short RTL phrase therefore keeps an LTR base, matching how such lines read.
///
/// # Fast path
///
/// Lines with no RTL characters at all (the overwhelming majority of any corpus)
/// skip the BiDi machinery entirely and are returned unchanged. This guarantees
/// zero behavioural change and zero added cost for LTR-only documents.
fn visual_to_logical(visual: String) -> String {
    // Fast path: no RTL character anywhere → already in logical order.
    if !visual.chars().any(is_rtl_char) {
        return visual;
    }

    // Choose the base embedding level from the line's dominant direction.
    let base = if is_rtl_dominant(&visual) {
        Level::rtl()
    } else {
        Level::ltr()
    };

    let bidi = BidiInfo::new(&visual, Some(base));
    // BidiInfo splits on paragraph separators; an extracted line normally has
    // none, but reorder each paragraph segment defensively and rejoin.
    if bidi.paragraphs.is_empty() {
        return visual;
    }
    let mut out = String::with_capacity(visual.len());
    for para in &bidi.paragraphs {
        let line = para.range.clone();
        out.push_str(&bidi.reorder_line(para, line));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(text: &str, x: f64, y: f64, font_size: f64) -> TextChunk {
        TextChunk {
            text: text.to_string(),
            x,
            y,
            font_size,
            font_name: "F1".to_string(),
            width: text.len() as f64 * font_size * 0.6,
            is_rtl: false,
            is_vertical: false,
            is_invisible: false,
        }
    }

    /// A vertical-writing-mode chunk (one glyph, square advance ≈ font_size).
    fn vchunk(text: &str, x: f64, y: f64, font_size: f64) -> TextChunk {
        TextChunk {
            text: text.to_string(),
            x,
            y,
            font_size,
            font_name: "V1".to_string(),
            width: font_size,
            is_rtl: false,
            is_vertical: true,
            is_invisible: false,
        }
    }

    #[test]
    fn single_chunk_produces_one_line() {
        let r = ReadingOrderReconstructor::new();
        let lines = r.reconstruct(vec![chunk("Hello", 100.0, 700.0, 12.0)]);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Hello");
        assert!((lines[0].y - 700.0).abs() < 0.5);
    }

    #[test]
    fn two_chunks_same_y_are_joined() {
        let r = ReadingOrderReconstructor::new();
        let lines = r.reconstruct(vec![
            chunk("Hello", 100.0, 700.0, 12.0),
            chunk("World", 145.0, 700.0, 12.0),
        ]);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].text.contains("Hello"));
        assert!(lines[0].text.contains("World"));
        assert!(
            lines[0].text.contains(" "),
            "should have space between chunks"
        );
    }

    #[test]
    fn two_chunks_different_y_become_two_lines() {
        let r = ReadingOrderReconstructor::new();
        let lines = r.reconstruct(vec![
            chunk("Line1", 100.0, 700.0, 12.0),
            chunk("Line2", 100.0, 685.0, 12.0),
        ]);
        assert_eq!(
            lines.len(),
            2,
            "15pt gap at 12pt font should produce 2 lines"
        );
        assert!(
            lines[0].text.contains("Line1"),
            "first line should be Line1 (higher y), got: {:?}",
            lines[0].text
        );
        assert!(lines[1].text.contains("Line2"));
    }

    #[test]
    fn chunks_within_tolerance_grouped_correctly() {
        let r = ReadingOrderReconstructor::new();
        let lines = r.reconstruct(vec![
            chunk("A", 50.0, 700.0, 12.0),
            chunk("B", 100.0, 702.0, 12.0),
            chunk("C", 150.0, 698.0, 12.0),
        ]);
        assert_eq!(
            lines.len(),
            1,
            "chunks within y tolerance should be on one line"
        );
        assert!(lines[0].text.contains("A"));
        assert!(lines[0].text.contains("B"));
        assert!(lines[0].text.contains("C"));
    }

    #[test]
    fn chunks_sorted_left_to_right_within_a_line() {
        let r = ReadingOrderReconstructor::new();
        let lines = r.reconstruct(vec![
            chunk("Right", 200.0, 700.0, 12.0),
            chunk("Left", 50.0, 700.0, 12.0),
        ]);
        assert_eq!(lines.len(), 1);
        let text = &lines[0].text;
        let left_pos = text.find("Left").unwrap_or(usize::MAX);
        let right_pos = text.find("Right").unwrap_or(usize::MAX);
        assert!(
            left_pos < right_pos,
            "Left (x=50) should come before Right (x=200)"
        );
    }

    #[test]
    fn lines_sorted_top_to_bottom() {
        let r = ReadingOrderReconstructor::new();
        let lines = r.reconstruct(vec![
            chunk("Bottom", 100.0, 100.0, 12.0),
            chunk("Middle", 100.0, 400.0, 12.0),
            chunk("Top", 100.0, 700.0, 12.0),
        ]);
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0].text.contains("Top"),
            "highest y (Top) should be first line, got: {:?}",
            lines[0].text
        );
        assert!(lines[1].text.contains("Middle"));
        assert!(lines[2].text.contains("Bottom"));
    }

    #[test]
    fn wide_gap_within_a_line_inserts_two_spaces() {
        let r = ReadingOrderReconstructor::new();
        let mut ca = chunk("Col1", 50.0, 700.0, 12.0);
        ca.width = 36.0;
        let cb = chunk("Col2", 200.0, 700.0, 12.0);
        let lines = r.reconstruct(vec![ca, cb]);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].text.contains("  "),
            "wide gap should produce two spaces, got: {:?}",
            lines[0].text
        );
    }

    #[test]
    fn no_gap_between_touching_chunks() {
        let r = ReadingOrderReconstructor::new();
        let mut ca = chunk("AB", 50.0, 700.0, 12.0);
        ca.width = 50.0;
        let cb = chunk("CD", 100.0, 700.0, 12.0);
        let lines = r.reconstruct(vec![ca, cb]);
        assert_eq!(lines.len(), 1);
        let text = &lines[0].text;
        assert!(
            !text.contains("  "),
            "zero gap should not produce double space"
        );
    }

    #[test]
    fn paragraph_break_detection() {
        let r = ReadingOrderReconstructor::new();
        let chunks = vec![
            chunk("Para1 Line1", 50.0, 700.0, 12.0),
            chunk("Para1 Line2", 50.0, 686.0, 12.0),
            chunk("Para1 Line3", 50.0, 672.0, 12.0),
            chunk("Para2 Line1", 50.0, 632.0, 12.0),
            chunk("Para2 Line2", 50.0, 618.0, 12.0),
        ];
        let lines = r.reconstruct(chunks);
        assert_eq!(lines.len(), 5);
        let break_line = lines.iter().find(|l| l.text.contains("Para2 Line1"));
        assert!(break_line.is_some());
        assert!(
            break_line.unwrap().is_paragraph_break,
            "40pt gap should trigger paragraph break"
        );
        let normal_line = lines.iter().find(|l| l.text.contains("Para1 Line2"));
        assert!(normal_line.is_some());
        assert!(
            !normal_line.unwrap().is_paragraph_break,
            "14pt gap (normal) should NOT trigger paragraph break"
        );
    }

    #[test]
    fn empty_input() {
        let r = ReadingOrderReconstructor::new();
        let lines = r.reconstruct(vec![]);
        assert!(lines.is_empty());
    }

    #[test]
    fn single_large_font_line_detected_as_heading() {
        let r = ReadingOrderReconstructor::new();
        let chunks = vec![
            chunk("Big Title", 50.0, 700.0, 24.0),
            chunk("Normal text", 50.0, 680.0, 12.0),
            chunk("More text", 50.0, 666.0, 12.0),
        ];
        let lines = r.reconstruct(chunks);
        assert_eq!(lines.len(), 3);
        let title_line = lines.iter().find(|l| l.text.contains("Big Title")).unwrap();
        assert!((title_line.font_size - 24.0).abs() < 1.0);
        let avg_fs = lines.iter().map(|l| l.font_size).sum::<f64>() / lines.len() as f64;
        assert!(
            title_line.is_heading(avg_fs),
            "24pt line should be detected as heading"
        );
    }

    #[test]
    fn two_column_layout_detected_and_ordered_correctly() {
        let r = ReadingOrderReconstructor::new();
        let chunks = vec![
            chunk("Left1", 50.0, 700.0, 12.0),
            chunk("Right1", 320.0, 700.0, 12.0),
            chunk("Left2", 50.0, 686.0, 12.0),
            chunk("Right2", 320.0, 686.0, 12.0),
            chunk("Left3", 50.0, 672.0, 12.0),
            chunk("Right3", 320.0, 672.0, 12.0),
        ];
        let lines = r.reconstruct(chunks);
        assert_eq!(lines.len(), 6);
        let left_positions: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.text.contains("Left"))
            .map(|(i, _)| i)
            .collect();
        let right_positions: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.text.contains("Right"))
            .map(|(i, _)| i)
            .collect();
        let max_left = left_positions.iter().max().copied().unwrap_or(0);
        let min_right = right_positions.iter().min().copied().unwrap_or(usize::MAX);
        assert!(
            max_left < min_right,
            "All left-column lines should precede right-column lines. Left positions: {:?}, Right positions: {:?}",
            left_positions,
            right_positions
        );
    }

    #[test]
    fn group_into_lines_preserves_order() {
        let r = ReadingOrderReconstructor::new();
        let chunks = vec![
            chunk("E", 50.0, 100.0, 12.0),
            chunk("D", 50.0, 200.0, 12.0),
            chunk("C", 50.0, 300.0, 12.0),
            chunk("B", 50.0, 400.0, 12.0),
            chunk("A", 50.0, 500.0, 12.0),
        ];
        let groups = r.group_into_lines(chunks);
        assert_eq!(groups.len(), 5);
        assert!(
            groups[0][0].text.contains("A"),
            "highest y first, got: {}",
            groups[0][0].text
        );
        assert!(
            groups[4][0].text.contains("E"),
            "lowest y last, got: {}",
            groups[4][0].text
        );
    }

    #[test]
    fn find_column_split_x_returns_none_for_single_column() {
        let r = ReadingOrderReconstructor::new();
        let groups: Vec<Vec<TextChunk>> = vec![
            vec![
                chunk("A", 50.0, 700.0, 12.0),
                chunk("B", 100.0, 700.0, 12.0),
            ],
            vec![
                chunk("C", 60.0, 686.0, 12.0),
                chunk("D", 110.0, 686.0, 12.0),
            ],
            vec![
                chunk("E", 55.0, 672.0, 12.0),
                chunk("F", 105.0, 672.0, 12.0),
            ],
        ];
        assert!(
            r.find_column_split_x(&groups).is_none(),
            "single-column layout should return None"
        );
    }

    #[test]
    fn mixed_font_sizes_on_same_line_are_grouped_correctly() {
        let r = ReadingOrderReconstructor::new();
        let mut big = chunk("Bold", 50.0, 700.0, 18.0);
        big.width = 50.0;
        let small = chunk("text", 110.0, 700.0, 12.0);
        let lines = r.reconstruct(vec![big, small]);
        assert_eq!(
            lines.len(),
            1,
            "mixed-size chunks on same line should be one line"
        );
        assert!(lines[0].text.contains("Bold"));
        assert!(lines[0].text.contains("text"));
    }

    #[test]
    fn negative_x_position_handled_gracefully() {
        let r = ReadingOrderReconstructor::new();
        let chunks = vec![
            chunk("Neg", -30.0, 700.0, 12.0),
            chunk("Pos", 100.0, 700.0, 12.0),
        ];
        let lines = r.reconstruct(chunks);
        assert!(!lines.is_empty(), "negative x chunks should not panic");
    }

    #[test]
    fn very_many_chunks_on_one_line() {
        let r = ReadingOrderReconstructor::new();
        let chunks: Vec<TextChunk> = (0..20)
            .map(|i| {
                let word = format!("w{:02}", i);
                let mut c = chunk(&word, 50.0 + i as f64 * 30.0, 700.0, 12.0);
                c.width = 28.0;
                c
            })
            .collect();
        let lines = r.reconstruct(chunks);
        assert_eq!(lines.len(), 1, "20 words on same y should form one line");
        let text = &lines[0].text;
        let p0 = text.find("w00").unwrap_or(usize::MAX);
        let p19 = text.find("w19").unwrap_or(usize::MAX);
        assert!(p0 < p19, "words should be left-to-right in output");
    }

    #[test]
    fn reconstruct_is_deterministic() {
        let r = ReadingOrderReconstructor::new();
        let chunks = vec![
            chunk("C", 150.0, 700.0, 12.0),
            chunk("A", 50.0, 700.0, 12.0),
            chunk("B", 100.0, 700.0, 12.0),
        ];
        let lines1 = r.reconstruct(chunks.clone());
        let lines2 = r.reconstruct(chunks.clone());
        assert_eq!(lines1.len(), lines2.len());
        assert_eq!(
            lines1[0].text, lines2[0].text,
            "reconstruction should be deterministic"
        );
    }

    #[test]
    fn x_min_and_x_max_on_text_line_are_correct() {
        let r = ReadingOrderReconstructor::new();
        let mut ca = chunk("Hello", 50.0, 700.0, 12.0);
        ca.width = 40.0;
        let mut cb = chunk("World", 100.0, 700.0, 12.0);
        cb.width = 40.0;
        let lines = r.reconstruct(vec![ca, cb]);
        assert_eq!(lines.len(), 1);
        assert!(
            (lines[0].x_min - 50.0).abs() < 1.0,
            "x_min should be 50.0, got {}",
            lines[0].x_min
        );
        assert!(
            (lines[0].x_max - 140.0).abs() < 1.0,
            "x_max should be 140.0, got {}",
            lines[0].x_max
        );
    }

    #[test]
    fn reconstruct_with_column_detection_disabled() {
        let mut r = ReadingOrderReconstructor::new();
        r.detect_columns = false;
        let chunks = vec![
            chunk("Left1", 50.0, 700.0, 12.0),
            chunk("Right1", 320.0, 700.0, 12.0),
            chunk("Left2", 50.0, 686.0, 12.0),
            chunk("Right2", 320.0, 686.0, 12.0),
        ];
        let lines = r.reconstruct(chunks);
        assert!(
            lines.iter().any(|l| l.column == 0),
            "all lines should be column 0 when detect_columns is false"
        );
        assert!(
            lines.iter().all(|l| l.column == 0),
            "no column 1 lines expected when detect_columns is false"
        );
    }

    #[test]
    fn hyphen_joining_merges_broken_word() {
        let mut r = ReadingOrderReconstructor::new();
        r.join_hyphens = true;
        let chunks = vec![
            chunk("pro-", 50.0, 700.0, 12.0),
            chunk("gramming", 50.0, 686.0, 12.0),
        ];
        let lines = r.reconstruct(chunks);
        assert_eq!(lines.len(), 1, "hyphen-broken word should be joined");
        assert_eq!(lines[0].text.trim(), "programming");
    }

    #[test]
    fn hyphen_joining_does_not_join_proper_noun() {
        let mut r = ReadingOrderReconstructor::new();
        r.join_hyphens = true;
        let chunks = vec![
            chunk("anti-", 50.0, 700.0, 12.0),
            chunk("American", 50.0, 686.0, 12.0),
        ];
        let lines = r.reconstruct(chunks);
        assert_eq!(lines.len(), 2, "uppercase next word should NOT be joined");
        assert!(lines[0].text.trim_end().ends_with('-'));
    }

    #[test]
    fn rtl_chunks_reordered_to_logical_order() {
        // Three Hebrew letters placed left-to-right on the page at x = 100, 200,
        // 300 (visual order vav, lamed, he). Hebrew reads right-to-left, so the
        // rightmost glyph (he, x=300) is logically FIRST. After BiDi
        // visual→logical conversion the extracted string must read he, lamed,
        // vav — i.e. the reverse of the on-page left-to-right placement.
        let r = ReadingOrderReconstructor::new();
        let chunks = vec![
            TextChunk {
                text: "\u{05D5}".to_string(), // vav, leftmost on page
                x: 100.0,
                y: 700.0,
                font_size: 12.0,
                font_name: "F1".to_string(),
                width: 10.0,
                is_rtl: true,
                is_vertical: false,
                is_invisible: false,
            },
            TextChunk {
                text: "\u{05DC}".to_string(), // lamed, middle
                x: 200.0,
                y: 700.0,
                font_size: 12.0,
                font_name: "F1".to_string(),
                width: 10.0,
                is_rtl: true,
                is_vertical: false,
                is_invisible: false,
            },
            TextChunk {
                text: "\u{05D4}".to_string(), // he, rightmost on page
                x: 300.0,
                y: 700.0,
                font_size: 12.0,
                font_name: "F1".to_string(),
                width: 10.0,
                is_rtl: true,
                is_vertical: false,
                is_invisible: false,
            },
        ];
        let lines = r.reconstruct(chunks);
        assert_eq!(lines.len(), 1);
        let text = &lines[0].text;
        let pos_he = text.find('\u{05D4}').unwrap_or(usize::MAX);
        let pos_lamed = text.find('\u{05DC}').unwrap_or(usize::MAX);
        let pos_vav = text.find('\u{05D5}').unwrap_or(usize::MAX);
        assert!(
            pos_he < pos_lamed && pos_lamed < pos_vav,
            "RTL line should be in logical order he<lamed<vav, got: {:?}",
            text
        );
    }

    #[test]
    fn vertical_chunks_sorted_y_descending() {
        // Three glyphs in ONE column (same x), at y = 300, 200, 100. Vertical
        // reading is top-to-bottom = highest y first, so the single emitted line
        // reads 三(y=300) 二(y=200) 一(y=100).
        let r = ReadingOrderReconstructor::new();
        let chunks = vec![
            vchunk("\u{4E00}", 100.0, 100.0, 12.0),
            vchunk("\u{4E8C}", 100.0, 200.0, 12.0),
            vchunk("\u{4E09}", 100.0, 300.0, 12.0),
        ];
        let lines = r.reconstruct(chunks);
        assert_eq!(lines.len(), 1, "one column → one line, got {lines:?}");
        let text = &lines[0].text;
        let p3 = text.find('\u{4E09}').unwrap_or(usize::MAX);
        let p2 = text.find('\u{4E8C}').unwrap_or(usize::MAX);
        let p1 = text.find('\u{4E00}').unwrap_or(usize::MAX);
        assert!(
            p3 < p2 && p2 < p1,
            "vertical column should read top-to-bottom 三二一, got: {text:?}"
        );
    }

    #[test]
    fn vertical_columns_ordered_right_to_left() {
        // Two columns: right column (x=200) holds 右上/右下, left column (x=100)
        // holds 左上/左下. Vertical reading orders columns right-to-left, so the
        // right column's line comes first.
        let r = ReadingOrderReconstructor::new();
        let chunks = vec![
            vchunk("\u{5DE6}", 100.0, 300.0, 12.0), // left-top
            vchunk("\u{4E0B}", 100.0, 280.0, 12.0), // left-bottom
            vchunk("\u{53F3}", 200.0, 300.0, 12.0), // right-top
            vchunk("\u{4E0A}", 200.0, 280.0, 12.0), // right-bottom
        ];
        let lines = r.reconstruct(chunks);
        assert_eq!(lines.len(), 2, "two columns → two lines, got {lines:?}");
        // First line is the rightmost column.
        assert!(
            lines[0].text.contains('\u{53F3}'),
            "rightmost column should be read first, got: {:?}",
            lines[0].text
        );
        assert!(
            lines[1].text.contains('\u{5DE6}'),
            "leftmost column should be read last, got: {:?}",
            lines[1].text
        );
    }

    #[test]
    fn mixed_vertical_and_horizontal_handled_per_run() {
        // A vertical body column plus a horizontal heading line. Each is grouped
        // by its own writing mode; both survive into the output.
        let r = ReadingOrderReconstructor::new();
        let chunks = vec![
            vchunk("\u{7E26}", 100.0, 300.0, 12.0), // vertical glyph
            vchunk("\u{66F8}", 100.0, 280.0, 12.0), // vertical glyph below it
            chunk("Heading", 50.0, 500.0, 12.0),    // horizontal line above
        ];
        let lines = r.reconstruct(chunks);
        assert!(
            lines.iter().any(|l| l.text.contains("Heading")),
            "horizontal heading should be present: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.text.contains('\u{7E26}')),
            "vertical text should be present: {lines:?}"
        );
    }

    // ── Unicode BiDi (UAX#9) visual→logical reordering ──────────────────────
    //
    // These exercise `visual_to_logical` directly with hand-constructed visual
    // strings and known-correct logical output. The transform-direction bug
    // (reordering the wrong way) is the #1 risk of a BiDi integration, so these
    // pin it down before the full pipeline runs.

    const HE_ALEF: char = '\u{05D0}'; // א
    const HE_BET: char = '\u{05D1}'; // ב
    const HE_GIMEL: char = '\u{05D2}'; // ג
    const AR_MEEM: char = '\u{0645}'; // م
    const AR_RA: char = '\u{0631}'; // ر

    #[test]
    fn bidi_pure_ltr_is_unchanged_via_fast_path() {
        // No RTL characters → fast path → byte-identical output.
        let input = "Hello, World! 123".to_string();
        assert_eq!(visual_to_logical(input.clone()), input);
    }

    #[test]
    fn bidi_pure_rtl_word_is_reversed() {
        // A Hebrew word laid out visually as alef-bet-gimel (left to right on the
        // page) reads logically gimel-bet-alef (right to left). The visual→logical
        // transform must reverse it.
        let visual: String = [HE_ALEF, HE_BET, HE_GIMEL].iter().collect();
        let logical = visual_to_logical(visual);
        let expected: String = [HE_GIMEL, HE_BET, HE_ALEF].iter().collect();
        assert_eq!(logical, expected, "pure RTL run should be reversed");
    }

    #[test]
    fn bidi_rtl_with_embedded_latin_word_keeps_latin_ltr() {
        // An RTL-dominant Arabic line containing an embedded Latin word "PDF".
        // (RTL-dominant so the line-level base-direction heuristic picks an RTL
        // base, matching how the glyphs were laid out.) Logically the line reads,
        // right-to-left: <arabic> PDF <arabic>. The embedded Latin run must stay
        // spelled forward ("PDF", not "FDP") while the Arabic runs reverse — the
        // exact case the old majority-vote heuristic got wrong.
        let before = [AR_MEEM, AR_RA, AR_MEEM].iter().collect::<String>();
        let after = [AR_RA, AR_MEEM].iter().collect::<String>();
        let logical = format!("{} PDF {}", before, after);
        // Build the on-page visual layout by running logical→visual once.
        let info = BidiInfo::new(&logical, Some(Level::rtl()));
        let para = &info.paragraphs[0];
        let visual = info.reorder_line(para, para.range.clone()).into_owned();
        assert_ne!(visual, logical, "sanity: visual differs from logical");

        let recovered = visual_to_logical(visual);
        assert!(
            recovered.contains("PDF"),
            "embedded Latin run must stay forward (PDF), got: {:?}",
            recovered
        );
        assert!(
            !recovered.contains("FDP"),
            "embedded Latin run must not be reversed, got: {:?}",
            recovered
        );
        assert_eq!(recovered, logical, "should recover the logical order");
    }

    #[test]
    fn bidi_rtl_with_number_and_parens_resolves_neutrals() {
        // Arabic text with a parenthesized number. Numbers (EN) and the bracket
        // pair are neutrals/weaks with specific UAX#9 resolution; verify the
        // digits stay in left-to-right order and the round trip is stable.
        let logical = format!("{}{} (2026)", AR_MEEM, AR_RA);
        let info = BidiInfo::new(&logical, Some(Level::rtl()));
        let para = &info.paragraphs[0];
        let visual = info.reorder_line(para, para.range.clone()).into_owned();

        let recovered = visual_to_logical(visual);
        assert!(
            recovered.contains("2026"),
            "digits must remain in LTR order, got: {:?}",
            recovered
        );
        assert_eq!(recovered, logical);
    }

    #[test]
    fn bidi_mostly_latin_line_with_short_rtl_phrase_keeps_ltr_base() {
        // A predominantly Latin line with a short Hebrew phrase. The base
        // direction is LTR (not RTL-dominant), so Latin stays first and the
        // Hebrew run is the only part reordered.
        let hebrew: String = [HE_ALEF, HE_BET].iter().collect();
        let logical = format!("Title: {} end", hebrew);
        let info = BidiInfo::new(&logical, Some(Level::ltr()));
        let para = &info.paragraphs[0];
        let visual = info.reorder_line(para, para.range.clone()).into_owned();

        let recovered = visual_to_logical(visual);
        assert!(
            recovered.starts_with("Title:"),
            "LTR-base line should start with the Latin text, got: {:?}",
            recovered
        );
        assert_eq!(recovered, logical);
    }

    #[test]
    fn bidi_empty_and_whitespace_are_safe() {
        assert_eq!(visual_to_logical(String::new()), "");
        assert_eq!(visual_to_logical("   ".to_string()), "   ");
    }
}
