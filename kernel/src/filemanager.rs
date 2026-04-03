//! Visual file manager pane for ClaudioOS.
//!
//! Provides a terminal-based file browser with directory listing, navigation,
//! and file operations. Renders into a dashboard pane using ANSI escape codes.
//!
//! Uses the VFS trait from `claudio_shell` — if no filesystem is mounted, shows
//! a "mount a filesystem first" message.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use pc_keyboard::KeyCode;

// ---------------------------------------------------------------------------
// Directory entry representation
// ---------------------------------------------------------------------------

/// Type of a filesystem entry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
    Executable,
}

/// A single directory entry for display.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    /// Modified timestamp as a formatted string (or empty if unavailable).
    pub modified: String,
}

// ---------------------------------------------------------------------------
// File manager modes
// ---------------------------------------------------------------------------

/// Modal state for multi-key operations (delete confirmation, rename, etc.).
#[derive(Debug, Clone)]
enum Mode {
    /// Normal browsing mode.
    Normal,
    /// Waiting for 'y'/'n' to confirm deletion.
    ConfirmDelete(String),
    /// Typing a new name for rename. `String` is the original name.
    Rename(String, String),
    /// Typing a destination path for copy.
    Copy(String, String),
    /// Typing a destination path for move.
    Move(String, String),
    /// Typing a name for new file/directory, bool = true for directory.
    NewEntry(String, bool),
    /// Typing a search/filter pattern.
    Search(String),
}

// ---------------------------------------------------------------------------
// FileManagerState
// ---------------------------------------------------------------------------

/// State for a file manager pane. Tracks the current directory, entries,
/// selection, and scroll position.
pub struct FileManagerState {
    /// Current directory path.
    pub current_path: String,
    /// Entries in the current directory (including . and ..).
    pub entries: Vec<DirEntry>,
    /// Index of the currently selected entry.
    pub selected_index: usize,
    /// Scroll offset for long listings.
    pub scroll_offset: usize,
    /// Layout pane id this file manager is bound to.
    pub pane_id: usize,
    /// Current modal mode.
    mode: Mode,
    /// Whether a VFS is available (false = show stub message).
    vfs_available: bool,
    /// Active search filter (empty = no filter).
    filter: String,
    /// Status message shown at the bottom.
    pub status_message: String,
}

impl FileManagerState {
    /// Create a new file manager state starting at the given path.
    pub fn new(pane_id: usize) -> Self {
        let mut state = Self {
            current_path: String::from("/"),
            entries: Vec::new(),
            selected_index: 0,
            scroll_offset: 0,
            pane_id,
            mode: Mode::Normal,
            vfs_available: false,
            filter: String::new(),
            status_message: String::new(),
        };
        // Populate with stub entries (. and ..) since no VFS is mounted yet.
        state.populate_stub();
        state
    }

    /// Populate entries from the VFS. Returns true if the VFS had data.
    pub fn refresh<V: claudio_shell::Vfs>(&mut self, vfs: &V) {
        self.entries.clear();

        // Always add . and ..
        self.entries.push(DirEntry {
            name: String::from("."),
            kind: EntryKind::Directory,
            size: 0,
            modified: String::new(),
        });
        if self.current_path != "/" {
            self.entries.push(DirEntry {
                name: String::from(".."),
                kind: EntryKind::Directory,
                size: 0,
                modified: String::new(),
            });
        }

        match vfs.list_dir(&self.current_path) {
            Ok(names) => {
                self.vfs_available = true;
                for name in names {
                    let full_path = if self.current_path.ends_with('/') {
                        format!("{}{}", self.current_path, name)
                    } else {
                        format!("{}/{}", self.current_path, name)
                    };
                    let is_dir = vfs.is_dir(&full_path);
                    let size = if is_dir {
                        0
                    } else {
                        vfs.read_file(&full_path).map(|d| d.len() as u64).unwrap_or(0)
                    };
                    let kind = if is_dir {
                        EntryKind::Directory
                    } else {
                        EntryKind::File
                    };
                    self.entries.push(DirEntry {
                        name,
                        kind,
                        size,
                        modified: String::new(),
                    });
                }
                // Apply filter if active.
                if !self.filter.is_empty() {
                    let f = self.filter.clone();
                    self.entries.retain(|e| {
                        e.name == "." || e.name == ".." || e.name.contains(f.as_str())
                    });
                }
                self.status_message.clear();
            }
            Err(e) => {
                self.vfs_available = false;
                self.status_message = e;
            }
        }

        // Clamp selection.
        if self.selected_index >= self.entries.len() {
            self.selected_index = if self.entries.is_empty() {
                0
            } else {
                self.entries.len() - 1
            };
        }
    }

