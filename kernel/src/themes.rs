//! Color theme system for ClaudioOS.
//!
//! Provides a set of built-in color themes and a global active theme behind
//! a `spin::Mutex`. Shell command `theme <name>` switches themes at runtime.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

/// An RGB color triple (matches `claudio_terminal::render::Color` layout but
/// is kernel-local so we don't modify crate code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl ThemeColor {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// A complete color theme for the ClaudioOS terminal environment.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Theme name (static str for built-ins).
    pub name: &'static str,
    /// Background color.
    pub bg_color: ThemeColor,
    /// Default foreground / text color.
    pub fg_color: ThemeColor,
    /// Shell/agent prompt name color (e.g. "shell", "agent-0").
    pub prompt_color: ThemeColor,
    /// Agent response text color.
    pub agent_color: ThemeColor,
    /// Shell command echo color.
    pub shell_color: ThemeColor,
    /// Error text color.
    pub error_color: ThemeColor,
    /// Info / status text color.
    pub info_color: ThemeColor,
    /// Border / separator color.
    pub border_color: ThemeColor,
    /// Highlight / accent color (thinking indicator, tool names).
    pub highlight_color: ThemeColor,
    /// Cursor / input caret color.
    pub cursor_color: ThemeColor,
}

// ---------------------------------------------------------------------------
// Built-in themes
// ---------------------------------------------------------------------------

/// Default dark theme — black background, white foreground, green prompt.
pub const THEME_DEFAULT: Theme = Theme {
    name: "default",
    bg_color: ThemeColor::new(16, 16, 16),
    fg_color: ThemeColor::new(204, 204, 204),
    prompt_color: ThemeColor::new(204, 204, 0),   // yellow name
    agent_color: ThemeColor::new(0, 204, 204),     // cyan agent text
    shell_color: ThemeColor::new(0, 204, 0),       // green shell echo
    error_color: ThemeColor::new(204, 0, 0),       // red errors
    info_color: ThemeColor::new(128, 128, 128),    // gray info
    border_color: ThemeColor::new(128, 128, 128),  // gray borders
    highlight_color: ThemeColor::new(204, 204, 0), // yellow highlights
    cursor_color: ThemeColor::new(255, 255, 255),  // white cursor
};

/// Solarized Dark — Ethan Schoonover's dark palette.
pub const THEME_SOLARIZED_DARK: Theme = Theme {
    name: "solarized-dark",
    bg_color: ThemeColor::new(0, 43, 54),
    fg_color: ThemeColor::new(131, 148, 150),
    prompt_color: ThemeColor::new(181, 137, 0),    // yellow
    agent_color: ThemeColor::new(38, 139, 210),    // blue
    shell_color: ThemeColor::new(133, 153, 0),     // green
    error_color: ThemeColor::new(220, 50, 47),     // red
    info_color: ThemeColor::new(88, 110, 117),     // base01
    border_color: ThemeColor::new(88, 110, 117),   // base01
    highlight_color: ThemeColor::new(203, 75, 22), // orange
    cursor_color: ThemeColor::new(238, 232, 213),  // base2
};

/// Solarized Light — the light variant.
pub const THEME_SOLARIZED_LIGHT: Theme = Theme {
    name: "solarized-light",
    bg_color: ThemeColor::new(253, 246, 227),
    fg_color: ThemeColor::new(101, 123, 131),
    prompt_color: ThemeColor::new(181, 137, 0),    // yellow
    agent_color: ThemeColor::new(38, 139, 210),    // blue
    shell_color: ThemeColor::new(133, 153, 0),     // green
    error_color: ThemeColor::new(220, 50, 47),     // red
    info_color: ThemeColor::new(147, 161, 161),    // base1
    border_color: ThemeColor::new(147, 161, 161),  // base1
    highlight_color: ThemeColor::new(203, 75, 22), // orange
    cursor_color: ThemeColor::new(7, 54, 66),      // base02
};

