use once_cell::sync::Lazy;
use regex::Regex;

// Compile regexes once at startup
static SEARCH_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/s(earch)? (?P<search_term>.*)").unwrap());

static ENTRY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^/(?P<entry_num>\d+)$").unwrap());

/// Case-insensitive substring search without allocating per-char.
pub(crate) fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let haystack_lower: String = haystack.chars().flat_map(|c| c.to_lowercase()).collect();
    let needle_lower: String = needle.chars().flat_map(|c| c.to_lowercase()).collect();
    haystack_lower.contains(&needle_lower)
}

/// `/ss <term>` — local filter over already-downloaded results. Returns the term.
pub(crate) fn local_search_term(input: &str) -> Option<&str> {
    input.strip_prefix("/ss ").map(str::trim)
}

/// `/<n>` — request the n-th book entry. Returns the parsed index.
pub(crate) fn entry_number(input: &str) -> Option<usize> {
    ENTRY_RE
        .captures(input)?
        .name("entry_num")?
        .as_str()
        .parse()
        .ok()
}

/// Map a user input line to the IRC string to send, or `None` if nothing
/// should be sent. Pure: `/ss` and `/<n>` are handled by the caller (the render
/// loop) since they depend on the in-memory book list.
pub(crate) fn process_command(command: &str, channel: &str) -> Option<String> {
    // JOIN
    if command == "/join" || command == "/j" {
        return Some(format!("JOIN {}\r\n", channel));
    }

    // QUIT (with optional message)
    if command.starts_with("/quit") || command.starts_with("/q ") || command == "/q" {
        let quit_msg = command
            .strip_prefix("/quit ")
            .or_else(|| command.strip_prefix("/q "))
            .unwrap_or("");
        return if quit_msg.is_empty() {
            Some("QUIT\r\n".to_string())
        } else {
            Some(format!("QUIT :{}\r\n", quit_msg))
        };
    }

    // SEARCH
    if let Some(caps) = SEARCH_RE.captures(command) {
        let search_term = caps.name("search_term").unwrap().as_str();
        return Some(format!("PRIVMSG {} :@search {}\r\n", channel, search_term));
    }

    // Default: raw IRC command (strip a single leading slash)
    Some(format!(
        "{}\r\n",
        command.strip_prefix('/').unwrap_or(command).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_regex() {
        // Test /search and /s commands
        assert!(SEARCH_RE.is_match("/search rust programming"));
        assert!(SEARCH_RE.is_match("/s python"));

        let caps1 = SEARCH_RE.captures("/search rust programming").unwrap();
        assert_eq!(
            caps1.name("search_term").unwrap().as_str(),
            "rust programming"
        );

        let caps2 = SEARCH_RE.captures("/s python").unwrap();
        assert_eq!(caps2.name("search_term").unwrap().as_str(), "python");

        // Test edge case - matches but captures empty string
        assert!(SEARCH_RE.is_match("/search "));
        let caps3 = SEARCH_RE.captures("/search ").unwrap();
        assert_eq!(caps3.name("search_term").unwrap().as_str(), "");

        // Test invalid
        assert!(!SEARCH_RE.is_match("/se"));
        assert!(!SEARCH_RE.is_match("/search"));
    }

    #[test]
    fn test_entry_regex() {
        // Test entry number patterns
        assert!(ENTRY_RE.is_match("/123"));
        assert!(ENTRY_RE.is_match("/0"));
        assert!(ENTRY_RE.is_match("/999"));

        let caps = ENTRY_RE.captures("/42").unwrap();
        assert_eq!(caps.name("entry_num").unwrap().as_str(), "42");

        // Test invalid
        assert!(!ENTRY_RE.is_match("/abc"));
        assert!(!ENTRY_RE.is_match("123"));
    }

    #[test]
    fn test_process_command_join_and_quit() {
        assert_eq!(
            process_command("/join", "#bookz"),
            Some("JOIN #bookz\r\n".to_string())
        );
        assert_eq!(process_command("/q", "#bookz"), Some("QUIT\r\n".to_string()));
        assert_eq!(
            process_command("/quit bye now", "#bookz"),
            Some("QUIT :bye now\r\n".to_string())
        );
    }

    #[test]
    fn test_process_command_search_and_raw() {
        assert_eq!(
            process_command("/s rust", "#bookz"),
            Some("PRIVMSG #bookz :@search rust\r\n".to_string())
        );
        assert_eq!(
            process_command("/whois bob", "#bookz"),
            Some("whois bob\r\n".to_string())
        );
    }

    #[test]
    fn test_entry_number() {
        assert_eq!(entry_number("/5"), Some(5));
        assert_eq!(entry_number("/0"), Some(0));
        assert_eq!(entry_number("/abc"), None);
        assert_eq!(entry_number("5"), None);
    }

    #[test]
    fn test_local_search_term() {
        assert_eq!(local_search_term("/ss async"), Some("async"));
        assert_eq!(local_search_term("/s async"), None);
        assert_eq!(local_search_term("hello"), None);
    }
}
