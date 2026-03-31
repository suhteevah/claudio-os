//! Simple HTML tokenizer and tree builder.
//!
//! Produces a flat [`Document`] with parent/child indices. Handles start tags,
//! end tags, self-closing tags, text, comments, and basic HTML entities.
//! Gracefully handles unclosed tags.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

// --- DOM types -----------------------------------------------------------

/// A parsed HTML document represented as a flat node arena.
pub struct Document {
    pub nodes: Vec<Node>,
}

/// A single node in the document tree.
pub struct Node {
    pub id: usize,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub data: NodeData,
}

/// The payload of a node.
pub enum NodeData {
    /// An HTML element with tag name and attributes.
    Element {
        tag: String,
        attributes: Vec<(String, String)>,
    },
    /// A run of text content.
    Text(String),
    /// An HTML comment.
    Comment(String),
}

// --- Void elements -------------------------------------------------------

const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

fn is_void_element(tag: &str) -> bool {
    VOID_ELEMENTS.contains(&tag)
}

// --- Entity decoding -----------------------------------------------------

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '&' {
            let mut entity = String::new();
            let mut found_semi = false;
            for _ in 0..10 {
                match chars.peek() {
                    Some(&';') => {
                        chars.next();
                        found_semi = true;
                        break;
                    }
                    Some(&c2) if c2.is_ascii_alphanumeric() || c2 == '#' => {
                        entity.push(c2);
                        chars.next();
                    }
                    _ => break,
                }
            }
            if found_semi {
                match entity.as_str() {
                    "amp" => out.push('&'),
                    "lt" => out.push('<'),
                    "gt" => out.push('>'),
                    "quot" => out.push('"'),
                    "apos" => out.push('\''),
                    "nbsp" => out.push('\u{00A0}'),
                    other => {
                        if let Some(rest) = other.strip_prefix('#') {
                            let code = if let Some(hex) = rest.strip_prefix('x') {
                                u32::from_str_radix(hex, 16).ok()
                            } else {
                                rest.parse::<u32>().ok()
                            };
                            if let Some(cp) = code.and_then(char::from_u32) {
                                out.push(cp);
                            } else {
                                out.push('&');
                                out.push_str(other);
                                out.push(';');
                            }
                        } else {
                            out.push('&');
                            out.push_str(other);
                            out.push(';');
                        }
                    }
                }
            } else {
                out.push('&');
                out.push_str(&entity);
            }
        } else {
            out.push(c);
        }
    }

    out
}

// --- Parser --------------------------------------------------------------

