//! Text-mode web browser pane for ClaudioOS.
//!
//! Uses wraith-dom for HTML parsing, wraith-render for text-mode rendering,
//! and claudio-net for HTTP/HTTPS fetching. Provides a lynx-like browsing
//! experience inside a dashboard pane.

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use claudio_net::http::HttpRequest;
use claudio_net::stack::NetworkStack;
use claudio_net::tls::https_request;
use claudio_net::Instant;

/// A clickable link extracted from the rendered page.
pub struct BrowserLink {
    /// The target URL (absolute or relative).
    pub url: String,
    /// Row in the rendered text grid.
    pub row: usize,
    /// Display text of the link.
    pub label: String,
}

/// Status of the browser pane.
#[derive(Clone)]
pub enum BrowserStatus {
    /// Showing about:blank or idle.
    Idle,
    /// Currently loading a page.
    Loading,
    /// Page loaded successfully.
    Done(usize),
    /// An error occurred.
    Error(String),
}

/// Input mode for the browser pane.
#[derive(Clone, PartialEq)]
pub enum BrowserInputMode {
    /// Normal browsing — keys are navigation commands.
    Normal,
    /// URL bar is active — keys are typed into the URL input.
    UrlInput,
    /// Link number input — typing a number to follow a link.
    LinkInput,
}

/// State for a text-mode web browser pane.
pub struct BrowserState {
    /// The currently displayed URL.
    pub current_url: String,
    /// Rendered text lines for the current page.
    pub page_lines: Vec<String>,
    /// Scroll offset (line index of the top visible line).
    pub scroll_offset: usize,
    /// Navigation history (URLs visited).
    pub history: Vec<String>,
    /// Links extracted from the current page.
    pub links: Vec<BrowserLink>,
    /// Current status.
    pub status: BrowserStatus,
    /// Current input mode.
    pub input_mode: BrowserInputMode,
    /// URL bar input buffer (used during UrlInput mode).
    pub url_input: String,
    /// Link number input buffer (used during LinkInput mode).
    pub link_input: String,
    /// Layout pane id this browser is bound to.
    pub pane_id: usize,
}

impl BrowserState {
    /// Create a new browser state bound to a layout pane.
    pub fn new(pane_id: usize) -> Self {
        let mut page_lines = Vec::new();
        page_lines.push(String::from(""));
        page_lines.push(String::from("  \x1b[96mClaudioOS Web Browser\x1b[0m"));
        page_lines.push(String::from("  \x1b[90m──────────────────────────────\x1b[0m"));
        page_lines.push(String::from(""));
        page_lines.push(String::from("  Type \x1b[93mg\x1b[0m to navigate to a URL"));
        page_lines.push(String::from(""));
        page_lines.push(String::from("  \x1b[90mKeys: g=go  b=back  r=reload  q=close\x1b[0m"));
        page_lines.push(String::from("  \x1b[90m      Up/Down/PgUp/PgDn=scroll\x1b[0m"));
        page_lines.push(String::from("  \x1b[90m      f=follow link (enter number)\x1b[0m"));

        Self {
            current_url: String::from("about:blank"),
            page_lines,
            scroll_offset: 0,
            history: Vec::new(),
            links: Vec::new(),
            status: BrowserStatus::Idle,
            input_mode: BrowserInputMode::Normal,
            url_input: String::new(),
            link_input: String::new(),
            pane_id,
        }
    }

    /// Navigate to a URL, fetching and rendering the page.
    pub fn navigate(
        &mut self,
        url: &str,
        stack: &mut NetworkStack,
        now: fn() -> Instant,
    ) {
        log::info!("[browser] navigating to: {}", url);
        self.status = BrowserStatus::Loading;

        // Push current URL to history before navigating.
        if self.current_url != "about:blank" {
            self.history.push(self.current_url.clone());
        }
        self.current_url = String::from(url);
        self.scroll_offset = 0;
        self.links.clear();
        self.page_lines.clear();
        self.page_lines.push(String::from("  \x1b[33mLoading...\x1b[0m"));

        // Fetch the page.
        let result = self.fetch_page(url, stack, now);

        match result {
            Ok(html) => {
                let body_len = html.len();
                self.render_html(&html);
                self.status = BrowserStatus::Done(body_len);
                log::info!("[browser] loaded {} ({} bytes)", url, body_len);
            }
            Err(e) => {
                self.page_lines.clear();
                self.page_lines.push(String::new());
                self.page_lines.push(format!("  \x1b[31mError loading {}\x1b[0m", url));
                self.page_lines.push(format!("  \x1b[31m{}\x1b[0m", e));
                self.page_lines.push(String::new());
                self.page_lines.push(String::from("  Press \x1b[93mb\x1b[0m to go back or \x1b[93mg\x1b[0m to try another URL."));
                self.status = BrowserStatus::Error(e);
                log::error!("[browser] failed to load {}", url);
            }
        }
    }

