//! Utility functions.

/// Ellipsizes a string if it exceeds `max_chars`.
pub fn ellipsize(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        truncated.push('…');
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ellipsize() {
        assert_eq!(ellipsize("hello", 10), "hello");
        assert_eq!(ellipsize("hello", 5), "hello");
        assert_eq!(ellipsize("hello world", 5), "hell…");
        assert_eq!(ellipsize("こんにちは世界", 5), "こんにち…");
    }
}
