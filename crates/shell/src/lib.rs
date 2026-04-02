//! claudio-shell — Hybrid AI-native shell for ClaudioOS
//!
//! Combines traditional Unix-like commands with natural language processing.
//! Type `ls /mnt/nvme0` OR `"show me what's on the NVMe drive"` — both work.
//!
//! This crate is `#![no_std]` and runs directly on the ClaudioOS bare-metal kernel.

#![no_std]

extern crate alloc;

pub mod ai;
pub mod builtin;
pub mod env;
pub mod parser;
pub mod pipe;
pub mod prompt;
pub mod script;
pub mod shell;

pub use shell::Shell;
pub use shell::LineReader;
pub use env::Environment;
pub use parser::{Command, Pipeline};
pub use builtin::{execute_builtin, Vfs, SystemInfo};
pub use ai::AiShellCallback;
pub use pipe::PipelineExecutor;
pub use prompt::Prompt;
pub use script::ScriptRunner;