    /// Fetch a page over HTTP/HTTPS. Returns the body as a string.
    fn fetch_page(
        &self,
        url: &str,
        stack: &mut NetworkStack,
        now: fn() -> Instant,
    ) -> Result<String, String> {
        // Parse URL.
        let (is_https, host, port, path) = parse_url(url)?;

        // Build HTTP request.
        let req = HttpRequest::get(&host, &path)
            .header("Connection", "close")
            .header("User-Agent", "ClaudioOS/0.1 wraith-browser")
            .header("Accept", "text/html, text/plain, */*");

        let request_bytes = req.to_bytes();

        if is_https {
            let rng_seed = now().total_millis() as u64;
            let raw_response = https_request(stack, &host, port, &request_bytes, now, rng_seed)
                .map_err(|e| format!("TLS error: {:?}", e))?;

            parse_http_response(&raw_response)
        } else {
            // Plain HTTP via TCP.
            use claudio_net::tls::{tcp_connect, tcp_send, tcp_recv, tcp_close};

            let ip = claudio_net::dns::resolve(stack, &host, || now())
                .map_err(|e| format!("DNS error: {:?}", e))?;

            static mut HTTP_PORT: u16 = 55000;
            let local_port = unsafe {
                let p = HTTP_PORT;
                HTTP_PORT = if HTTP_PORT >= 65534 { 55000 } else { HTTP_PORT + 1 };
                p
            };

            let handle = tcp_connect(stack, ip, port, local_port, now)
                .map_err(|e| format!("TCP connect error: {:?}", e))?;

            tcp_send(stack, handle, &request_bytes, now)
                .map_err(|e| format!("TCP send error: {:?}", e))?;

            let mut response = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match tcp_recv(stack, handle, &mut buf, now) {
                    Ok(0) => break,
                    Ok(n) => {
                        response.extend_from_slice(&buf[..n]);
                        if response.len() > 512 * 1024 {
                            break; // Cap at 512KB.
                        }
                    }
                    Err(_) => {
                        if !response.is_empty() {
                            break;
                        }
                        tcp_close(stack, handle);
                        return Err(String::from("TCP receive error"));
                    }
                }
            }
            tcp_close(stack, handle);
            parse_http_response(&response)
        }
    }

    /// Parse HTML and render to text lines with numbered links.
    fn render_html(&mut self, html: &str) {
        let doc = wraith_dom::parse(html);
        let page = wraith_render::render(&doc, 78, 2000);

        self.page_lines.clear();
        self.links.clear();

        // Title line.
        if !page.title.is_empty() {
            self.page_lines.push(format!("  \x1b[96m{}\x1b[0m", page.title));
            self.page_lines.push(String::from("  \x1b[90m──────────────────────────────────────────────────\x1b[0m"));
        }

        // Render cell grid to text lines.
        for (row_idx, row) in page.cells.iter().enumerate() {
            let mut line = String::with_capacity(page.width + 16);
            line.push_str("  "); // Left margin.

            // Check if any link starts on this row.
            let row_links: Vec<(usize, &wraith_render::LinkRegion)> = page
                .links
                .iter()
                .enumerate()
                .filter(|(_, lr)| lr.row == row_idx)
                .collect();

            for cell in row.iter() {
                line.push(cell.ch);
            }

            // Trim trailing spaces.
            let trimmed_len = line.trim_end().len();
            line.truncate(trimmed_len);

            // Append link references at end of line.
            for (link_idx, lr) in &row_links {
                let global_idx = self.links.len();
                self.links.push(BrowserLink {
                    url: lr.url.clone(),
                    row: self.page_lines.len(),
                    label: {
                        // Extract the link text from the cells.
                        let start = lr.col_start.min(row.len());
                        let end = lr.col_end.min(row.len());
                        row[start..end].iter().map(|c| c.ch).collect::<String>()
                    },
                });
                line.push_str(&format!(" \x1b[33m[{}]\x1b[0m", global_idx));
                let _ = link_idx; // suppress unused warning
            }

            self.page_lines.push(line);
        }

        // Append link index at the bottom.
        if !self.links.is_empty() {
            self.page_lines.push(String::new());
            self.page_lines.push(String::from("  \x1b[90m── Links ──────────────────────────────────────────\x1b[0m"));
            for (i, link) in self.links.iter().enumerate() {
                let label = if link.label.is_empty() {
                    &link.url
                } else {
                    &link.label
                };
                self.page_lines.push(format!(
                    "  \x1b[33m[{}]\x1b[0m \x1b[4m{}\x1b[0m  \x1b[90m{}\x1b[0m",
                    i,
                    label.trim(),
                    link.url
                ));
            }
        }
    }

    /// Render the browser content into a terminal pane.
    /// Returns a string of ANSI escape sequences to write to the pane.
    pub fn render_to_pane(&self, pane_rows: usize) -> String {
        let mut output = String::with_capacity(4096);

        // Clear pane.
        output.push_str("\x1b[2J\x1b[H");

        // URL bar (row 1).
        let status_text = match &self.status {
            BrowserStatus::Idle => String::from("about:blank"),
            BrowserStatus::Loading => String::from("\x1b[33mLoading...\x1b[0m"),
            BrowserStatus::Done(bytes) => format!("\x1b[92mDone ({} bytes)\x1b[0m", bytes),
            BrowserStatus::Error(e) => format!("\x1b[31mError: {}\x1b[0m", e),
        };

        match &self.input_mode {
            BrowserInputMode::UrlInput => {
                output.push_str(&format!(
                    "\x1b[44m\x1b[37m URL: {}\x1b[K\x1b[0m\r\n",
                    self.url_input
                ));
            }
            BrowserInputMode::LinkInput => {
                output.push_str(&format!(
                    "\x1b[44m\x1b[37m Link #: {}\x1b[K\x1b[0m\r\n",
                    self.link_input
                ));
            }
            BrowserInputMode::Normal => {
                output.push_str(&format!(
                    "\x1b[44m\x1b[37m {}\x1b[K\x1b[0m  {}\r\n",
                    self.current_url, status_text
                ));
            }
        }

        // Separator.
        output.push_str("\x1b[90m────────────────────────────────────────────────────────────\x1b[0m\r\n");

        // Page content (rows 3..pane_rows-1).
        let content_rows = if pane_rows > 3 { pane_rows - 3 } else { 1 };
        let total_lines = self.page_lines.len();

        for i in 0..content_rows {
            let line_idx = self.scroll_offset + i;
            if line_idx < total_lines {
                output.push_str(&self.page_lines[line_idx]);
            }
            output.push_str("\r\n");
        }

        // Status bar (last row).
        let scroll_info = if total_lines > content_rows {
            let pct = if total_lines == 0 {
                100
            } else {
                ((self.scroll_offset + content_rows) * 100) / total_lines.max(1)
            };
            format!("{}%", pct.min(100))
        } else {
            String::from("All")
        };
        let links_info = if self.links.is_empty() {
            String::new()
        } else {
            format!(" | {} links", self.links.len())
        };
        output.push_str(&format!(
            "\x1b[7m g=go b=back r=reload f=follow q=close | {}{} \x1b[K\x1b[0m",
            scroll_info, links_info
        ));

        output
    }

    /// Scroll down by `n` lines.
    pub fn scroll_down(&mut self, n: usize) {
        let max = self.page_lines.len().saturating_sub(1);
        self.scroll_offset = (self.scroll_offset + n).min(max);
    }

    /// Scroll up by `n` lines.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
    }

    /// Go back in history.
    pub fn go_back(&mut self, stack: &mut NetworkStack, now: fn() -> Instant) {
        if let Some(prev_url) = self.history.pop() {
            let url = prev_url.clone();
            // Don't push current_url to history again (navigate does that).
            self.current_url = String::from("about:blank");
            self.navigate(&url, stack, now);
            // Remove the duplicate history entry that navigate() added.
            // (We already have it from pop, navigate pushes about:blank which we don't want.)
            if let Some(last) = self.history.last() {
                if last == "about:blank" {
                    self.history.pop();
                }
            }
        }
    }

    /// Follow a link by its index number.
    pub fn follow_link(&mut self, idx: usize, stack: &mut NetworkStack, now: fn() -> Instant) {
        if idx >= self.links.len() {
            return;
        }

        let link_url = self.links[idx].url.clone();

        // Resolve relative URLs.
        let absolute_url = resolve_url(&self.current_url, &link_url);
        self.navigate(&absolute_url, stack, now);
    }

    /// Handle a keyboard character in the browser pane.
    /// Returns `true` if the browser consumed the key, `false` if it should
    /// be passed through (e.g., 'q' to close the pane).
    pub fn handle_key(&mut self, c: char, stack: &mut NetworkStack, now: fn() -> Instant) -> BrowserKeyResult {
        match &self.input_mode {
            BrowserInputMode::UrlInput => {
                if c == '\n' || c == '\r' {
                    // Submit URL.
                    let url = core::mem::replace(&mut self.url_input, String::new());
                    self.input_mode = BrowserInputMode::Normal;
                    if !url.is_empty() {
                        // Add scheme if missing.
                        let url = if !url.starts_with("http://") && !url.starts_with("https://") {
                            format!("https://{}", url)
                        } else {
                            url
                        };
                        self.navigate(&url, stack, now);
                    }
                    BrowserKeyResult::Consumed
                } else if c == '\x1b' {
                    // Escape — cancel URL input.
                    self.url_input.clear();
                    self.input_mode = BrowserInputMode::Normal;
                    BrowserKeyResult::Consumed
                } else if c == '\x08' || c == '\x7f' {
                    // Backspace.
                    self.url_input.pop();
                    BrowserKeyResult::Consumed
                } else if !c.is_control() {
                    self.url_input.push(c);
                    BrowserKeyResult::Consumed
                } else {
                    BrowserKeyResult::Consumed
                }
            }
            BrowserInputMode::LinkInput => {
                if c == '\n' || c == '\r' {
                    // Submit link number.
                    let num_str = core::mem::replace(&mut self.link_input, String::new());
                    self.input_mode = BrowserInputMode::Normal;
                    if let Ok(idx) = num_str.parse::<usize>() {
                        self.follow_link(idx, stack, now);
                    }
                    BrowserKeyResult::Consumed
                } else if c == '\x1b' {
                    self.link_input.clear();
                    self.input_mode = BrowserInputMode::Normal;
                    BrowserKeyResult::Consumed
                } else if c == '\x08' || c == '\x7f' {
                    self.link_input.pop();
                    BrowserKeyResult::Consumed
                } else if c.is_ascii_digit() {
                    self.link_input.push(c);
                    BrowserKeyResult::Consumed
                } else {
                    BrowserKeyResult::Consumed
                }
            }
            BrowserInputMode::Normal => {
                match c {
                    'g' => {
                        self.input_mode = BrowserInputMode::UrlInput;
                        self.url_input.clear();
                        BrowserKeyResult::Consumed
                    }
                    'b' => {
                        self.go_back(stack, now);
                        BrowserKeyResult::Consumed
                    }
                    'r' => {
                        let url = self.current_url.clone();
                        if url != "about:blank" {
                            // Don't push to history on reload.
                            self.current_url = String::from("about:blank");
                            self.navigate(&url, stack, now);
                        }
                        BrowserKeyResult::Consumed
                    }
                    'f' => {
                        if !self.links.is_empty() {
                            self.input_mode = BrowserInputMode::LinkInput;
                            self.link_input.clear();
                        }
                        BrowserKeyResult::Consumed
                    }
                    'j' => {
                        self.scroll_down(1);
                        BrowserKeyResult::Consumed
                    }
                    'k' => {
                        self.scroll_up(1);
                        BrowserKeyResult::Consumed
                    }
                    ' ' => {
                        self.scroll_down(20);
                        BrowserKeyResult::Consumed
                    }
                    'q' => {
                        BrowserKeyResult::CloseBrowser
                    }
                    _ => BrowserKeyResult::Consumed,
                }
            }
        }
    }

    /// Handle a raw key (arrows, page up/down) in the browser pane.
    pub fn handle_raw_key(&mut self, key: pc_keyboard::KeyCode) -> BrowserKeyResult {
        use pc_keyboard::KeyCode;
        match key {
            KeyCode::ArrowUp => {
                self.scroll_up(1);
                BrowserKeyResult::Consumed
            }
            KeyCode::ArrowDown => {
                self.scroll_down(1);
                BrowserKeyResult::Consumed
            }
            KeyCode::PageUp => {
                self.scroll_up(20);
                BrowserKeyResult::Consumed
            }
            KeyCode::PageDown => {
                self.scroll_down(20);
                BrowserKeyResult::Consumed
            }
            KeyCode::Home => {
                self.scroll_offset = 0;
                BrowserKeyResult::Consumed
            }
            KeyCode::End => {
                self.scroll_offset = self.page_lines.len().saturating_sub(1);
                BrowserKeyResult::Consumed
            }
            _ => BrowserKeyResult::Consumed,
        }
    }
}

