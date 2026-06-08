use unicode_width::UnicodeWidthChar;

pub const MIN_COL_WIDTH: u16 = 4;
pub const PADDING: u16 = 2;

pub fn calculate_header_min_widths<S: AsRef<str>>(headers: &[S]) -> Vec<u16> {
    headers
        .iter()
        .map(|h| (h.as_ref().chars().count() as u16 + PADDING).max(MIN_COL_WIDTH))
        .collect()
}

pub fn wrapped_line_count(text: &str, width: u16) -> u16 {
    use unicode_width::UnicodeWidthStr;

    if width == 0 {
        return 0;
    }

    text.lines().fold(0u16, |acc, line| {
        let w = UnicodeWidthStr::width(line).min(u16::MAX as usize) as u16;
        let wrapped = w.max(1).div_ceil(width);
        acc.saturating_add(wrapped)
    })
}

pub fn search_viewport_offset(input: &str, cursor: usize, visible_width: usize) -> usize {
    let chars: Vec<char> = input.chars().collect();
    let cursor = cursor.min(chars.len());

    if visible_width == 0 {
        return cursor;
    }

    let mut viewport_offset = 0;
    let mut width_before_cursor = display_width(&chars[..cursor]);

    while width_before_cursor >= visible_width && viewport_offset < cursor {
        width_before_cursor =
            width_before_cursor.saturating_sub(char_width(chars[viewport_offset]));
        viewport_offset += 1;
    }

    viewport_offset
}

pub fn slice_chars_fitting_width(input: &str, start: usize, visible_width: usize) -> String {
    if visible_width == 0 {
        return String::new();
    }

    let mut width = 0;
    let mut visible = String::new();

    for ch in input.chars().skip(start) {
        let ch_width = char_width(ch);
        if width + ch_width > visible_width {
            break;
        }
        width += ch_width;
        visible.push(ch);
    }

    visible
}

pub fn display_width(chars: &[char]) -> usize {
    chars.iter().map(|&ch| char_width(ch)).sum()
}

pub fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_min_widths_with_str_slice() {
        let headers = ["id", "name", "description"];
        let widths = calculate_header_min_widths(&headers);
        assert_eq!(widths, vec![4, 6, 13]);
    }

    #[test]
    fn calculates_min_widths_with_string_vec() {
        let headers = vec!["id".to_string(), "name".to_string()];
        let widths = calculate_header_min_widths(&headers);
        assert_eq!(widths, vec![4, 6]);
    }

    #[test]
    fn enforces_min_width() {
        let headers = ["a"];
        let widths = calculate_header_min_widths(&headers);
        assert_eq!(widths, vec![MIN_COL_WIDTH]);
    }

    mod wrapped_line_count_tests {
        use super::super::wrapped_line_count;
        use rstest::rstest;

        #[rstest]
        #[case("", 80, 0)]
        #[case("hello", 80, 1)]
        #[case("hello world", 5, 3)]
        #[case("line1\nline2\nline3", 80, 3)]
        #[case("hello", 0, 0)]
        #[case("12345", 5, 1)]
        #[case("あいう", 6, 1)]
        #[case("あいう", 4, 2)]
        fn counts_wrapped_lines(#[case] text: &str, #[case] width: u16, #[case] expected: u16) {
            assert_eq!(wrapped_line_count(text, width), expected);
        }
    }

    #[test]
    fn slice_chars_fitting_width_omits_first_wide_char_when_viewport_is_too_narrow() {
        assert_eq!(slice_chars_fitting_width("界a", 0, 1), "");
    }

    #[test]
    fn slice_chars_fitting_width_keeps_chars_that_fit_exactly() {
        assert_eq!(slice_chars_fitting_width("ab", 0, 2), "ab");
    }
}