/// Monokai — the iconic dark theme.
pub const THEME_MONOKAI: Theme = Theme {
    name: "monokai",
    bg_color: ThemeColor::new(39, 40, 34),
    fg_color: ThemeColor::new(248, 248, 242),
    prompt_color: ThemeColor::new(230, 219, 116),  // yellow
    agent_color: ThemeColor::new(102, 217, 239),   // cyan
    shell_color: ThemeColor::new(166, 226, 46),    // green
    error_color: ThemeColor::new(249, 38, 114),    // pink/red
    info_color: ThemeColor::new(117, 113, 94),     // comment gray
    border_color: ThemeColor::new(117, 113, 94),   // comment gray
    highlight_color: ThemeColor::new(253, 151, 31),// orange
    cursor_color: ThemeColor::new(248, 248, 240),  // near-white
};

/// Dracula — the popular dark theme.
pub const THEME_DRACULA: Theme = Theme {
    name: "dracula",
    bg_color: ThemeColor::new(40, 42, 54),
    fg_color: ThemeColor::new(248, 248, 242),
    prompt_color: ThemeColor::new(241, 250, 140),  // yellow
    agent_color: ThemeColor::new(139, 233, 253),   // cyan
    shell_color: ThemeColor::new(80, 250, 123),    // green
    error_color: ThemeColor::new(255, 85, 85),     // red
    info_color: ThemeColor::new(98, 114, 164),     // comment
    border_color: ThemeColor::new(98, 114, 164),   // comment
    highlight_color: ThemeColor::new(255, 184, 108),// orange
    cursor_color: ThemeColor::new(248, 248, 242),  // fg
};

/// Nord — the arctic, north-bluish palette.
pub const THEME_NORD: Theme = Theme {
    name: "nord",
    bg_color: ThemeColor::new(46, 52, 64),
    fg_color: ThemeColor::new(216, 222, 233),
    prompt_color: ThemeColor::new(235, 203, 139),  // yellow (nord13)
    agent_color: ThemeColor::new(136, 192, 208),   // frost blue (nord8)
    shell_color: ThemeColor::new(163, 190, 140),   // green (nord14)
    error_color: ThemeColor::new(191, 97, 106),    // red (nord11)
    info_color: ThemeColor::new(76, 86, 106),      // nord3
    border_color: ThemeColor::new(76, 86, 106),    // nord3
    highlight_color: ThemeColor::new(208, 135, 112),// orange (nord12)
    cursor_color: ThemeColor::new(236, 239, 244),  // snow storm (nord6)
};

/// Gruvbox — retro groove color scheme.
pub const THEME_GRUVBOX: Theme = Theme {
    name: "gruvbox",
    bg_color: ThemeColor::new(40, 40, 40),
    fg_color: ThemeColor::new(235, 219, 178),
    prompt_color: ThemeColor::new(250, 189, 47),   // yellow
    agent_color: ThemeColor::new(131, 165, 152),   // aqua
    shell_color: ThemeColor::new(184, 187, 38),    // green
    error_color: ThemeColor::new(251, 73, 52),     // red
    info_color: ThemeColor::new(146, 131, 116),    // gray
    border_color: ThemeColor::new(146, 131, 116),  // gray
    highlight_color: ThemeColor::new(254, 128, 25),// orange
    cursor_color: ThemeColor::new(253, 244, 193),  // fg0
};

/// ClaudioOS — custom: deep navy background, cyan accents, orange highlights.
pub const THEME_CLAUDIOOS: Theme = Theme {
    name: "claudioos",
    bg_color: ThemeColor::new(10, 15, 36),
    fg_color: ThemeColor::new(200, 210, 230),
    prompt_color: ThemeColor::new(0, 200, 255),    // bright cyan
    agent_color: ThemeColor::new(0, 220, 255),     // cyan
    shell_color: ThemeColor::new(80, 255, 180),    // mint green
    error_color: ThemeColor::new(255, 80, 80),     // bright red
    info_color: ThemeColor::new(90, 100, 140),     // muted blue-gray
    border_color: ThemeColor::new(50, 70, 120),    // navy border
    highlight_color: ThemeColor::new(255, 160, 40),// orange
    cursor_color: ThemeColor::new(0, 220, 255),    // cyan cursor
};

