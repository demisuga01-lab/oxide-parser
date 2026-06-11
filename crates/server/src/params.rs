use crate::error::ServerError;

pub fn parse_page_range(input: Option<&str>, total_pages: usize) -> Result<Vec<usize>, String> {
    let s = match input {
        None | Some("all") | Some("") => {
            return Ok((1..=total_pages).collect());
        }
        Some(s) => s.trim(),
    };

    let mut pages: Vec<usize> = Vec::new();

    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        if let Some(dash_pos) = part.find('-') {
            let start_str = &part[..dash_pos];
            let end_str = &part[dash_pos + 1..];
            let start: usize = start_str
                .trim()
                .parse()
                .map_err(|_| format!("invalid page number '{}'", start_str))?;
            let end: usize = end_str
                .trim()
                .parse()
                .map_err(|_| format!("invalid page number '{}'", end_str))?;
            if start == 0 || end == 0 {
                return Err("page numbers are 1-indexed; 0 is invalid".to_string());
            }
            if start > end {
                return Err(format!("range {}-{} is backwards", start, end));
            }
            if end > total_pages {
                return Err(format!(
                    "page {} exceeds document length ({})",
                    end, total_pages
                ));
            }
            pages.extend(start..=end);
        } else {
            let n: usize = part
                .parse()
                .map_err(|_| format!("invalid page number '{}'", part))?;
            if n == 0 {
                return Err("page numbers are 1-indexed; 0 is invalid".to_string());
            }
            if n > total_pages {
                return Err(format!(
                    "page {} exceeds document length ({})",
                    n, total_pages
                ));
            }
            pages.push(n);
        }
    }

    if pages.is_empty() {
        return Err("no valid page numbers found in range string".to_string());
    }

    pages.sort_unstable();
    pages.dedup();
    Ok(pages)
}

pub fn parse_bool_param(s: Option<&str>, default: bool) -> Result<bool, ServerError> {
    match s.map(str::trim) {
        None => Ok(default),
        Some("true") | Some("1") | Some("yes") => Ok(true),
        Some("false") | Some("0") | Some("no") => Ok(false),
        Some(other) => Err(ServerError::InvalidParameter(format!(
            "expected 'true' or 'false', got '{}'",
            other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_returns_all_pages() {
        assert_eq!(parse_page_range(None, 5).unwrap(), vec![1, 2, 3, 4, 5]);
        assert_eq!(
            parse_page_range(Some("all"), 5).unwrap(),
            vec![1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn single_page() {
        assert_eq!(parse_page_range(Some("3"), 5).unwrap(), vec![3]);
    }

    #[test]
    fn comma_list() {
        assert_eq!(parse_page_range(Some("1,3,5"), 5).unwrap(), vec![1, 3, 5]);
    }

    #[test]
    fn range() {
        assert_eq!(parse_page_range(Some("2-4"), 5).unwrap(), vec![2, 3, 4]);
    }

    #[test]
    fn mixed_ranges() {
        assert_eq!(
            parse_page_range(Some("1-2,4,6-7"), 10).unwrap(),
            vec![1, 2, 4, 6, 7]
        );
    }

    #[test]
    fn deduplication() {
        assert_eq!(parse_page_range(Some("1,1,2"), 5).unwrap(), vec![1, 2]);
    }

    #[test]
    fn page_zero_is_error() {
        assert!(parse_page_range(Some("0"), 5).is_err());
    }

    #[test]
    fn out_of_range_is_error() {
        assert!(parse_page_range(Some("6"), 5).is_err());
    }

    #[test]
    fn backwards_range_is_error() {
        assert!(parse_page_range(Some("5-2"), 5).is_err());
    }

    #[test]
    fn whitespace_is_tolerated() {
        assert_eq!(
            parse_page_range(Some(" 1 , 3 , 5 "), 5).unwrap(),
            vec![1, 3, 5]
        );
        assert_eq!(parse_page_range(Some("2 - 4"), 5).unwrap(), vec![2, 3, 4]);
    }

    #[test]
    fn empty_string_treated_as_all() {
        assert_eq!(parse_page_range(Some(""), 5).unwrap(), vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn single_page_equal_to_total_is_valid() {
        assert_eq!(parse_page_range(Some("5"), 5).unwrap(), vec![5]);
    }

    #[test]
    fn range_full_document() {
        assert_eq!(
            parse_page_range(Some("1-5"), 5).unwrap(),
            vec![1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn overlapping_ranges_deduplicated() {
        assert_eq!(
            parse_page_range(Some("1-3,2-4"), 5).unwrap(),
            vec![1, 2, 3, 4]
        );
    }

    #[test]
    fn non_numeric_returns_error() {
        assert!(parse_page_range(Some("a-b"), 5).is_err());
        assert!(parse_page_range(Some("1,two,3"), 5).is_err());
    }

    #[test]
    fn zero_in_range_is_error() {
        assert!(parse_page_range(Some("0-3"), 5).is_err());
    }
}
