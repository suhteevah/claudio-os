//! Minimal CSS selector matching.
//!
//! Supports a subset of CSS selectors sufficient for form detection:
//! - Tag name: `form`, `input`, `button`, `a`, `script`
//! - ID: `#login-form`
//! - Class: `.submit-btn`
//! - Attribute presence: `[type]`
//! - Attribute value: `[type="email"]`, `[name="password"]`
//! - Descendant combinator: `form input` (space-separated)
//! - Multiple selectors: `input, select, textarea` (comma-separated)

use alloc::string::String;
use alloc::vec::Vec;

use crate::parser::{Document, NodeData};

// --- Types ---------------------------------------------------------------

/// A parsed CSS selector (may contain comma-separated alternatives).
pub struct Selector {
    alternatives: Vec<SelectorChain>,
}

/// A descendant-combinator chain: `form input[type="email"]` becomes two parts.
struct SelectorChain {
    parts: Vec<SimpleSelector>,
}

/// A single simple selector matching one element.
struct SimpleSelector {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<AttrMatcher>,
}

struct AttrMatcher {
    name: String,
    value: Option<String>,
}

// --- Parsing -------------------------------------------------------------

impl Selector {
    /// Parse a CSS selector string. Returns `None` if the string is empty.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let mut alternatives = Vec::new();
        for alt in s.split(',') {
            let alt = alt.trim();
            if alt.is_empty() {
                continue;
            }
            if let Some(chain) = parse_chain(alt) {
                alternatives.push(chain);
            }
        }
        if alternatives.is_empty() {
            None
        } else {
            Some(Selector { alternatives })
        }
    }

    /// Check if a node matches this selector.
    pub fn matches(&self, doc: &Document, node_id: usize) -> bool {
        self.alternatives
            .iter()
            .any(|chain| chain_matches(doc, node_id, chain))
    }
}

fn parse_chain(s: &str) -> Option<SelectorChain> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for token in tokens {
        if let Some(simple) = parse_simple(token) {
            parts.push(simple);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(SelectorChain { parts })
    }
}

fn parse_simple(s: &str) -> Option<SimpleSelector> {
    let mut sel = SimpleSelector {
        tag: None,
        id: None,
        classes: Vec::new(),
        attrs: Vec::new(),
    };

    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut pos = 0;

    // Leading tag name
    if pos < len && bytes[pos] != b'#' && bytes[pos] != b'.' && bytes[pos] != b'[' {
        let start = pos;
        while pos < len && bytes[pos] != b'#' && bytes[pos] != b'.' && bytes[pos] != b'[' {
            pos += 1;
        }
        let tag = &s[start..pos];
        if !tag.is_empty() && tag != "*" {
            sel.tag = Some(String::from(tag.to_ascii_lowercase()));
        }
    }

    while pos < len {
        match bytes[pos] {
            b'#' => {
                pos += 1;
                let start = pos;
                while pos < len && bytes[pos] != b'#' && bytes[pos] != b'.' && bytes[pos] != b'[' {
                    pos += 1;
                }
                sel.id = Some(String::from(&s[start..pos]));
            }
            b'.' => {
                pos += 1;
                let start = pos;
                while pos < len && bytes[pos] != b'#' && bytes[pos] != b'.' && bytes[pos] != b'[' {
                    pos += 1;
                }
                if start < pos {
                    sel.classes.push(String::from(&s[start..pos]));
                }
            }
            b'[' => {
                pos += 1;
                let start = pos;
                while pos < len && bytes[pos] != b']' {
                    pos += 1;
                }
                let attr_str = &s[start..pos];
                if pos < len {
                    pos += 1;
                }
                if let Some(eq) = attr_str.find('=') {
                    let name = attr_str[..eq].trim().to_ascii_lowercase();
                    let mut value = attr_str[eq + 1..].trim();
                    if (value.starts_with('"') && value.ends_with('"'))
                        || (value.starts_with('\'') && value.ends_with('\''))
                    {
                        value = &value[1..value.len() - 1];
                    }
                    sel.attrs.push(AttrMatcher {
                        name,
                        value: Some(String::from(value)),
                    });
                } else {
                    let name = attr_str.trim().to_ascii_lowercase();
                    if !name.is_empty() {
                        sel.attrs.push(AttrMatcher { name, value: None });
                    }
                }
            }
            _ => {
                pos += 1;
            }
        }
    }

    if sel.tag.is_none() && sel.id.is_none() && sel.classes.is_empty() && sel.attrs.is_empty() {
        None
    } else {
        Some(sel)
    }
}

// --- Matching ------------------------------------------------------------

