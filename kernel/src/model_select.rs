//! Multi-model selection for ClaudioOS.
//!
//! Supports switching between Anthropic models (Opus, Sonnet, Haiku) both
//! globally (default for new agents) and per-agent. Provides shell commands
//! and agent slash-commands for model switching.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use spin::Mutex;

// ---------------------------------------------------------------------------
// Available models
// ---------------------------------------------------------------------------

/// Information about an available model.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    /// Short name (used in commands): "opus", "sonnet", "haiku"
    pub short_name: &'static str,
    /// Full API model identifier
    pub api_id: &'static str,
    /// Human-readable display name
    pub display_name: &'static str,
    /// Context window size in tokens
    pub context_window: u32,
    /// Max output tokens
    pub max_output_tokens: u32,
    /// Speed characteristic description
    pub speed: &'static str,
    /// Quality/capability description
    pub quality: &'static str,
}

/// All available models.
pub static MODELS: &[ModelInfo] = &[
    ModelInfo {
        short_name: "opus",
        api_id: "claude-opus-4-6",
        display_name: "Claude Opus 4.6",
        context_window: 200_000,
        max_output_tokens: 32_000,
        speed: "slowest",
        quality: "highest quality, best for complex reasoning",
    },
    ModelInfo {
        short_name: "sonnet",
        api_id: "claude-sonnet-4-6",
        display_name: "Claude Sonnet 4.6",
        context_window: 200_000,
        max_output_tokens: 16_000,
        speed: "balanced",
        quality: "best balance of speed and quality",
    },
    ModelInfo {
        short_name: "haiku",
        api_id: "claude-haiku-4-5",
        display_name: "Claude Haiku 4.5",
        context_window: 200_000,
        max_output_tokens: 8_192,
        speed: "fastest",
        quality: "fastest, best for simple tasks",
    },
];

/// Default model short name.
const DEFAULT_MODEL_SHORT: &str = "sonnet";

// ---------------------------------------------------------------------------
// Global default model
// ---------------------------------------------------------------------------

/// Global default model API ID — used for new agent sessions and the claude.ai
/// completion endpoint. Protected by a spinlock since it can be changed at runtime.
static GLOBAL_MODEL: Mutex<&'static str> = Mutex::new("claude-sonnet-4-6");

/// Set the global default model by short name.
/// Returns `Ok(api_id)` or `Err(message)` if the name is unrecognized.
pub fn set_global_model(short_name: &str) -> Result<&'static str, String> {
    let model = lookup_model(short_name)?;
    let mut global = GLOBAL_MODEL.lock();
    *global = model.api_id;
    log::info!("[model] global default changed to {} ({})", model.display_name, model.api_id);
    Ok(model.api_id)
}

/// Get the current global default model API ID.
pub fn global_model_id() -> &'static str {
    *GLOBAL_MODEL.lock()
}

/// Get the current global default model info.
pub fn global_model_info() -> &'static ModelInfo {
    let id = global_model_id();
    MODELS.iter().find(|m| m.api_id == id).unwrap_or(&MODELS[1]) // fallback to sonnet
}

// ---------------------------------------------------------------------------
// Model lookup
// ---------------------------------------------------------------------------

/// Look up a model by short name, API ID, or partial match.
pub fn lookup_model(name: &str) -> Result<&'static ModelInfo, String> {
    let lower = name.to_lowercase();
    let lower = lower.trim();

    // Exact short name match
    if let Some(m) = MODELS.iter().find(|m| m.short_name == lower) {
        return Ok(m);
    }

    // Exact API ID match
    if let Some(m) = MODELS.iter().find(|m| m.api_id == lower) {
        return Ok(m);
    }

    // Partial match on short name or display name
    if let Some(m) = MODELS.iter().find(|m| {
        m.short_name.contains(&*lower)
            || m.display_name.to_lowercase().contains(&*lower)
    }) {
        return Ok(m);
    }

    Err(format!(
        "Unknown model '{}'. Available: {}",
        name,
        MODELS
            .iter()
            .map(|m| m.short_name)
            .collect::<alloc::vec::Vec<_>>()
            .join(", ")
    ))
}

// ---------------------------------------------------------------------------
// Per-agent model management
// ---------------------------------------------------------------------------

/// Set the model for a specific agent session.
/// Returns the new model API ID on success.
pub fn set_agent_model(
    session: &mut claudio_agent::AgentSession,
    short_name: &str,
) -> Result<&'static str, String> {
    let model = lookup_model(short_name)?;
    session.model = String::from(model.api_id);
    log::info!(
        "[model] agent {} model changed to {} ({})",
        session.id, model.display_name, model.api_id
    );
    Ok(model.api_id)
}

// ---------------------------------------------------------------------------
// Shell command handler
// ---------------------------------------------------------------------------

/// Handle the `model` shell command.
///
/// Usage:
/// - `model` — show current global model and all available models
/// - `model opus` / `model sonnet` / `model haiku` — switch global default
/// - `model info` — detailed info about all models
pub fn handle_command(args: &str) -> String {
    let args = args.trim();

    if args.is_empty() || args == "list" {
        // Show current model and list all available
        let current = global_model_info();
        let mut out = format!(
            "Current model: {} ({})\n\nAvailable models:\n",
            current.display_name, current.api_id
        );
        for m in MODELS {
            let marker = if m.api_id == current.api_id { " <-- active" } else { "" };
            out.push_str(&format!(
                "  {:8} {:30} {}{}\n",
                m.short_name, m.api_id, m.speed, marker
            ));
        }
        out.push_str("\nUsage: model <name> to switch (e.g., 'model opus')\n");
        out
    } else if args == "info" {
        // Detailed info about all models
        let mut out = String::from("Model details:\n\n");
        for m in MODELS {
            out.push_str(&format!(
                "{} ({})\n  API ID:         {}\n  Context window: {} tokens\n  Max output:     {} tokens\n  Speed:          {}\n  Quality:        {}\n\n",
                m.display_name, m.short_name, m.api_id,
                m.context_window, m.max_output_tokens,
                m.speed, m.quality
            ));
        }
        out
    } else {
        // Try to switch model
        match set_global_model(args) {
            Ok(api_id) => {
                let info = lookup_model(args).unwrap();
                format!(
                    "Switched global model to {} ({})\nSpeed: {} | Quality: {}\n",
                    info.display_name, api_id, info.speed, info.quality
                )
            }
            Err(e) => format!("{}\n", e),
        }
    }
}

/// Handle the `/model` agent slash-command (per-agent model switch).
///
/// Returns a display string describing the change or an error.
pub fn handle_agent_command(
    session: &mut claudio_agent::AgentSession,
    args: &str,
) -> String {
    let args = args.trim();

    if args.is_empty() {
        // Show current model for this agent
        let current_id = &session.model;
        let info = MODELS.iter().find(|m| m.api_id == current_id.as_str());
        match info {
            Some(m) => format!(
                "Agent {} model: {} ({})\n",
                session.id, m.display_name, m.api_id
            ),
            None => format!(
                "Agent {} model: {} (custom)\n",
                session.id, current_id
            ),
        }
    } else {
        match set_agent_model(session, args) {
            Ok(api_id) => {
                let info = lookup_model(args).unwrap();
                format!(
                    "Agent {} switched to {} ({})\n",
                    session.id, info.display_name, api_id
                )
            }
            Err(e) => format!("{}\n", e),
        }
    }
}

/// Get the model API ID to use for the claude.ai completion endpoint.
///
/// This returns the global default model, which is used in `send_via_claude_ai`
/// to replace the hardcoded model name.
pub fn claude_ai_model_id() -> &'static str {
    global_model_id()
}
