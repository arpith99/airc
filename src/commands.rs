use crate::client::IrcClient;
use crate::ui::print_line;
use once_cell::sync::Lazy;
use regex::Regex;
use std::sync::Arc;

// Compile regexes once at startup
static SEARCH_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"/s(earch)? (?P<search_term>.*)").unwrap());

static ENTRY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^/(?P<entry_num>\d+)$").unwrap());

// Case-insensitive substring search without allocation
fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }

    let haystack_lower: String = haystack.chars().flat_map(|c| c.to_lowercase()).collect();
    let needle_lower: String = needle.chars().flat_map(|c| c.to_lowercase()).collect();
    haystack_lower.contains(&needle_lower)
}

// Local command to search the search results
pub(crate) async fn handle_search_results(client: Arc<IrcClient>, search_term: &str) {
    // Collect matching indices and lines to avoid holding lock during I/O
    let matches: Vec<(usize, String)> = {
        let list = client.search_results.lock().await;
        list.iter()
            .enumerate()
            .filter(|(_, book_line)| contains_ignore_case(book_line, search_term))
            .map(|(i, book_line)| (i, book_line.clone()))
            .collect()
    };

    if matches.is_empty() {
        print_line(&format!("No results found for '{}'\n", search_term), true);
    } else {
        for (i, book_line) in matches {
            print_line(&format!("{}: {}\n", i, book_line), true);
        }
    }
}

// Returns Option<String> - Some(msg) if should send to IRC, None if shouldn't
pub(crate) async fn process_command(client: Arc<IrcClient>, command: &str) -> Option<String> {
    // JOIN command
    if command == "/join" || command == "/j" {
        return Some(format!("JOIN {}\r\n", client.config.channel));
    }

    // QUIT command
    if command.starts_with("/quit") || command.starts_with("/q ") || command == "/q" {
        // Extract optional quit message
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

    // SEARCH command
    if let Some(caps) = SEARCH_RE.captures(command) {
        let search_term = caps.name("search_term").unwrap().as_str();
        print_line(&format!("Searching for: {}\n", search_term), true);
        return Some(format!(
            "PRIVMSG {} :@search {}\r\n",
            client.config.channel, search_term
        ));
    }

    // ENTRY NUMBER selection
    if let Some(caps) = ENTRY_RE.captures(command) {
        let entry_str = caps.name("entry_num").unwrap().as_str();

        match entry_str.parse::<usize>() {
            Ok(entry_num) => {
                print_line(&format!("Requesting entry number: {}\n", entry_num), true);

                // Look up the actual book entry
                let results = client.search_results.lock().await;
                if let Some(book_entry) = results.get(entry_num) {
                    print_line(&format!("Book entry: {}\n", book_entry), true);
                    return Some(format!("PRIVMSG {} :{}\r\n", client.config.channel, book_entry));
                } else {
                    print_line(
                        &format!("Entry number {} not found in search results\n", entry_num),
                        true,
                    );
                    return None; // Don't send anything
                }
            }
            Err(_) => {
                print_line(&format!("Invalid entry number: '{}'\n", entry_str), true);
                return None;
            }
        }
    }

    // Default: treat as raw IRC command
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
}
