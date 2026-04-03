//! Full-text search engine for ClaudioOS.
//!
//! Provides unified search across multiple sources:
//! - Files on mounted VFS (grep-like substring search)
//! - Man pages (built-in documentation)
//! - Shell history
//! - Agent conversation history (if persisted)
//!
//! ## Shell commands
//! - `search <query>` — search everything
//! - `search -f <query>` — files only
//! - `search -m <query>` — man pages only
//! - `search -h <query>` — shell history only
//!
//! Results are displayed with highlighted matches and relevance scores.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// The source of a search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchSource {
    /// A file on the VFS — contains the file path.
    File(String),
    /// A built-in man page — contains the command name.
    ManPage(String),
    /// Shell history — contains the history index.
    History(usize),
    /// Agent conversation — contains the conversation/agent identifier.
    Conversation(String),
}

impl core::fmt::Display for SearchSource {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SearchSource::File(path) => write!(f, "file:{}", path),
            SearchSource::ManPage(name) => write!(f, "man:{}", name),
            SearchSource::History(idx) => write!(f, "history:{}", idx),
            SearchSource::Conversation(id) => write!(f, "conv:{}", id),
        }
    }
}

/// A single search result.
#[derive(Clone, Debug)]
pub struct SearchResult {
    /// Where this result came from.
    pub source: SearchSource,
    /// Line number within the source (0 if not applicable).
    pub line_number: usize,
    /// Context text surrounding the match (the matching line or nearby text).
    pub context: String,
    /// Relevance score (higher = more relevant). Based on frequency and position.
    pub relevance_score: u32,
}

/// Search scope filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchScope {
    /// Search everything.
    All,
    /// Files only.
    Files,
    /// Man pages only.
    ManPages,
    /// Shell history only.
    History,
}

// ---------------------------------------------------------------------------
// Core search engine
// ---------------------------------------------------------------------------

/// Perform a full-text search across the specified scope.
///
/// The `query` is treated as a case-insensitive substring. Results are scored
/// by frequency (number of matches in the source) and position (earlier matches
/// rank higher).
///
/// # Arguments
/// * `query` — the search term
/// * `scope` — which sources to search
/// * `file_contents` — list of (path, content) tuples from the VFS
/// * `history_entries` — shell history entries
pub fn search(
    query: &str,
    scope: SearchScope,
    file_contents: &[(&str, &str)],
    history_entries: &[&str],
) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let query_lower = query.to_lowercase();

    if query_lower.is_empty() {
        return results;
    }

    // Search files.
    if scope == SearchScope::All || scope == SearchScope::Files {
        search_files(&query_lower, file_contents, &mut results);
    }

    // Search man pages.
    if scope == SearchScope::All || scope == SearchScope::ManPages {
        search_manpages(&query_lower, &mut results);
    }

    // Search shell history.
    if scope == SearchScope::All || scope == SearchScope::History {
        search_history(&query_lower, history_entries, &mut results);
    }

    // Sort by relevance (descending).
    results.sort_by(|a, b| b.relevance_score.cmp(&a.relevance_score));

    results
}

// ---------------------------------------------------------------------------
// File search (grep-like)
// ---------------------------------------------------------------------------

