/// Truncate a string to at most `max_bytes` bytes without splitting a UTF-8
/// character. Byte-index slicing (`&s[..n]`) panics when `n` is not a char
/// boundary — e.g. Thai text or emoji in agent output.
pub fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Truncate and append "..." when the string was shortened.
pub fn truncate_utf8_ellipsis(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        s.to_string()
    } else {
        format!("{}...", truncate_utf8(s, max_bytes))
    }
}

/// Largest byte index `<= max_bytes` that lies on a UTF-8 char boundary.
/// Use this when you need the index itself (e.g. to slice `&s[..idx]`);
/// prefer `truncate_utf8` when you just want the truncated string.
pub fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut i = max_bytes;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thai_text_does_not_panic() {
        // Thai chars are 3 bytes each — every cut point that isn't a multiple
        // of 3 would panic with plain byte slicing.
        let thai = "สวัสดีครับ ผลการวิเคราะห์ข้อมูลเสร็จสมบูรณ์แล้ว".repeat(10);
        for max in 0..200 {
            let t = truncate_utf8(&thai, max);
            assert!(t.len() <= max);
            assert!(thai.starts_with(t));
        }
    }

    #[test]
    fn ascii_unchanged() {
        assert_eq!(truncate_utf8("hello", 10), "hello");
        assert_eq!(truncate_utf8("hello world", 5), "hello");
        assert_eq!(truncate_utf8_ellipsis("hello", 3), "hel...");
    }

    #[test]
    fn floor_char_boundary_never_splits_utf8() {
        let thai = "สวัสดีครับ".repeat(5);
        for max in 0..=thai.len() + 5 {
            let idx = floor_char_boundary(&thai, max);
            assert!(idx <= max.min(thai.len()));
            assert!(thai.is_char_boundary(idx));
            // Slicing at the returned index must never panic.
            let _ = &thai[..idx];
        }
        assert_eq!(floor_char_boundary("abc", 10), 3);
        assert_eq!(floor_char_boundary("abc", 2), 2);
    }
}
