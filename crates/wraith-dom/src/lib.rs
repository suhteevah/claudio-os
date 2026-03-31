//! # wraith-dom
//!
//! A minimal `#![no_std]` HTML parser for bare-metal use.
//!
//! Designed for OAuth form detection on ClaudioOS: parse login pages,
//! find forms, extract input fields, and detect redirects. Does NOT
//! attempt full HTML5 spec compliance -- just enough to handle real-world
//! login/OAuth pages from Google, GitHub, Anthropic, etc.
//!
//! ## Features
//!
//! - Simple HTML tokenizer and tree builder (`parser`)
//! - Minimal CSS selector matching (`selector`)
//! - Form detection and extraction (`forms`)
//! - Visible text extraction (`text`)
//!
//! ## Memory
//!
//! All allocations go through `alloc`. No external dependencies beyond `log`.

#![no_std]

extern crate alloc;

pub mod forms;
pub mod parser;
pub mod selector;
pub mod text;

pub use forms::{find_forms, find_login_form, Form, FormInput};
pub use parser::{parse, Document, Node, NodeData};
pub use selector::{select, Selector};
pub use text::{extract_links, extract_text, extract_title};
