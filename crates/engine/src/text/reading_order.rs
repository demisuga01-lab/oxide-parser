use std::cmp::Ordering;

use super::collector::TextChunk;

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
            Self::sort_group_for_direction(group);
        }

        let mut result: Vec<(f64, Vec<TextChunk>)> = groups;
        result.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
        result.into_iter().map(|(_, chunks)| chunks).collect()
    }

    fn sort_group_for_direction(group: &mut [TextChunk]) {
        let vertical_count = group.iter().filter(|c| c.is_vertical).count();
        let rtl_count = group.iter().filter(|c| c.is_rtl).count();
        let is_vertical_line = vertical_count * 2 > group.len() && !group.is_empty();
        let is_rtl_line = rtl_count * 2 > group.len() && !group.is_empty();

        if is_vertical_line {
            group.sort_by(|a, b| b.y.partial_cmp(&a.y).unwrap_or(Ordering::Equal));
        } else if is_rtl_line {
            // TODO(bidi): Full Unicode BiDi (UAX#9) is not implemented. Mixed
            // LTR/RTL lines are handled by majority-vote and may have incorrect
            // word order when both directions appear on the same line.
            group.sort_by(|a, b| b.x.partial_cmp(&a.x).unwrap_or(Ordering::Equal));
        } else {
            group.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal));
        }
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
    fn rtl_chunks_sorted_x_descending() {
        let r = ReadingOrderReconstructor::new();
        let chunks = vec![
            TextChunk {
                text: "\u{05D4}".to_string(),
                x: 300.0,
                y: 700.0,
                font_size: 12.0,
                font_name: "F1".to_string(),
                width: 10.0,
                is_rtl: true,
                is_vertical: false,
                is_invisible: false,
            },
            TextChunk {
                text: "\u{05DC}".to_string(),
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
                text: "\u{05D5}".to_string(),
                x: 100.0,
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
            "RTL chunks should be ordered x-descending, got: {:?}",
            text
        );
    }

    #[test]
    fn vertical_chunks_sorted_y_descending() {
        let r = ReadingOrderReconstructor::new();
        let chunks = vec![
            TextChunk {
                text: "\u{4E00}".to_string(),
                x: 100.0,
                y: 100.0,
                font_size: 12.0,
                font_name: "F1".to_string(),
                width: 12.0,
                is_rtl: false,
                is_vertical: true,
                is_invisible: false,
            },
            TextChunk {
                text: "\u{4E8C}".to_string(),
                x: 100.0,
                y: 200.0,
                font_size: 12.0,
                font_name: "F1".to_string(),
                width: 12.0,
                is_rtl: false,
                is_vertical: true,
                is_invisible: false,
            },
            TextChunk {
                text: "\u{4E09}".to_string(),
                x: 100.0,
                y: 300.0,
                font_size: 12.0,
                font_name: "F1".to_string(),
                width: 12.0,
                is_rtl: false,
                is_vertical: true,
                is_invisible: false,
            },
        ];
        let lines = r.reconstruct(chunks);
        assert!(!lines.is_empty(), "vertical chunks should produce lines");
        if lines.len() == 3 {
            assert!(lines[0].text.contains('\u{4E09}'));
            assert!(lines[2].text.contains('\u{4E00}'));
        }
    }
}