/// TempleOS — white background, black foreground. Honoring Terry A. Davis.
pub const THEME_TEMPLEOS: Theme = Theme {
    name: "templeos",
    bg_color: ThemeColor::new(255, 255, 255),
    fg_color: ThemeColor::new(0, 0, 0),
    prompt_color: ThemeColor::new(0, 0, 170),      // DOS blue
    agent_color: ThemeColor::new(0, 0, 0),         // black
    shell_color: ThemeColor::new(0, 128, 0),       // dark green
    error_color: ThemeColor::new(170, 0, 0),       // dark red
    info_color: ThemeColor::new(100, 100, 100),    // gray
    border_color: ThemeColor::new(0, 0, 0),        // black
    highlight_color: ThemeColor::new(170, 0, 170), // DOS magenta
    cursor_color: ThemeColor::new(0, 0, 0),        // black
};

// ---------------------------------------------------------------------------
// All built-in themes
// ---------------------------------------------------------------------------

const ALL_THEMES: &[Theme] = &[
    THEME_DEFAULT,
    THEME_SOLARIZED_DARK,
    THEME_SOLARIZED_LIGHT,
    THEME_MONOKAI,
    THEME_DRACULA,
    THEME_NORD,
    THEME_GRUVBOX,
    THEME_CLAUDIOOS,
    THEME_TEMPLEOS,
];

// ---------------------------------------------------------------------------
// Global active theme
// ---------------------------------------------------------------------------

static ACTIVE_THEME: Mutex<Theme> = Mutex::new(THEME_DEFAULT);

/// Set the active theme by name. Returns `Ok(())` if found, `Err` with message
/// if the theme name is not recognized.
pub fn set_theme(name: &str) -> Result<(), String> {
    let lower = name.to_ascii_lowercase();
    for theme in ALL_THEMES {
        // Match case-insensitively and allow partial matches without hyphens.
        let theme_lower = theme.name;
        if theme_lower == lower
            || theme_lower.replace('-', "") == lower.replace('-', "")
        {
            *ACTIVE_THEME.lock() = *theme;
            log::info!("[themes] switched to theme: {}", theme.name);
            return Ok(());
        }
    }
    Err(alloc::format!(
        "Unknown theme '{}'. Available: {}",
        name,
        list_theme_names().join(", ")
    ))
}

/// Get a copy of the currently active theme.
pub fn get_theme() -> Theme {
    *ACTIVE_THEME.lock()
}

/// List all available theme names.
pub fn list_themes() -> Vec<&'static str> {
    ALL_THEMES.iter().map(|t| t.name).collect()
}

/// List theme names as owned Strings (convenience for shell output).
pub fn list_theme_names() -> Vec<String> {
    ALL_THEMES.iter().map(|t| String::from(t.name)).collect()
}

// ---------------------------------------------------------------------------
// ANSI escape helpers — generate escape codes from theme colors
// ---------------------------------------------------------------------------

impl ThemeColor {
    /// Generate ANSI 24-bit foreground color escape sequence.
    pub fn ansi_fg(self) -> String {
        alloc::format!("\x1b[38;2;{};{};{}m", self.r, self.g, self.b)
    }

    /// Generate ANSI 24-bit background color escape sequence.
    pub fn ansi_bg(self) -> String {
        alloc::format!("\x1b[48;2;{};{};{}m", self.r, self.g, self.b)
    }
}

impl Theme {
    /// Generate the prompt line for a pane, using this theme's colors.
    ///
    /// Format: `<name_color>name<fg> state <shell_color>prompt_char<reset> input`
    pub fn format_prompt(&self, name: &str, state: &str, prompt_char: &str, input: &str, row: usize) -> String {
        alloc::format!(
            "\x1b[s\x1b[{};1H\x1b[2K{}{}\x1b[0m{} {} {}{} \x1b[0m{}{}\x1b[u",
            row,
            self.prompt_color.ansi_fg(),
            name,
            self.fg_color.ansi_fg(),
            state,
            self.shell_color.ansi_fg(),
            prompt_char,
            self.fg_color.ansi_fg(),
            input,
        )
    }