/// Parse an HTML string into a [`Document`].
pub fn parse(html: &str) -> Document {
    let mut doc = Document { nodes: Vec::new() };

    // Virtual root node at index 0
    doc.nodes.push(Node {
        id: 0,
        parent: None,
        children: Vec::new(),
        data: NodeData::Element {
            tag: String::from("[root]"),
            attributes: Vec::new(),
        },
    });

    let mut current_parent: usize = 0;
    let bytes = html.as_bytes();
    let len = bytes.len();
    let mut pos: usize = 0;

    while pos < len {
        if bytes[pos] == b'<' {
            // Comment: <!--...-->
            if pos + 3 < len && &bytes[pos..pos + 4] == b"<!--" {
                if let Some(end) = find_comment_end(html, pos + 4) {
                    let comment_text = &html[pos + 4..end];
                    let node_id = doc.nodes.len();
                    doc.nodes.push(Node {
                        id: node_id,
                        parent: Some(current_parent),
                        children: Vec::new(),
                        data: NodeData::Comment(String::from(comment_text)),
                    });
                    doc.nodes[current_parent].children.push(node_id);
                    pos = end + 3; // skip past "-->"
                    continue;
                }
            }

            // Doctype / processing instructions: skip
            if pos + 1 < len && (bytes[pos + 1] == b'!' || bytes[pos + 1] == b'?') {
                if let Some(gt) = find_byte(bytes, b'>', pos + 1) {
                    pos = gt + 1;
                } else {
                    pos += 1;
                }
                continue;
            }

            // End tag: </...>
            if pos + 1 < len && bytes[pos + 1] == b'/' {
                if let Some(gt) = find_byte(bytes, b'>', pos + 2) {
                    let tag_name = html[pos + 2..gt].trim().to_ascii_lowercase();
                    if !tag_name.is_empty() {
                        current_parent = close_tag(&doc, current_parent, &tag_name);
                    }
                    pos = gt + 1;
                    continue;
                }
            }

            // Start tag
            if let Some(gt) = find_tag_end(bytes, pos + 1) {
                let self_closing = gt > 0 && bytes[gt - 1] == b'/';
                let tag_content_end = if self_closing { gt - 1 } else { gt };
                let tag_content = &html[pos + 1..tag_content_end];

                if let Some((tag_name, attrs)) = parse_tag(tag_content) {
                    let tag_lower = tag_name.to_ascii_lowercase();

                    // Raw-text elements: script and style
                    if tag_lower == "script" || tag_lower == "style" {
                        let node_id = doc.nodes.len();
                        doc.nodes.push(Node {
                            id: node_id,
                            parent: Some(current_parent),
                            children: Vec::new(),
                            data: NodeData::Element {
                                tag: tag_lower.clone(),
                                attributes: attrs,
                            },
                        });
                        doc.nodes[current_parent].children.push(node_id);

                        let close_str = format!("</{}", tag_lower);
                        pos = gt + 1;
                        if let Some(close_start) = find_substr_ci(html, &close_str, pos) {
                            let inner = &html[pos..close_start];
                            if !inner.trim().is_empty() {
                                let text_id = doc.nodes.len();
                                doc.nodes.push(Node {
                                    id: text_id,
                                    parent: Some(node_id),
                                    children: Vec::new(),
                                    data: NodeData::Text(String::from(inner.trim())),
                                });
                                doc.nodes[node_id].children.push(text_id);
                            }
                            if let Some(close_gt) = find_byte(bytes, b'>', close_start) {
                                pos = close_gt + 1;
                            } else {
                                pos = len;
                            }
                        }
                        continue;
                    }

                    // Auto-close for elements like <p>, <li> etc.
                    if should_auto_close(&doc, current_parent, &tag_lower) {
                        current_parent = close_tag(&doc, current_parent, &tag_lower);
                    }

                    let node_id = doc.nodes.len();
                    doc.nodes.push(Node {
                        id: node_id,
                        parent: Some(current_parent),
                        children: Vec::new(),
                        data: NodeData::Element {
                            tag: tag_lower.clone(),
                            attributes: attrs,
                        },
                    });
                    doc.nodes[current_parent].children.push(node_id);

                    if !self_closing && !is_void_element(&tag_lower) {
                        current_parent = node_id;
                    }
                }

                pos = gt + 1;
                continue;
            }
        }

        // Text content
        let text_start = pos;
        while pos < len && bytes[pos] != b'<' {
            pos += 1;
        }
        let raw_text = &html[text_start..pos];
        let trimmed = raw_text.trim();
        if !trimmed.is_empty() {
            let decoded = decode_entities(trimmed);
            let node_id = doc.nodes.len();
            doc.nodes.push(Node {
                id: node_id,
                parent: Some(current_parent),
                children: Vec::new(),
                data: NodeData::Text(decoded),
            });
            doc.nodes[current_parent].children.push(node_id);
        }
    }

    doc
}

// --- Tag parsing helpers -------------------------------------------------

fn parse_tag(content: &str) -> Option<(String, Vec<(String, String)>)> {
    let content = content.trim();
    if content.is_empty() {
        return None;
    }
    let (tag_name, rest) = match content.find(|c: char| c.is_ascii_whitespace()) {
        Some(idx) => (&content[..idx], content[idx..].trim_start()),
        None => (content, ""),
    };
    if tag_name.is_empty() {
        return None;
    }
    let attrs = parse_attributes(rest);
    Some((String::from(tag_name), attrs))
}

