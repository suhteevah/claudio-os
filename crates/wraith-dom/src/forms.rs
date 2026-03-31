//! Form detection and extraction.
//!
//! Finds `<form>` elements in a parsed document, extracts their inputs,
//! and provides heuristics to locate login/OAuth forms.

use alloc::string::String;
use alloc::vec::Vec;

use crate::parser::{Document, NodeData};

// --- Types ---------------------------------------------------------------

/// A parsed HTML form.
pub struct Form {
    /// The `action` attribute (URL the form submits to).
    pub action: String,
    /// The HTTP method: `"GET"` or `"POST"`.
    pub method: String,
    /// All input fields within the form.
    pub inputs: Vec<FormInput>,
}

/// A single form input field.
pub struct FormInput {
    /// The `name` attribute.
    pub name: String,
    /// The `type` attribute (e.g., `"text"`, `"email"`, `"password"`, `"hidden"`, `"submit"`).
    pub input_type: String,
    /// Pre-filled value, if any.
    pub value: String,
}

// --- Extraction ----------------------------------------------------------

/// Find all `<form>` elements and extract their fields.
pub fn find_forms(doc: &Document) -> Vec<Form> {
    let mut forms = Vec::new();
    for node in &doc.nodes {
        if let NodeData::Element { tag, attributes } = &node.data {
            if tag == "form" {
                let action = attr_val(attributes, "action");
                let method_raw = attr_val(attributes, "method");
                let method = if method_raw.eq_ignore_ascii_case("post") {
                    String::from("POST")
                } else {
                    String::from("GET")
                };
                let mut inputs = Vec::new();
                collect_inputs(doc, node.id, &mut inputs);
                forms.push(Form {
                    action,
                    method,
                    inputs,
                });
            }
        }
    }
    forms
}

/// Find a login/OAuth form using heuristics:
/// 1. Has a password input field.
/// 2. Action URL contains login-related keywords.
/// 3. Form id/class contains login-related keywords.
/// 4. Fallback: the only form on the page.
pub fn find_login_form(doc: &Document) -> Option<Form> {
    let forms = find_forms(doc);

    // First: form with a password field
    for form in &forms {
        if form.inputs.iter().any(|i| i.input_type == "password") {
            return Some(clone_form(form));
        }
    }

    let keywords = [
        "login", "auth", "signin", "sign-in", "sign_in", "oauth", "session",
    ];

    // Second: action URL contains keywords
    for form in &forms {
        let action_lower = form.action.to_ascii_lowercase();
        if keywords.iter().any(|kw| action_lower.contains(kw)) {
            return Some(clone_form(form));
        }
    }

    // Third: form id/class contains keywords
    for node in &doc.nodes {
        if let NodeData::Element { tag, attributes } = &node.data {
            if tag == "form" {
                let id = attr_val(attributes, "id").to_ascii_lowercase();
                let class = attr_val(attributes, "class").to_ascii_lowercase();
                let combined = alloc::format!("{} {}", id, class);
                if keywords.iter().any(|kw| combined.contains(kw)) {
                    let action = attr_val(attributes, "action");
                    let method_raw = attr_val(attributes, "method");
                    let method = if method_raw.eq_ignore_ascii_case("post") {
                        String::from("POST")
                    } else {
                        String::from("GET")
                    };
                    let mut inputs = Vec::new();
                    collect_inputs(doc, node.id, &mut inputs);
                    return Some(Form {
                        action,
                        method,
                        inputs,
                    });
                }
            }
        }
    }

    // Fallback: single form
    if forms.len() == 1 {
        return Some(clone_form(&forms[0]));
    }

    None
}

// --- Helpers -------------------------------------------------------------