    /// Format the welcome banner using theme colors.
    pub fn format_welcome_banner(&self) -> String {
        let title_fg = self.agent_color.ansi_fg();
        let accent_fg = self.highlight_color.ansi_fg();
        let border_fg = self.info_color.ansi_fg();
        let ok_fg = self.shell_color.ansi_fg();
        let phase_fg = self.shell_color.ansi_fg();
        let dim_fg = self.info_color.ansi_fg();
        let reset = "\x1b[0m";

        let mut s = String::new();
        s.push_str(&alloc::format!("{}ClaudioOS v0.1.0{} — {}Bare Metal AI Agent Terminal{}\r\n",
            title_fg, reset, accent_fg, reset));
        s.push_str(&alloc::format!("{}────────────────────────────────────────────────────{}\r\n",
            border_fg, reset));
        s.push_str("\r\n");
        s.push_str(&alloc::format!("  {}Phase 1{}: Boot to terminal ............. {}OK{}\r\n",
            phase_fg, reset, ok_fg, reset));
        s.push_str(&alloc::format!("  {}Phase 2{}: Networking ................... {}OK{}\r\n",
            phase_fg, reset, ok_fg, reset));
        s.push_str(&alloc::format!("  {}Phase 3{}: TLS + API .................... {}OK{}\r\n",
            phase_fg, reset, ok_fg, reset));
        s.push_str(&alloc::format!("  {}Phase 4{}: Multi-agent dashboard ........ {}OK{}\r\n",
            phase_fg, reset, ok_fg, reset));
        s.push_str("\r\n");
        s.push_str(&alloc::format!("{}Ctrl+B then \" = split | n/p = focus | c = new agent | s = new shell | x = close | , = rename{}\r\n",
            dim_fg, reset));
        s.push_str(&alloc::format!("{}IPC: /msg <agent> <text> | /broadcast <text> | /inbox | /agents | /channel create|read|write{}\r\n",
            dim_fg, reset));
        s.push_str(&alloc::format!("{}Type commands or natural language. Type 'help' for builtins.{}\r\n",
            dim_fg, reset));
        s.push_str(&alloc::format!("{}Active theme: {}. Type 'theme <name>' to switch.{}\r\n",
            dim_fg, self.name, reset));
        s.push_str("\r\n");
        s
    }

    /// ANSI escape for "thinking" indicator.
    pub fn format_thinking(&self) -> String {
        alloc::format!("{}{}{}", self.highlight_color.ansi_fg(), "[thinking...]", "\x1b[0m")
    }

    /// ANSI escape for user input echo (shell).
    pub fn format_shell_echo(&self, input: &str) -> String {
        alloc::format!("\r\n{}$ {}\x1b[0m\r\n", self.shell_color.ansi_fg(), input)
    }

    /// ANSI escape for user input echo (agent).
    pub fn format_agent_echo(&self, input: &str) -> String {
        alloc::format!("\r\n{}> {}\x1b[0m\r\n", self.shell_color.ansi_fg(), input)
    }

    /// ANSI escape for agent response text.
    pub fn format_agent_response(&self, text: &str) -> String {
        alloc::format!("\r\n{}{}\x1b[0m\r\n", self.agent_color.ansi_fg(), text)
    }

    /// ANSI escape for error text.
    pub fn format_error(&self, text: &str) -> String {
        alloc::format!("\r\n{}[error: {}]\x1b[0m\r\n", self.error_color.ansi_fg(), text)
    }

    /// ANSI escape for tool call display.
    pub fn format_tool_call(&self, name: &str, summary: &str, preview: &str, is_error: bool) -> String {
        if is_error {
            alloc::format!(
                "\r\n{}[tool] {}({}){} -> error: {}\x1b[0m\r\n",
                self.highlight_color.ansi_fg(), name, summary,
                self.error_color.ansi_fg(), preview
            )
        } else {
            alloc::format!(
                "\r\n{}[tool] {}({}){} -> {}\x1b[0m\r\n",
                self.highlight_color.ansi_fg(), name, summary,
                self.info_color.ansi_fg(), preview
            )
        }
    }
}
