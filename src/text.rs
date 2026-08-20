use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const DISPLAY_LIMIT: usize = 80;
pub const TAB_LABEL_WIDTH: usize = 15;

pub fn complete_line(value: &str) -> Option<String> {
    value
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

pub fn display_line(value: &str) -> Option<String> {
    bounded_line(value, 0)
}

pub fn tab_label(value: &str) -> Option<String> {
    let line = complete_line(value)?;
    if UnicodeWidthStr::width(line.as_str()) <= TAB_LABEL_WIDTH {
        return Some(line);
    }

    let content_width = TAB_LABEL_WIDTH - UnicodeWidthStr::width("…");
    let mut width = 0;
    let mut output = String::new();
    for grapheme in line.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > content_width {
            break;
        }
        width += grapheme_width;
        output.push_str(grapheme);
    }
    output.push('…');
    Some(output)
}

pub fn quoted_display_line(value: &str) -> Option<String> {
    let line = bounded_line(value, 2)?;
    Some(format!("\"{line}\""))
}

fn bounded_line(value: &str, reserved: usize) -> Option<String> {
    let line = complete_line(value)?;
    let limit = DISPLAY_LIMIT - reserved;
    let count = line.chars().count();
    if count <= limit {
        return Some(line);
    }

    let mut output: String = line.chars().take(limit - 1).collect();
    output.push('…');
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_first_non_empty_trimmed_line() {
        assert_eq!(display_line(" \n  hello  \nworld"), Some("hello".into()));
        assert_eq!(display_line("\n \t"), None);
    }

    #[test]
    fn truncates_unicode_to_eighty_scalars_including_ellipsis() {
        let exact = "界".repeat(80);
        assert_eq!(display_line(&exact), Some(exact));

        let value = "界".repeat(81);
        let output = display_line(&value).unwrap();
        assert_eq!(output.chars().count(), 80);
        assert!(output.ends_with('…'));
        assert_eq!(output.chars().filter(|value| *value == '界').count(), 79);
    }

    #[test]
    fn quotes_display_line_within_the_eighty_scalar_limit() {
        assert_eq!(quoted_display_line(" hello "), Some("\"hello\"".into()));

        let output = quoted_display_line(&"界".repeat(79)).unwrap();
        assert_eq!(output.chars().count(), 80);
        assert!(output.starts_with('"'));
        assert!(output.ends_with("…\""));
        assert_eq!(output.chars().filter(|value| *value == '界').count(), 77);
    }

    #[test]
    fn bounds_tab_labels_by_grapheme_display_width() {
        assert_eq!(tab_label("abcdefghijklmno"), Some("abcdefghijklmno".into()));
        assert_eq!(
            tab_label("abcdefghijklmnop"),
            Some("abcdefghijklmn…".into())
        );
        assert_eq!(tab_label("界界界界界界界"), Some("界界界界界界界".into()));
        assert_eq!(
            tab_label("界界界界界界界界"),
            Some("界界界界界界界…".into())
        );
        assert_eq!(
            tab_label("👨‍👩‍👧‍👦 family planning notes"),
            Some("👨‍👩‍👧‍👦 family plan…".into())
        );
    }

    #[test]
    fn derives_sidebar_and_tab_bounds_independently_from_complete_line() {
        let grapheme = format!("a{}", "\u{301}".repeat(90));
        let source = complete_line(&format!("  {grapheme}  \nignored")).unwrap();

        let sidebar = display_line(&source).unwrap();
        assert_eq!(sidebar.chars().count(), 80);
        assert!(sidebar.ends_with('…'));
        assert_eq!(tab_label(&source), Some(grapheme));
    }
}
