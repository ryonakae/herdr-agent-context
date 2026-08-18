pub const DISPLAY_LIMIT: usize = 80;

pub fn display_line(value: &str) -> Option<String> {
    bounded_line(value, 0)
}

pub fn quoted_display_line(value: &str) -> Option<String> {
    let line = bounded_line(value, 2)?;
    Some(format!("\"{line}\""))
}

fn bounded_line(value: &str, reserved: usize) -> Option<String> {
    let line = value.lines().map(str::trim).find(|line| !line.is_empty())?;
    let limit = DISPLAY_LIMIT - reserved;
    let count = line.chars().count();
    if count <= limit {
        return Some(line.to_owned());
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
}