fn search_files(
    query: &str,
    files: &[(&str, &str)],
    results: &mut Vec<SearchResult>,
) {
    for (path, content) in files {
        let mut match_count = 0u32;

        for (line_num, line) in content.lines().enumerate() {
            let line_lower = line.to_lowercase();
            if line_lower.contains(query) {
                match_count += 1;

                // Limit results per file to avoid flooding.
                if match_count <= 10 {
                    // Calculate relevance: earlier lines get slight boost.
                    let position_bonus = if line_num < 10 { 5 } else if line_num < 50 { 2 } else { 0 };
                    // Frequency of query in this line.
                    let freq = count_occurrences(&line_lower, query);

                    results.push(SearchResult {
                        source: SearchSource::File(String::from(*path)),
                        line_number: line_num + 1,
                        context: truncate_context(line, 120),
                        relevance_score: 10 + freq as u32 * 5 + position_bonus,
                    });
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Man page search
// ---------------------------------------------------------------------------

fn search_manpages(query: &str, results: &mut Vec<SearchResult>) {
    // Access the built-in man pages.
    let pages = crate::manpages::search_keyword(query);

    for page in pages {
        // Score: man page matches are generally high-quality.
        let name_lower = page.name.to_lowercase();
        let desc_lower = page.description.to_lowercase();

        let mut score = 15u32;

        // Boost if query matches the command name directly.
        if name_lower.contains(query) {
            score += 20;
        }

        // Count occurrences in description.
        let freq = count_occurrences(&desc_lower, query);
        score += freq as u32 * 3;

        // Build context from synopsis + first line of description.
        let context = format!("{} — {}", page.synopsis, first_line(page.description));

        results.push(SearchResult {
            source: SearchSource::ManPage(String::from(page.name)),
            line_number: 0,
            context: truncate_context(&context, 120),
            relevance_score: score,
        });
    }
}

// ---------------------------------------------------------------------------
// History search
// ---------------------------------------------------------------------------

fn search_history(query: &str, entries: &[&str], results: &mut Vec<SearchResult>) {
    for (idx, entry) in entries.iter().enumerate() {
        let entry_lower = entry.to_lowercase();
        if entry_lower.contains(query) {
            let freq = count_occurrences(&entry_lower, query);
            // More recent history is more relevant.
            let recency_bonus = if idx > entries.len().saturating_sub(20) { 10 } else { 0 };

            results.push(SearchResult {
                source: SearchSource::History(idx + 1),
                line_number: 0,
                context: truncate_context(entry, 120),
                relevance_score: 8 + freq as u32 * 4 + recency_bonus,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Count non-overlapping occurrences of `needle` in `haystack`.
fn count_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        count += 1;
        start += pos + needle.len();
    }
    count
}

/// Get the first line of a multi-line string.
fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

/// Truncate a string to `max_len` characters, appending "..." if truncated.
fn truncate_context(s: &str, max_len: usize) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= max_len {
        String::from(trimmed)
    } else {
        let mut result = String::from(&trimmed[..max_len - 3]);
        result.push_str("...");
        result
    }
}

/// Highlight occurrences of `query` in `text` using ANSI escape codes.
/// Returns the text with matches wrapped in bold red.
fn highlight_matches(text: &str, query: &str) -> String {
    if query.is_empty() {
        return String::from(text);
    }

    let text_lower = text.to_lowercase();
    let query_lower = query.to_lowercase();
    let mut result = String::new();
    let mut last_end = 0;

    let text_bytes = text.as_bytes();
    let mut search_pos = 0;

    while let Some(pos) = text_lower[search_pos..].find(&query_lower) {
        let abs_pos = search_pos + pos;
        // Append text before the match.
        result.push_str(&text[last_end..abs_pos]);
        // Append highlighted match (bold red).
        result.push_str("\x1b[1;31m");
        result.push_str(&text[abs_pos..abs_pos + query.len()]);
        result.push_str("\x1b[0m");
        last_end = abs_pos + query.len();
        search_pos = last_end;
    }

    // Append remaining text.
    result.push_str(&text[last_end..]);
    result
}

// ---------------------------------------------------------------------------
// Shell command handler
// ---------------------------------------------------------------------------

/// Handle `search` shell commands. Returns output string.
///
/// - `search <query>` — search everything
/// - `search -f <query>` — files only
/// - `search -m <query>` — man pages only
/// - `search -h <query>` — history only
pub fn handle_command(
    args: &str,
    file_contents: &[(&str, &str)],
    history_entries: &[&str],
) -> String {
    let parts: Vec<&str> = args.split_whitespace().collect();

    if parts.is_empty() {
        return "Usage: search [-f|-m|-h] <query>\n\
                \n\
                Options:\n\
                \x20 -f    Search files only\n\
                \x20 -m    Search man pages only\n\
                \x20 -h    Search shell history only\n\
                \x20 (none) Search everything\n\
                \n\
                Examples:\n\
                \x20 search network     Search all sources for 'network'\n\
                \x20 search -f TODO     Search files for 'TODO'\n\
                \x20 search -m ping     Search man pages for 'ping'\n\
                \x20 search -h curl     Search history for 'curl'\n".into();
    }

    let (scope, query) = if parts[0].starts_with('-') {
        let flag = parts[0];
        let q = parts[1..].join(" ");
        let s = match flag {
            "-f" => SearchScope::Files,
            "-m" => SearchScope::ManPages,
            "-h" => SearchScope::History,
            _ => return format!("Unknown flag: {}. Use -f, -m, or -h.\n", flag),
        };
        (s, q)
    } else {
        (SearchScope::All, parts.join(" "))
    };

    if query.is_empty() {
        return "Error: no search query provided.\n".into();
    }

    let results = search(&query, scope, file_contents, history_entries);

    if results.is_empty() {
        return format!("No results found for '{}'.\n", query);
    }

    let max_display = 50;
    let total = results.len();
    let display_count = core::cmp::min(total, max_display);

    let mut out = format!(
        "Search results for '{}' ({} matches{}):\n\n",
        query,
        total,
        if total > max_display { format!(", showing top {}", max_display) } else { String::new() }
    );

    for result in results.iter().take(display_count) {
        // Source label with color.
        let source_label = match &result.source {
            SearchSource::File(path) => format!("\x1b[36m{}\x1b[0m", path),
            SearchSource::ManPage(name) => format!("\x1b[33mman({})\x1b[0m", name),
            SearchSource::History(idx) => format!("\x1b[35mhistory[{}]\x1b[0m", idx),
            SearchSource::Conversation(id) => format!("\x1b[34mconv:{}\x1b[0m", id),
        };

        // Line number if applicable.
        let line_info = if result.line_number > 0 {
            format!(":{}", result.line_number)
        } else {
            String::new()
        };

        // Relevance indicator.
        let relevance = if result.relevance_score >= 30 {
            "\x1b[32m***\x1b[0m"
        } else if result.relevance_score >= 20 {
            "\x1b[33m** \x1b[0m"
        } else {
            "\x1b[90m*  \x1b[0m"
        };

        let highlighted_context = highlight_matches(&result.context, &query);

        out.push_str(&format!(
            "  {} {}{} {}\n",
            relevance, source_label, line_info, highlighted_context
        ));
    }

    if total > max_display {
        out.push_str(&format!("\n  ... and {} more results.\n", total - max_display));
    }

    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Agent tool interface
// ---------------------------------------------------------------------------

/// Handle the `search` agent tool call.
///
/// Input JSON: { "query": "...", "scope": "all"|"files"|"manpages"|"history" }
/// Returns formatted search results.
pub fn tool_search(
    input: &str,
    file_contents: &[(&str, &str)],
    history_entries: &[&str],
) -> String {
    let query = extract_json_str(input, "query").unwrap_or_default();
    let scope_str = extract_json_str(input, "scope").unwrap_or_else(|| String::from("all"));

    if query.is_empty() {
        return "Error: 'query' field is required.".into();
    }

    let scope = match &*scope_str {
        "files" | "file" => SearchScope::Files,
        "manpages" | "man" => SearchScope::ManPages,
        "history" => SearchScope::History,
        _ => SearchScope::All,
    };

    let results = search(&query, scope, file_contents, history_entries);

    if results.is_empty() {
        return format!("No results found for '{}'.", query);
    }

    let max_display = 30;
    let total = results.len();
    let mut out = format!("Found {} results for '{}':\n", total, query);

    for result in results.iter().take(max_display) {
        let line_info = if result.line_number > 0 {
            format!(":{}", result.line_number)
        } else {
            String::new()
        };

        out.push_str(&format!(
            "  [{}] {}{}: {}\n",
            result.relevance_score,
            result.source,
            line_info,
            result.context,
        ));
    }

    if total > max_display {
        out.push_str(&format!("... and {} more results.\n", total - max_display));
    }

    out
}

// ---------------------------------------------------------------------------
// Minimal JSON extraction (shared with email module pattern)
// ---------------------------------------------------------------------------

fn extract_json_str(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\"", key);
    let start = json.find(&needle)?;
    let after_key = &json[start + needle.len()..];
    let after_colon = after_key.trim_start().strip_prefix(':')?;
    let after_ws = after_colon.trim_start();
    if after_ws.starts_with('"') {
        let content = &after_ws[1..];
        let mut result = String::new();
        let mut chars = content.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(result),
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        match escaped {
                            'n' => result.push('\n'),
                            't' => result.push('\t'),
                            '"' => result.push('"'),
                            '\\' => result.push('\\'),
                            _ => {
                                result.push('\\');
                                result.push(escaped);
                            }
                        }
                    }
                }
                _ => result.push(c),
            }
        }
        Some(result)
    } else {
        None
    }
}