/// Result of a browser key handler.
pub enum BrowserKeyResult {
    /// Key was consumed by the browser.
    Consumed,
    /// The user pressed 'q' to close the browser pane.
    CloseBrowser,
}

// ---------------------------------------------------------------------------
// URL parsing helpers
// ---------------------------------------------------------------------------

/// Parse a URL into (is_https, host, port, path).
fn parse_url(url: &str) -> Result<(bool, String, u16, String), String> {
    let (is_https, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(format!("unsupported scheme in: {}", url));
    };

    let (host_port, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], String::from(&rest[idx..])),
        None => (rest, String::from("/")),
    };

    let (host, port) = match host_port.rfind(':') {
        Some(idx) => {
            let port_str = &host_port[idx + 1..];
            let port: u16 = port_str
                .parse()
                .map_err(|_| format!("bad port: {}", port_str))?;
            (String::from(&host_port[..idx]), port)
        }
        None => {
            let default_port = if is_https { 443 } else { 80 };
            (String::from(host_port), default_port)
        }
    };

    Ok((is_https, host, port, path))
}

/// Parse an HTTP response, extracting the body as a string.
fn parse_http_response(raw: &[u8]) -> Result<String, String> {
    let resp = claudio_net::http::HttpResponse::parse(raw)
        .map_err(|e| format!("HTTP parse error: {:?}", e))?;

    if resp.status >= 300 && resp.status < 400 {
        // Check for redirect Location header.
        for (name, value) in &resp.headers {
            if name.eq_ignore_ascii_case("location") {
                return Err(format!("Redirect to: {}", value));
            }
        }
    }

    if resp.status >= 400 {
        return Err(format!("HTTP {}", resp.status));
    }

    let body = if resp.is_chunked() {
        claudio_net::http::decode_chunked(&resp.body).unwrap_or(resp.body)
    } else {
        resp.body
    };

    // Convert bytes to string (lossy UTF-8).
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Resolve a potentially relative URL against a base URL.
fn resolve_url(base: &str, relative: &str) -> String {
    // Already absolute.
    if relative.starts_with("http://") || relative.starts_with("https://") {
        return String::from(relative);
    }

    // Protocol-relative.
    if relative.starts_with("//") {
        if base.starts_with("https://") {
            return format!("https:{}", relative);
        } else {
            return format!("http:{}", relative);
        }
    }

    // Extract scheme + host from base.
    let (scheme_host, _base_path) = if let Some(rest) = base.strip_prefix("https://") {
        let host_end = rest.find('/').unwrap_or(rest.len());
        (format!("https://{}", &rest[..host_end]), &rest[host_end..])
    } else if let Some(rest) = base.strip_prefix("http://") {
        let host_end = rest.find('/').unwrap_or(rest.len());
        (format!("http://{}", &rest[..host_end]), &rest[host_end..])
    } else {
        return String::from(relative);
    };

    if relative.starts_with('/') {
        // Absolute path.
        format!("{}{}", scheme_host, relative)
    } else {
        // Relative path — append to base directory.
        let base_dir = if let Some(last_slash) = _base_path.rfind('/') {
            &_base_path[..=last_slash]
        } else {
            "/"
        };
        format!("{}{}{}", scheme_host, base_dir, relative)
    }
}