fn parse_attributes(s: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let mut pos = 0;
    let bytes = s.as_bytes();
    let len = bytes.len();

    while pos < len {
        // Skip whitespace
        while pos < len && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos >= len {
            break;
        }

        // Attribute name
        let name_start = pos;
        while pos < len && bytes[pos] != b'=' && !bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let name = s[name_start..pos].to_ascii_lowercase();
        if name.is_empty() {
            pos += 1;
            continue;
        }

        // Skip whitespace
        while pos < len && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }

        if pos < len && bytes[pos] == b'=' {
            pos += 1;
            while pos < len && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }

            if pos < len && (bytes[pos] == b'"' || bytes[pos] == b'\'') {
                let quote = bytes[pos];
                pos += 1;
                let val_start = pos;
                while pos < len && bytes[pos] != quote {
                    pos += 1;
                }
                let value = decode_entities(&s[val_start..pos]);
                if pos < len {
                    pos += 1;
                }
                attrs.push((name, value));
            } else {
                // Unquoted value
                let val_start = pos;
                while pos < len && !bytes[pos].is_ascii_whitespace() && bytes[pos] != b'>' {
                    pos += 1;
                }
                let value = decode_entities(&s[val_start..pos]);
                attrs.push((name, value));
            }
        } else {
            // Boolean attribute
            attrs.push((name, String::new()));
        }
    }

    attrs
}

fn close_tag(doc: &Document, current: usize, tag: &str) -> usize {
    let mut walk = current;
    loop {
        if let NodeData::Element { tag: ref t, .. } = doc.nodes[walk].data {
            if t == tag {
                return doc.nodes[walk].parent.unwrap_or(0);
            }
        }
        match doc.nodes[walk].parent {
            Some(p) if p != walk => walk = p,
            _ => break,
        }
    }
    current
}

fn should_auto_close(doc: &Document, current: usize, new_tag: &str) -> bool {
    if let NodeData::Element { tag: ref t, .. } = doc.nodes[current].data {
        let auto_close_same = ["p", "li", "dt", "dd", "option", "tr", "td", "th"];
        if t == new_tag && auto_close_same.contains(&new_tag) {
            return true;
        }
    }
    false
}

fn find_byte(bytes: &[u8], needle: u8, start: usize) -> Option<usize> {
    for i in start..bytes.len() {
        if bytes[i] == needle {
            return Some(i);
        }
    }
    None
}

fn find_tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let len = bytes.len();
    let mut pos = start;
    while pos < len {
        match bytes[pos] {
            b'"' | b'\'' => {
                let quote = bytes[pos];
                pos += 1;
                while pos < len && bytes[pos] != quote {
                    pos += 1;
                }
                if pos < len {
                    pos += 1;
                }
            }
            b'>' => return Some(pos),
            _ => pos += 1,
        }
    }
    None
}

fn find_comment_end(html: &str, start: usize) -> Option<usize> {
    html[start..].find("-->").map(|i| start + i)
}

fn find_substr_ci(haystack: &str, needle: &str, start: usize) -> Option<usize> {
    let h = &haystack[start..];
    let n_lower = needle.to_ascii_lowercase();
    let n_len = n_lower.len();
    if n_len > h.len() {
        return None;
    }
    for i in 0..=(h.len() - n_len) {
        if h[i..i + n_len].eq_ignore_ascii_case(&n_lower) {
            return Some(start + i);
        }
    }
    None
}

// --- Document helpers ----------------------------------------------------

impl Document {
    /// Get a node by id.
    pub fn get(&self, id: usize) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// Get the tag name of a node (empty string for non-elements).
    pub fn tag_name(&self, id: usize) -> &str {
        match &self.nodes[id].data {
            NodeData::Element { tag, .. } => tag.as_str(),
            _ => "",
        }
    }

    /// Get an attribute value from an element node.
    pub fn attr(&self, id: usize, name: &str) -> Option<&str> {
        if let NodeData::Element { attributes, .. } = &self.nodes[id].data {
            for (k, v) in attributes {
                if k == name {
                    return Some(v.as_str());
                }
            }
        }
        None
    }

    /// Get all text content under a node (recursive).
    pub fn inner_text(&self, id: usize) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    fn collect_text(&self, id: usize, out: &mut String) {
        match &self.nodes[id].data {
            NodeData::Text(t) => {
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                out.push_str(t);
            }
            NodeData::Element { .. } => {
                for &child in &self.nodes[id].children {
                    self.collect_text(child, out);
                }
            }
            _ => {}
        }
    }
}