    /// Populate with stub entries when no VFS is mounted.
    fn populate_stub(&mut self) {
        self.entries.clear();
        self.entries.push(DirEntry {
            name: String::from("."),
            kind: EntryKind::Directory,
            size: 0,
            modified: String::new(),
        });
        self.entries.push(DirEntry {
            name: String::from(".."),
            kind: EntryKind::Directory,
            size: 0,
            modified: String::new(),
        });
        self.vfs_available = false;
        self.status_message = String::from("No filesystem mounted. Use `mount` to attach a device.");
    }

    /// Navigate into a directory or open a file.
    pub fn enter_selected<V: claudio_shell::Vfs>(&mut self, vfs: &V) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let entry = &self.entries[self.selected_index];
        match entry.kind {
            EntryKind::Directory => {
                let name = entry.name.clone();
                if name == "." {
                    // Stay.
                } else if name == ".." {
                    self.go_parent(vfs);
                } else {
                    if self.current_path.ends_with('/') {
                        self.current_path = format!("{}{}", self.current_path, name);
                    } else {
                        self.current_path = format!("{}/{}", self.current_path, name);
                    }
                    self.selected_index = 0;
                    self.scroll_offset = 0;
                    self.refresh(vfs);
                }
                None
            }
            _ => {
                // Return the full path for opening in editor.
                let name = entry.name.clone();
                let full_path = if self.current_path.ends_with('/') {
                    format!("{}{}", self.current_path, name)
                } else {
                    format!("{}/{}", self.current_path, name)
                };
                Some(full_path)
            }
        }
    }

    /// Navigate to the parent directory.
    pub fn go_parent<V: claudio_shell::Vfs>(&mut self, vfs: &V) {
        if self.current_path == "/" {
            return;
        }
        // Strip trailing slash if present.
        let path = if self.current_path.ends_with('/') && self.current_path.len() > 1 {
            &self.current_path[..self.current_path.len() - 1]
        } else {
            self.current_path.as_str()
        };
        if let Some(pos) = path.rfind('/') {
            if pos == 0 {
                self.current_path = String::from("/");
            } else {
                self.current_path = String::from(&path[..pos]);
            }
        }
        self.selected_index = 0;
        self.scroll_offset = 0;
        self.refresh(vfs);
    }

    /// Move selection up.
    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
            if self.selected_index < self.scroll_offset {
                self.scroll_offset = self.selected_index;
            }
        }
    }

    /// Move selection down.
    pub fn move_down(&mut self, visible_rows: usize) {
        if !self.entries.is_empty() && self.selected_index < self.entries.len() - 1 {
            self.selected_index += 1;
            // Scroll if selection goes below visible area.
            // Reserve 4 rows for header + path bar + column headers + footer.
            let max_visible = visible_rows.saturating_sub(4);
            if self.selected_index >= self.scroll_offset + max_visible {
                self.scroll_offset = self.selected_index - max_visible + 1;
            }
        }
    }

    /// Handle a Unicode character input. Returns an optional action string.
    pub fn handle_char<V: claudio_shell::Vfs>(
        &mut self,
        c: char,
        vfs: &mut V,
    ) -> Option<FileManagerAction> {
        match &self.mode {
            Mode::Normal => self.handle_normal_char(c, vfs),
            Mode::ConfirmDelete(_) => {
                let path = match &self.mode {
                    Mode::ConfirmDelete(p) => p.clone(),
                    _ => unreachable!(),
                };
                if c == 'y' || c == 'Y' {
                    match vfs.remove(&path) {
                        Ok(()) => {
                            self.status_message = format!("Deleted: {}", path);
                        }
                        Err(e) => {
                            self.status_message = format!("Delete failed: {}", e);
                        }
                    }
                    self.mode = Mode::Normal;
                    self.refresh(vfs);
                } else {
                    self.status_message = String::from("Delete cancelled.");
                    self.mode = Mode::Normal;
                }
                Some(FileManagerAction::Redraw)
            }
            Mode::Rename(_, input) => {
                let mut input = input.clone();
                let orig = match &self.mode {
                    Mode::Rename(o, _) => o.clone(),
                    _ => unreachable!(),
                };
                if c == '\n' || c == '\r' {
                    if !input.is_empty() {
                        let dst = if self.current_path.ends_with('/') {
                            format!("{}{}", self.current_path, input)
                        } else {
                            format!("{}/{}", self.current_path, input)
                        };
                        match vfs.move_file(&orig, &dst) {
                            Ok(()) => {
                                self.status_message = format!("Renamed to: {}", input);
                            }
                            Err(e) => {
                                self.status_message = format!("Rename failed: {}", e);
                            }
                        }
                        self.refresh(vfs);
                    }
                    self.mode = Mode::Normal;
                } else if c == '\x1b' {
                    self.mode = Mode::Normal;
                    self.status_message = String::from("Rename cancelled.");
                } else if c == '\x08' || c == '\x7f' {
                    input.pop();
                    self.mode = Mode::Rename(orig, input);
                } else if !c.is_control() {
                    input.push(c);
                    self.mode = Mode::Rename(orig, input);
                }
                Some(FileManagerAction::Redraw)
            }
            Mode::Copy(_, input) => {
                let mut input = input.clone();
                let src = match &self.mode {
                    Mode::Copy(s, _) => s.clone(),
                    _ => unreachable!(),
                };
                if c == '\n' || c == '\r' {
                    if !input.is_empty() {
                        match vfs.copy_file(&src, &input) {
                            Ok(()) => {
                                self.status_message = format!("Copied to: {}", input);
                            }
                            Err(e) => {
                                self.status_message = format!("Copy failed: {}", e);
                            }
                        }
                        self.refresh(vfs);
                    }
                    self.mode = Mode::Normal;
                } else if c == '\x1b' {
                    self.mode = Mode::Normal;
                    self.status_message = String::from("Copy cancelled.");
                } else if c == '\x08' || c == '\x7f' {
                    input.pop();
                    self.mode = Mode::Copy(src, input);
                } else if !c.is_control() {
                    input.push(c);
                    self.mode = Mode::Copy(src, input);
                }
                Some(FileManagerAction::Redraw)
            }
            Mode::Move(_, input) => {
                let mut input = input.clone();
                let src = match &self.mode {
                    Mode::Move(s, _) => s.clone(),
                    _ => unreachable!(),
                };
                if c == '\n' || c == '\r' {
                    if !input.is_empty() {
                        match vfs.move_file(&src, &input) {
                            Ok(()) => {
                                self.status_message = format!("Moved to: {}", input);
                            }
                            Err(e) => {
                                self.status_message = format!("Move failed: {}", e);
                            }
                        }
                        self.refresh(vfs);
                    }
                    self.mode = Mode::Normal;
                } else if c == '\x1b' {
                    self.mode = Mode::Normal;
                    self.status_message = String::from("Move cancelled.");
                } else if c == '\x08' || c == '\x7f' {
                    input.pop();
                    self.mode = Mode::Move(src, input);
                } else if !c.is_control() {
                    input.push(c);
                    self.mode = Mode::Move(src, input);
                }
                Some(FileManagerAction::Redraw)
            }
            Mode::NewEntry(input, is_dir) => {
                let mut input = input.clone();
                let is_dir = *is_dir;
                if c == '\n' || c == '\r' {
                    if !input.is_empty() {
                        let full = if self.current_path.ends_with('/') {
                            format!("{}{}", self.current_path, input)
                        } else {
                            format!("{}/{}", self.current_path, input)
                        };
                        if is_dir {
                            match vfs.mkdir(&full) {
                                Ok(()) => {
                                    self.status_message = format!("Created directory: {}", input);
                                }
                                Err(e) => {
                                    self.status_message = format!("mkdir failed: {}", e);
                                }
                            }
                        } else {
                            match vfs.touch(&full) {
                                Ok(()) => {
                                    self.status_message = format!("Created file: {}", input);
                                }
                                Err(e) => {
                                    self.status_message = format!("touch failed: {}", e);
                                }
                            }
                        }
                        self.refresh(vfs);
                    }
                    self.mode = Mode::Normal;
                } else if c == '\x1b' {
                    self.mode = Mode::Normal;
                    self.status_message = String::from("Cancelled.");
                } else if c == '\x08' || c == '\x7f' {
                    input.pop();
                    self.mode = Mode::NewEntry(input, is_dir);
                } else if c == '\t' {
                    // Tab toggles between file and directory creation.
                    self.mode = Mode::NewEntry(input, !is_dir);
                    let kind = if !is_dir { "directory" } else { "file" };
                    self.status_message = format!("Creating {} (Tab to toggle): ", kind);
                } else if !c.is_control() {
                    input.push(c);
                    self.mode = Mode::NewEntry(input, is_dir);
                }
                Some(FileManagerAction::Redraw)
            }
            Mode::Search(input) => {
                let mut input = input.clone();
                if c == '\n' || c == '\r' {
                    self.filter = input;
                    self.mode = Mode::Normal;
                    self.refresh(vfs);
                } else if c == '\x1b' {
                    self.filter.clear();
                    self.mode = Mode::Normal;
                    self.refresh(vfs);
                    self.status_message = String::from("Search cleared.");
                } else if c == '\x08' || c == '\x7f' {
                    input.pop();
                    self.mode = Mode::Search(input);
                } else if !c.is_control() {
                    input.push(c);
                    self.mode = Mode::Search(input);
                }
                Some(FileManagerAction::Redraw)
            }
        }
    }

    /// Handle a character in normal browsing mode.
    fn handle_normal_char<V: claudio_shell::Vfs>(
        &mut self,
        c: char,
        _vfs: &mut V,
    ) -> Option<FileManagerAction> {
        match c {
            // Backspace — go to parent directory.
            '\x08' | '\x7f' => Some(FileManagerAction::GoParent),
            // Enter — open selected.
            '\n' | '\r' => Some(FileManagerAction::Enter),
            // d — delete selected.
            'd' => {
                if self.entries.is_empty() {
                    return Some(FileManagerAction::Redraw);
                }
                let entry = &self.entries[self.selected_index];
                if entry.name == "." || entry.name == ".." {
                    self.status_message = String::from("Cannot delete . or ..");
                    return Some(FileManagerAction::Redraw);
                }
                let full_path = if self.current_path.ends_with('/') {
                    format!("{}{}", self.current_path, entry.name)
                } else {
                    format!("{}/{}", self.current_path, entry.name)
                };
                self.status_message = format!("Delete {}? (y/n)", entry.name);
                self.mode = Mode::ConfirmDelete(full_path);
                Some(FileManagerAction::Redraw)
            }
            // r — rename selected.
            'r' => {
                if self.entries.is_empty() {
                    return Some(FileManagerAction::Redraw);
                }
                let entry = &self.entries[self.selected_index];
                if entry.name == "." || entry.name == ".." {
                    self.status_message = String::from("Cannot rename . or ..");
                    return Some(FileManagerAction::Redraw);
                }
                let full_path = if self.current_path.ends_with('/') {
                    format!("{}{}", self.current_path, entry.name)
                } else {
                    format!("{}/{}", self.current_path, entry.name)
                };
                self.status_message = format!("Rename: {}", entry.name);
                self.mode = Mode::Rename(full_path, String::new());
                Some(FileManagerAction::Redraw)
            }
            // c — copy selected.
            'c' => {
                if self.entries.is_empty() {
                    return Some(FileManagerAction::Redraw);
                }
                let entry = &self.entries[self.selected_index];
                if entry.name == "." || entry.name == ".." {
                    self.status_message = String::from("Cannot copy . or ..");
                    return Some(FileManagerAction::Redraw);
                }
                let full_path = if self.current_path.ends_with('/') {
                    format!("{}{}", self.current_path, entry.name)
                } else {
                    format!("{}/{}", self.current_path, entry.name)
                };
                self.status_message = String::from("Copy to: ");
                self.mode = Mode::Copy(full_path, String::new());
                Some(FileManagerAction::Redraw)
            }
            // m — move selected.
            'm' => {
                if self.entries.is_empty() {
                    return Some(FileManagerAction::Redraw);
                }
                let entry = &self.entries[self.selected_index];
                if entry.name == "." || entry.name == ".." {
                    self.status_message = String::from("Cannot move . or ..");
                    return Some(FileManagerAction::Redraw);
                }
                let full_path = if self.current_path.ends_with('/') {
                    format!("{}{}", self.current_path, entry.name)
                } else {
                    format!("{}/{}", self.current_path, entry.name)
                };
                self.status_message = String::from("Move to: ");
                self.mode = Mode::Move(full_path, String::new());
                Some(FileManagerAction::Redraw)
            }
            // n — new file/directory.
            'n' => {
                self.status_message = String::from("New file name (Tab for directory): ");
                self.mode = Mode::NewEntry(String::new(), false);
                Some(FileManagerAction::Redraw)
            }
            // / — search/filter.
            '/' => {
                self.status_message = String::from("Search: ");
                self.mode = Mode::Search(String::new());
                Some(FileManagerAction::Redraw)
            }
            _ => None,
        }
    }

    /// Handle a raw key (arrow keys, etc.). Returns an action if the key was consumed.
    pub fn handle_raw_key(&mut self, key: KeyCode, visible_rows: usize) -> Option<FileManagerAction> {
        match key {
            KeyCode::ArrowUp => {
                self.move_up();
                Some(FileManagerAction::Redraw)
            }
            KeyCode::ArrowDown => {
                self.move_down(visible_rows);
                Some(FileManagerAction::Redraw)
            }
            KeyCode::Home => {
                self.selected_index = 0;
                self.scroll_offset = 0;
                Some(FileManagerAction::Redraw)
            }
            KeyCode::End => {
                if !self.entries.is_empty() {
                    self.selected_index = self.entries.len() - 1;
                    let max_visible = visible_rows.saturating_sub(4);
                    if self.selected_index >= max_visible {
                        self.scroll_offset = self.selected_index - max_visible + 1;
                    }
                }
                Some(FileManagerAction::Redraw)
            }
            KeyCode::PageUp => {
                let page = visible_rows.saturating_sub(4);
                self.selected_index = self.selected_index.saturating_sub(page);
                if self.selected_index < self.scroll_offset {
                    self.scroll_offset = self.selected_index;
                }
                Some(FileManagerAction::Redraw)
            }
            KeyCode::PageDown => {
                let page = visible_rows.saturating_sub(4);
                self.selected_index = (self.selected_index + page).min(
                    if self.entries.is_empty() { 0 } else { self.entries.len() - 1 },
                );
                let max_visible = visible_rows.saturating_sub(4);
                if self.selected_index >= self.scroll_offset + max_visible {
                    self.scroll_offset = self.selected_index - max_visible + 1;
                }
                Some(FileManagerAction::Redraw)
            }
            _ => None,
        }
    }
}

