//! Text extraction from a parsed DOM tree.
//!
//! Extract visible text, the page title, and links.

use alloc::string::String;
use alloc::vec::Vec;

use crate::parser::{Document, NodeData};

// --- Invisible/block tags ------------------------------------------------

const INVISIBLE_TAGS: &[&str] = &[
    "script", "style", "noscript", "template", "head", "meta", "link",
];

fn is_invisible(tag: &str) -> bool {
    INVISIBLE_TAGS.contains(&tag)
}

const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "br",
    "hr",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "tr",
    "blockquote",
    "pre",
    "section",
    "article",
    "header",
    "footer",
    "nav",
    "main",
    "form",
    "fieldset",
    "table",
];

fn is_block(tag: &str) -> bool {
    BLOCK_TAGS.contains(&tag)
}

// --- Public API ----------------------------------------------------------

/// Extract all visible text from the document, stripping tags.
/// Block-level elements produce line breaks; inline elements produce spaces.
pub fn extract_text(doc: &Document) -> String {
    let mut out = String::new();
    collect_visible_text(doc, 0, &mut out);
    clean_whitespace(&out)
}

/// Extract the page title from the `<title>` tag.
pub fn extract_title(doc: &Document) -> Option<String> {
    for node in &doc.nodes {
        if let NodeData::Element { tag, .. } = &node.data {
            if tag == "title" {
                let text = doc.inner_text(node.id);
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(String::from(trimmed));
                }
            }
        }
    }
    None
}

/// Find all links in the document. Returns `(href, link_text)` pairs.
pub fn extract_links(doc: &Document) -> Vec<(String, String)> {
    let mut links = Vec::new();
    for node in &doc.nodes {
        if let NodeData::Element { tag, attributes } = &node.data {
            if tag == "a" {
                let href = attributes
                    .iter()
                    .find(|(k, _)| k == "href")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                if !href.is_empty() {
                    let text = doc.inner_text(node.id);
                    let text = text.trim();
                    links.push((href, String::from(text)));
                }
            }
        }
    }
    links
}

// --- Helpers -------------------------------------------------------------

fn collect_visible_text(doc: &Document, node_id: usize, out: &mut String) {
    let node = &doc.nodes[node_id];
    match &node.data {
        NodeData::Text(text) => {
            if !out.is_empty() && !out.ends_with(' ') && !out.ends_with('\n') {
                out.push(' ');
            }
            out.push_str(text);
        }
        NodeData::Element { tag, .. } => {
            if is_invisible(tag) {
                return;
            }
            let block = is_block(tag);
            if block && !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            for &child_id in &node.children {
                collect_visible_text(doc, child_id, out);
            }
            if block && !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
        }
        NodeData::Comment(_) => {}
    }
}

fn clean_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    let mut last_was_newline = false;

    for c in s.chars() {
        if c == '\n' {
            if !last_was_newline {
                out.push('\n');
                last_was_newline = true;
                last_was_space = false;
            }
        } else if c.is_whitespace() {
            if !last_was_space && !last_was_newline {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(c);
            last_was_space = false;
            last_was_newline = false;
        }
    }

    let trimmed = out.trim();
    String::from(trimmed)
}

// --- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn extract_simple_text() {
        let doc = parse("<html><body><p>Hello</p><p>World</p></body></html>");
        let text = extract_text(&doc);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
    }

    #[test]
    fn scripts_excluded() {
        let doc =
            parse("<body><p>Visible</p><script>var x = 1;</script><p>Also visible</p></body>");
        let text = extract_text(&doc);
        assert!(text.contains("Visible"));
        assert!(text.contains("Also visible"));
        assert!(!text.contains("var x"));
    }

    #[test]
    fn extract_title_tag() {
        let doc = parse("<html><head><title>My Page Title</title></head><body></body></html>");
        assert_eq!(extract_title(&doc), Some(String::from("My Page Title")));
    }

    #[test]
    fn extract_title_missing() {
        let doc = parse("<html><head></head><body><p>no title</p></body></html>");
        assert!(extract_title(&doc).is_none());
    }

    #[test]
    fn extract_links_basic() {
        let doc = parse(
            r#"
            <body>
                <a href="https://example.com">Example</a>
                <a href="/about">About Us</a>
                <a>No href</a>
            </body>
        "#,
        );
        let links = extract_links(&doc);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].0, "https://example.com");
        assert_eq!(links[0].1, "Example");
        assert_eq!(links[1].0, "/about");
        assert_eq!(links[1].1, "About Us");
    }

    #[test]
    fn block_elements_produce_newlines() {
        let doc = parse("<div><h1>Title</h1><p>Para one</p><p>Para two</p></div>");
        let text = extract_text(&doc);
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        assert!(lines.len() >= 3, "Expected >= 3 lines, got: {:?}", lines);
    }

    #[test]
    fn style_excluded() {
        let doc =
            parse("<head><style>body { color: red; }</style></head><body><p>Hello</p></body>");
        let text = extract_text(&doc);
        assert!(!text.contains("color"));
        assert!(text.contains("Hello"));
    }

    #[test]
    fn nested_link_text() {
        let doc = parse(r#"<a href="/x"><span>Click</span> <em>here</em></a>"#);
        let links = extract_links(&doc);
        assert_eq!(links.len(), 1);
        assert!(links[0].1.contains("Click"));
        assert!(links[0].1.contains("here"));
    }
}