fn simple_matches(doc: &Document, node_id: usize, sel: &SimpleSelector) -> bool {
    let node = &doc.nodes[node_id];
    let (tag, attrs) = match &node.data {
        NodeData::Element { tag, attributes } => (tag.as_str(), attributes),
        _ => return false,
    };

    if let Some(ref expected_tag) = sel.tag {
        if tag != expected_tag.as_str() {
            return false;
        }
    }

    if let Some(ref expected_id) = sel.id {
        let node_id_val = attrs
            .iter()
            .find(|(k, _)| k == "id")
            .map(|(_, v)| v.as_str());
        if node_id_val != Some(expected_id.as_str()) {
            return false;
        }
    }

    if !sel.classes.is_empty() {
        let class_str = attrs
            .iter()
            .find(|(k, _)| k == "class")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        for expected in &sel.classes {
            if !class_str.split_whitespace().any(|c| c == expected.as_str()) {
                return false;
            }
        }
    }

    for am in &sel.attrs {
        let node_attr = attrs.iter().find(|(k, _)| k == am.name.as_str());
        match (&am.value, node_attr) {
            (None, None) => return false,
            (Some(_), None) => return false,
            (Some(expected), Some((_, actual))) => {
                if actual != expected {
                    return false;
                }
            }
            (None, Some(_)) => {}
        }
    }

    true
}

fn chain_matches(doc: &Document, node_id: usize, chain: &SelectorChain) -> bool {
    if chain.parts.is_empty() {
        return false;
    }
    let last = &chain.parts[chain.parts.len() - 1];
    if !simple_matches(doc, node_id, last) {
        return false;
    }
    if chain.parts.len() == 1 {
        return true;
    }

    let mut part_idx = chain.parts.len() - 2;
    let mut ancestor = doc.nodes[node_id].parent;

    loop {
        match ancestor {
            None => return false,
            Some(anc_id) => {
                if simple_matches(doc, anc_id, &chain.parts[part_idx]) {
                    if part_idx == 0 {
                        return true;
                    }
                    part_idx -= 1;
                }
                ancestor = doc.nodes[anc_id].parent;
            }
        }
    }
}

// --- Query ---------------------------------------------------------------

/// Select all nodes matching a selector.
pub fn select(doc: &Document, selector: &Selector) -> Vec<usize> {
    let mut results = Vec::new();
    for node in &doc.nodes {
        if selector.matches(doc, node.id) {
            results.push(node.id);
        }
    }
    results
}

// --- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn select_by_tag() {
        let doc = parse("<div><p>one</p><p>two</p><span>three</span></div>");
        let sel = Selector::parse("p").unwrap();
        assert_eq!(select(&doc, &sel).len(), 2);
    }

    #[test]
    fn select_by_id() {
        let doc = parse(r#"<div id="main"><p id="first">hello</p></div>"#);
        let sel = Selector::parse("#first").unwrap();
        let ids = select(&doc, &sel);
        assert_eq!(ids.len(), 1);
        assert_eq!(doc.tag_name(ids[0]), "p");
    }

    #[test]
    fn select_by_class() {
        let doc = parse(r#"<div class="a b"><span class="b c">x</span></div>"#);
        let sel = Selector::parse(".b").unwrap();
        assert_eq!(select(&doc, &sel).len(), 2);
    }

    #[test]
    fn select_by_attribute_presence() {
        let doc = parse(r#"<input type="text"><input><select name="x"></select>"#);
        let sel = Selector::parse("[type]").unwrap();
        assert_eq!(select(&doc, &sel).len(), 1);
    }

    #[test]
    fn select_by_attribute_value() {
        let doc = parse(r#"<input type="email"><input type="password"><input type="text">"#);
        let sel = Selector::parse(r#"[type="password"]"#).unwrap();
        assert_eq!(select(&doc, &sel).len(), 1);
    }

    #[test]
    fn select_descendant() {
        let doc = parse(
            r#"<form><input type="email"><div><input type="password"></div></form><input type="text">"#,
        );
        let sel = Selector::parse("form input").unwrap();
        assert_eq!(select(&doc, &sel).len(), 2);
    }

    #[test]
    fn select_comma_separated() {
        let doc = parse("<div><input><select></select><textarea></textarea><p>hi</p></div>");
        let sel = Selector::parse("input, select, textarea").unwrap();
        assert_eq!(select(&doc, &sel).len(), 3);
    }

    #[test]
    fn combined_tag_and_attr() {
        let doc = parse(r#"<input type="email"><div type="email"></div>"#);
        let sel = Selector::parse(r#"input[type="email"]"#).unwrap();
        let ids = select(&doc, &sel);
        assert_eq!(ids.len(), 1);
        assert_eq!(doc.tag_name(ids[0]), "input");
    }

    #[test]
    fn tag_class_combined() {
        let doc = parse(r#"<button class="submit">Go</button><div class="submit">x</div>"#);
        let sel = Selector::parse("button.submit").unwrap();
        assert_eq!(select(&doc, &sel).len(), 1);
    }
}