/// Actions returned by the file manager input handlers.
pub enum FileManagerAction {
    /// Redraw the file manager pane (selection changed, mode changed, etc.).
    Redraw,
    /// Enter the selected entry (open dir or file).
    Enter,
    /// Go to the parent directory.
    GoParent,
    /// Open a file in the editor pane. Contains the file path.
    OpenFile(String),
}

// ---------------------------------------------------------------------------
// Rendering — produces ANSI escape sequences for the pane
// ---------------------------------------------------------------------------

/// Format a file size in human-readable form.
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}K", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1}M", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1}G", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Render the file manager state into ANSI escape sequences for a terminal pane.
///
/// `cols` and `rows` are the pane dimensions in characters.
pub fn render_to_pane(state: &FileManagerState, cols: usize, rows: usize) -> String {
    let mut out = String::with_capacity(2048);

    // Clear screen and home cursor.
    out.push_str("\x1b[2J\x1b[H");

    if rows < 5 || cols < 20 {
        out.push_str("\x1b[31mPane too small\x1b[0m");
        return out;
    }

    // -- Row 1: Path bar (blue background) --
    let path_label = format!(" {} ", state.current_path);
    let path_display = if path_label.len() > cols {
        let start = path_label.len() - cols + 3;
        format!("...{}", &path_label[start..])
    } else {
        path_label.clone()
    };
    out.push_str(&format!(
        "\x1b[44;97m{:<width$}\x1b[0m\r\n",
        path_display,
        width = cols
    ));

    // -- Row 2: Column headers --
    let name_width = if cols > 40 { cols - 28 } else { cols - 10 };
    out.push_str(&format!(
        "\x1b[1;37m{:<nw$} {:>8} {:>6} {:>10}\x1b[0m\r\n",
        "Name",
        "Size",
        "Type",
        "Modified",
        nw = name_width,
    ));

    // -- Separator --
    let sep: String = core::iter::repeat('-').take(cols).collect();
    out.push_str(&format!("\x1b[90m{}\x1b[0m\r\n", sep));

    // -- File listing --
    // Reserve rows: 1 path bar + 1 headers + 1 separator + 1 footer + 1 status = 5
    let listing_rows = rows.saturating_sub(5);

    if !state.vfs_available && state.entries.len() <= 2 {
        // No VFS — show message.
        out.push_str("\r\n");
        out.push_str("\x1b[33m  No filesystem mounted.\x1b[0m\r\n");
        out.push_str("\x1b[90m  Use `mount` in a shell pane to attach a device.\x1b[0m\r\n");
    } else {
        let end = (state.scroll_offset + listing_rows).min(state.entries.len());
        let start = state.scroll_offset;

        for i in start..end {
            let entry = &state.entries[i];
            let is_selected = i == state.selected_index;

            // Color based on entry kind.
            let color_code = match entry.kind {
                EntryKind::Directory => "34",   // Blue
                EntryKind::Executable => "32",  // Green
                EntryKind::Symlink => "36",     // Cyan
                EntryKind::File => "37",        // White
            };

            // Type label.
            let type_label = match entry.kind {
                EntryKind::Directory => "DIR",
                EntryKind::Executable => "EXEC",
                EntryKind::Symlink => "LINK",
                EntryKind::File => "FILE",
            };

            // Size display.
            let size_str = match entry.kind {
                EntryKind::Directory => String::from("<DIR>"),
                _ => format_size(entry.size),
            };

            // Modified display.
            let mod_str = if entry.modified.is_empty() {
                String::from("--")
            } else {
                entry.modified.clone()
            };

            // Truncate name if needed.
            let display_name = if entry.name.len() > name_width {
                format!("{}~", &entry.name[..name_width - 1])
            } else {
                entry.name.clone()
            };

            if is_selected {
                // Inverse video for selected item.
                out.push_str(&format!(
                    "\x1b[7;{}m{:<nw$} {:>8} {:>6} {:>10}\x1b[0m\r\n",
                    color_code,
                    display_name,
                    size_str,
                    type_label,
                    mod_str,
                    nw = name_width,
                ));
            } else {
                out.push_str(&format!(
                    "\x1b[{}m{:<nw$}\x1b[37m {:>8} {:>6} {:>10}\x1b[0m\r\n",
                    color_code,
                    display_name,
                    size_str,
                    type_label,
                    mod_str,
                    nw = name_width,
                ));
            }
        }
    }

    // -- Footer: file count + total size --
    // Move to the second-to-last row.
    let footer_row = rows - 1;
    out.push_str(&format!("\x1b[{};1H", footer_row));

    let file_count = state.entries.iter().filter(|e| e.name != "." && e.name != "..").count();
    let total_size: u64 = state.entries.iter().map(|e| e.size).sum();
    let filter_info = if state.filter.is_empty() {
        String::new()
    } else {
        format!(" [filter: {}]", state.filter)
    };
    let footer = format!(
        " {} items, {}{} ",
        file_count,
        format_size(total_size),
        filter_info,
    );
    out.push_str(&format!(
        "\x1b[90m{:<width$}\x1b[0m\r\n",
        footer,
        width = cols,
    ));

    // -- Status / mode line (last row) --
    let status_line = match &state.mode {
        Mode::Normal => {
            if state.status_message.is_empty() {
                String::from(" \x1b[90mUp/Down:nav  Enter:open  Backspace:parent  d:del  r:rename  c:copy  m:move  n:new  /:search\x1b[0m")
            } else {
                format!(" \x1b[93m{}\x1b[0m", state.status_message)
            }
        }
        Mode::ConfirmDelete(_) => {
            format!(" \x1b[91m{}\x1b[0m", state.status_message)
        }
        Mode::Rename(_, input) => {
            format!(" \x1b[93mRename to: {}\x1b[5m_\x1b[0m", input)
        }
        Mode::Copy(_, input) => {
            format!(" \x1b[93mCopy to: {}\x1b[5m_\x1b[0m", input)
        }
        Mode::Move(_, input) => {
            format!(" \x1b[93mMove to: {}\x1b[5m_\x1b[0m", input)
        }
        Mode::NewEntry(input, is_dir) => {
            let kind = if *is_dir { "directory" } else { "file" };
            format!(" \x1b[93mNew {} name: {}\x1b[5m_\x1b[0m", kind, input)
        }
        Mode::Search(input) => {
            format!(" \x1b[93mSearch: {}\x1b[5m_\x1b[0m", input)
        }
    };
    // Truncate status if too long.
    out.push_str(&status_line);

    out
}