fn attr_val(attrs: &[(String, String)], name: &str) -> String {
    attrs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

fn collect_inputs(doc: &Document, node_id: usize, inputs: &mut Vec<FormInput>) {
    for &child_id in &doc.nodes[node_id].children {
        if let NodeData::Element { tag, attributes } = &doc.nodes[child_id].data {
            match tag.as_str() {
                "input" => {
                    let name = attr_val(attributes, "name");
                    let input_type = attr_val(attributes, "type");
                    let input_type = if input_type.is_empty() {
                        String::from("text")
                    } else {
                        input_type.to_ascii_lowercase()
                    };
                    let value = attr_val(attributes, "value");
                    inputs.push(FormInput {
                        name,
                        input_type,
                        value,
                    });
                }
                "select" => {
                    let name = attr_val(attributes, "name");
                    let value = find_selected_option(doc, child_id);
                    inputs.push(FormInput {
                        name,
                        input_type: String::from("select"),
                        value,
                    });
                }
                "textarea" => {
                    let name = attr_val(attributes, "name");
                    let value = doc.inner_text(child_id);
                    inputs.push(FormInput {
                        name,
                        input_type: String::from("textarea"),
                        value,
                    });
                }
                "button" => {
                    let name = attr_val(attributes, "name");
                    let btn_type = attr_val(attributes, "type");
                    let btn_type = if btn_type.is_empty() {
                        String::from("submit")
                    } else {
                        btn_type.to_ascii_lowercase()
                    };
                    let value = attr_val(attributes, "value");
                    let value = if value.is_empty() {
                        doc.inner_text(child_id)
                    } else {
                        value
                    };
                    inputs.push(FormInput {
                        name,
                        input_type: btn_type,
                        value,
                    });
                }
                _ => {
                    collect_inputs(doc, child_id, inputs);
                }
            }
        }
    }
}

fn find_selected_option(doc: &Document, select_id: usize) -> String {
    let mut first_value = None;
    for &child_id in &doc.nodes[select_id].children {
        if let NodeData::Element { tag, attributes } = &doc.nodes[child_id].data {
            if tag == "option" {
                let val = attr_val(attributes, "value");
                if first_value.is_none() {
                    first_value = Some(val.clone());
                }
                if attributes.iter().any(|(k, _)| k == "selected") {
                    return val;
                }
            }
        }
    }
    first_value.unwrap_or_default()
}

fn clone_form(form: &Form) -> Form {
    Form {
        action: form.action.clone(),
        method: form.method.clone(),
        inputs: form
            .inputs
            .iter()
            .map(|i| FormInput {
                name: i.name.clone(),
                input_type: i.input_type.clone(),
                value: i.value.clone(),
            })
            .collect(),
    }
}

// --- Tests ---------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn find_simple_form() {
        let doc = parse(
            r#"
            <html><body>
            <form action="/login" method="post">
                <input type="email" name="user">
                <input type="password" name="pass">
                <input type="hidden" name="csrf" value="abc123">
                <button type="submit">Sign In</button>
            </form>
            </body></html>
        "#,
        );
        let forms = find_forms(&doc);
        assert_eq!(forms.len(), 1);
        let f = &forms[0];
        assert_eq!(f.action, "/login");
        assert_eq!(f.method, "POST");
        assert_eq!(f.inputs.len(), 4);
        let email = f.inputs.iter().find(|i| i.input_type == "email").unwrap();
        assert_eq!(email.name, "user");
        let hidden = f.inputs.iter().find(|i| i.input_type == "hidden").unwrap();
        assert_eq!(hidden.value, "abc123");
    }

    #[test]
    fn detect_login_form_by_password() {
        let doc = parse(
            r#"
            <form action="/search" method="get">
                <input type="text" name="q">
            </form>
            <form action="/auth" method="post">
                <input type="email" name="email">
                <input type="password" name="password">
            </form>
        "#,
        );
        let login = find_login_form(&doc).unwrap();
        assert_eq!(login.method, "POST");
        assert!(login.inputs.iter().any(|i| i.input_type == "password"));
    }

    #[test]
    fn detect_login_form_by_action_url() {
        let doc = parse(
            r#"
            <form action="https://accounts.google.com/signin/v2" method="post">
                <input type="email" name="identifier">
                <button type="submit">Next</button>
            </form>
        "#,
        );
        let login = find_login_form(&doc).unwrap();
        assert!(login.action.contains("signin"));
    }

    #[test]
    fn detect_login_form_by_id() {
        let doc = parse(
            r#"
            <form id="login-form" action="/api/v1/session" method="post">
                <input type="text" name="username">
                <button type="submit">Log In</button>
            </form>
        "#,
        );
        let login = find_login_form(&doc).unwrap();
        assert_eq!(login.inputs.len(), 2);
    }

    #[test]
    fn form_with_select_and_textarea() {
        let doc = parse(
            r#"
            <form action="/profile" method="post">
                <select name="role">
                    <option value="user">User</option>
                    <option value="admin" selected>Admin</option>
                </select>
                <textarea name="bio">Hello world</textarea>
            </form>
        "#,
        );
        let forms = find_forms(&doc);
        let f = &forms[0];
        let sel = f.inputs.iter().find(|i| i.input_type == "select").unwrap();
        assert_eq!(sel.name, "role");
        assert_eq!(sel.value, "admin");
        let ta = f
            .inputs
            .iter()
            .find(|i| i.input_type == "textarea")
            .unwrap();
        assert_eq!(ta.name, "bio");
        assert!(ta.value.contains("Hello world"));
    }

    #[test]
    fn nested_inputs_in_divs() {
        let doc = parse(
            r#"
            <form action="/login" method="post">
                <div class="field">
                    <input type="email" name="email">
                </div>
                <div class="field">
                    <input type="password" name="password">
                </div>
            </form>
        "#,
        );
        let forms = find_forms(&doc);
        assert_eq!(forms[0].inputs.len(), 2);
    }

    #[test]
    fn single_form_fallback() {
        let doc = parse(
            r#"
            <form action="/do-something" method="post">
                <input type="text" name="data">
            </form>
        "#,
        );
        assert!(find_login_form(&doc).is_some());
    }
}