// --- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_page() {
        let doc = parse("<html><head><title>Hello</title></head><body><p>World</p></body></html>");
        assert!(doc.nodes.len() >= 8);
        let title = doc
            .nodes
            .iter()
            .find(|n| matches!(&n.data, NodeData::Element { tag, .. } if tag == "title"));
        assert!(title.is_some());
        let hello = doc
            .nodes
            .iter()
            .find(|n| matches!(&n.data, NodeData::Text(t) if t == "Hello"));
        assert!(hello.is_some());
    }

    #[test]
    fn parse_form_with_inputs() {
        let doc = parse(
            r#"
            <form action="/login" method="post">
                <input type="email" name="user" />
                <input type="password" name="pass">
                <button type="submit">Sign In</button>
            </form>
        "#,
        );
        let form = doc
            .nodes
            .iter()
            .find(|n| doc.tag_name(n.id) == "form")
            .unwrap();
        assert_eq!(doc.attr(form.id, "action"), Some("/login"));
        assert_eq!(doc.attr(form.id, "method"), Some("post"));
        let inputs: Vec<_> = doc
            .nodes
            .iter()
            .filter(|n| doc.tag_name(n.id) == "input")
            .collect();
        assert_eq!(inputs.len(), 2);
    }

    #[test]
    fn entity_decoding() {
        let doc = parse(r#"<p>a &amp; b &lt; c &gt; d &quot;e&quot;</p>"#);
        let text = doc
            .nodes
            .iter()
            .find(|n| matches!(&n.data, NodeData::Text(_)))
            .unwrap();
        if let NodeData::Text(t) = &text.data {
            assert_eq!(t, "a & b < c > d \"e\"");
        }
    }

    #[test]
    fn comments_are_parsed() {
        let doc = parse("<div><!-- a comment --><p>text</p></div>");
        let comment = doc
            .nodes
            .iter()
            .find(|n| matches!(&n.data, NodeData::Comment(_)));
        assert!(comment.is_some());
        if let NodeData::Comment(c) = &comment.unwrap().data {
            assert_eq!(c.trim(), "a comment");
        }
    }

    #[test]
    fn self_closing_tags() {
        let doc = parse(r#"<div><br/><hr><img src="x.png" /></div>"#);
        assert!(doc.nodes.iter().any(|n| doc.tag_name(n.id) == "br"));
        assert!(doc.nodes.iter().any(|n| doc.tag_name(n.id) == "hr"));
        assert!(doc.nodes.iter().any(|n| doc.tag_name(n.id) == "img"));
    }

    #[test]
    fn unclosed_tags_handled() {
        let doc = parse("<div><p>Hello<p>World</div>");
        let ps: Vec<_> = doc
            .nodes
            .iter()
            .filter(|n| doc.tag_name(n.id) == "p")
            .collect();
        assert_eq!(ps.len(), 2);
    }

    #[test]
    fn script_content_captured() {
        let doc = parse("<html><head><script>var x = 1;</script></head><body></body></html>");
        let script = doc
            .nodes
            .iter()
            .find(|n| doc.tag_name(n.id) == "script")
            .unwrap();
        assert!(!script.children.is_empty());
    }

    #[test]
    fn inner_text_works() {
        let doc = parse("<div>Hello <span>beautiful</span> world</div>");
        let div = doc
            .nodes
            .iter()
            .find(|n| doc.tag_name(n.id) == "div")
            .unwrap();
        let text = doc.inner_text(div.id);
        assert!(text.contains("Hello"));
        assert!(text.contains("beautiful"));
        assert!(text.contains("world"));
    }

    #[test]
    fn numeric_entity_ref() {
        let doc = parse("<p>&#169; &#x41;</p>");
        let text = doc
            .nodes
            .iter()
            .find(|n| matches!(&n.data, NodeData::Text(_)))
            .unwrap();
        if let NodeData::Text(t) = &text.data {
            assert!(t.contains('\u{00A9}'));
            assert!(t.contains('A'));
        }
    }
}
