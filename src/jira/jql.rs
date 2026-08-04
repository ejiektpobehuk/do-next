//! JQL string helpers.

/// Escape a value for use inside a double-quoted JQL string literal.
///
/// Mostly defensive: Cloud account ids are alphanumeric. It matters on Data
/// Center, where the user literal is a username that may contain a quote or
/// backslash, and where an unescaped one turns a query into a syntax error
/// rather than a wrong answer.
pub fn escape_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_plain_values_alone() {
        assert_eq!(escape_string("557058:abc-123"), "557058:abc-123");
        assert_eq!(escape_string("jsmith"), "jsmith");
    }

    #[test]
    fn escapes_quotes_and_backslashes() {
        assert_eq!(escape_string(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape_string(r"a\b"), r"a\\b");
        // Backslash first, so an escaped quote is not double-escaped.
        assert_eq!(escape_string(r#"a\"b"#), r#"a\\\"b"#);
    }
}
