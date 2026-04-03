//! Screensaver module — TempleOS-inspired screensavers for ClaudioOS.
//!
//! Modes:
//! - **Starfield**: 3D star field flying through space (classic parallax)
//! - **Matrix**: Falling green characters (Matrix rain)
//! - **Bouncing logo**: "ClaudioOS" bouncing around the screen (DVD style)
//! - **Pipes**: Randomly growing coloured pipes filling the screen
//! - **Clock**: Large digital clock centred on screen
//!
//! Activated after N seconds of idle (default 300s). Any keypress deactivates.

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::framebuffer;
use crate::terminal::FramebufferDrawTarget;
use claudio_terminal::render::{render_char, fill_rect, Color, FONT_WIDTH, FONT_HEIGHT};

// ---------------------------------------------------------------------------
// XorShift64 PRNG — seeded from PIT ticks
// ---------------------------------------------------------------------------

struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0xDEAD_BEEF_CAFE_BABE } else { seed },
        }
    }

    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Random u64 in [0, bound).
    fn next_bounded(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        self.next() % bound
    }

    /// Random i64 in [-half, +half).
    fn next_signed(&mut self, range: u64) -> i64 {
        let half = (range / 2) as i64;
        (self.next_bounded(range) as i64) - half
    }
}

// ---------------------------------------------------------------------------
// Screensaver modes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Starfield,
    Matrix,
    BouncingLogo,
    Pipes,
    Clock,
}

impl Mode {
    pub fn from_name(name: &str) -> Option<Mode> {
        match name {
            "starfield" | "stars" => Some(Mode::Starfield),
            "matrix" | "rain" => Some(Mode::Matrix),
            "bounce" | "logo" | "dvd" => Some(Mode::BouncingLogo),
            "pipes" => Some(Mode::Pipes),
            "clock" | "time" => Some(Mode::Clock),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Starfield => "starfield",
            Mode::Matrix => "matrix",
            Mode::BouncingLogo => "bounce",
            Mode::Pipes => "pipes",
            Mode::Clock => "clock",
        }
    }

    fn next(self) -> Mode {
        match self {
            Mode::Starfield => Mode::Matrix,
            Mode::Matrix => Mode::BouncingLogo,
            Mode::BouncingLogo => Mode::Pipes,
            Mode::Pipes => Mode::Clock,
            Mode::Clock => Mode::Starfield,
        }
    }
}

// ---------------------------------------------------------------------------
// Star (for starfield mode)
// ---------------------------------------------------------------------------

const NUM_STARS: usize = 256;

struct Star {
    /// X position in fixed-point: [-16384, 16384] mapped to screen
    x: i32,
    /// Y position in fixed-point
    y: i32,
    /// Z depth: 1 (close) to 1024 (far)
    z: i32,
    /// Speed multiplier (1-3)
    speed: i32,
}

// ---------------------------------------------------------------------------
// Matrix column (for matrix mode)
// ---------------------------------------------------------------------------

const MAX_MATRIX_COLS: usize = 240;

struct MatrixCol {
    /// Current head row (character grid row)
    head: i32,
    /// Trail length in rows
    length: usize,
    /// Speed: rows to advance per frame
    speed: u8,
    /// Frame counter for speed control
    frame_acc: u8,
    /// Active?
    active: bool,
}

// ---------------------------------------------------------------------------
// Pipe (for pipes mode)
// ---------------------------------------------------------------------------

const MAX_PIPES: usize = 8;
const MAX_PIPE_SEGMENTS: usize = 600;

#[derive(Clone, Copy, PartialEq)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

struct Pipe {
    /// Current column (character grid)
    col: usize,
    /// Current row (character grid)
    row: usize,
    /// Direction of travel
    dir: Dir,
    /// Pipe colour
    color: Color,
    /// How many segments drawn
    segments: usize,
    /// Active?
    active: bool,
}

// ---------------------------------------------------------------------------
// Bouncing logo state
// ---------------------------------------------------------------------------

struct LogoState {
    /// Pixel position X
    x: i32,
    /// Pixel position Y
    y: i32,
    /// Velocity X (pixels per frame)
    vx: i32,
    /// Velocity Y (pixels per frame)
    vy: i32,
    /// Current colour index
    color_idx: usize,
}

// ---------------------------------------------------------------------------
// ScreensaverState
// ---------------------------------------------------------------------------

/// Idle timeout in PIT ticks. Default = 300 seconds * 18.2 ticks/s ≈ 5460.
const DEFAULT_IDLE_TICKS: u64 = 300 * 18;

pub struct ScreensaverState {
    /// Whether the screensaver is currently rendering.
    pub active: bool,
    /// Whether the screensaver is disabled entirely.
    pub disabled: bool,
    /// Current mode.
    pub mode: Mode,
    /// Animation frame counter.
    frame: u64,
    /// Idle timeout in PIT ticks.
    idle_timeout_ticks: u64,
    /// Last keypress tick.
    last_input_tick: u64,
    /// PRNG.
    rng: Rng,

    // -- Starfield state --
    stars: Vec<Star>,

    // -- Matrix state --
    matrix_cols: Vec<MatrixCol>,
    /// Character grid for matrix (stores the char at each position for trail rendering)
    matrix_grid: Vec<u8>,
    matrix_grid_cols: usize,
    matrix_grid_rows: usize,

    // -- Bouncing logo state --
    logo: LogoState,

    // -- Pipes state --
    pipes: Vec<Pipe>,
    /// Character grid for pipes (stores pipe chars so we don't overdraw)
    pipe_grid: Vec<u8>,
    pipe_grid_cols: usize,
    pipe_grid_rows: usize,
}

impl ScreensaverState {
    pub fn new() -> Self {
        let seed = crate::interrupts::tick_count();
        Self {
            active: false,
            disabled: false,
            mode: Mode::Starfield,
            frame: 0,
            idle_timeout_ticks: DEFAULT_IDLE_TICKS,
            last_input_tick: crate::interrupts::tick_count(),
            rng: Rng::new(seed),
            stars: Vec::new(),
            matrix_cols: Vec::new(),
            matrix_grid: Vec::new(),
            matrix_grid_cols: 0,
            matrix_grid_rows: 0,
            logo: LogoState {
                x: 100,
                y: 100,
                vx: 2,
                vy: 1,
                color_idx: 0,
            },
            pipes: Vec::new(),
            pipe_grid: Vec::new(),
            pipe_grid_cols: 0,
            pipe_grid_rows: 0,
        }
    }

    /// Record that input was received (keypress, mouse). Deactivates screensaver.
    /// Returns true if the screensaver was active (i.e., the keypress should be consumed).
    pub fn record_input(&mut self) -> bool {
        self.last_input_tick = crate::interrupts::tick_count();
        if self.active {
            self.active = false;
            self.frame = 0;
            log::info!("[screensaver] deactivated by input");
            true
        } else {
            false
        }
    }

    /// Check if the idle timeout has elapsed and we should activate.
    pub fn check_idle(&mut self) -> bool {
        if self.disabled || self.active {
            return false;
        }
        let now = crate::interrupts::tick_count();
        let elapsed = now.wrapping_sub(self.last_input_tick);
        if elapsed >= self.idle_timeout_ticks {
            self.activate();
            true
        } else {
            false
        }
    }

    /// Activate the screensaver with the current mode.
    pub fn activate(&mut self) {
        self.active = true;
        self.frame = 0;
        log::info!("[screensaver] activated: mode={}", self.mode.name());
        self.init_mode();
    }

    /// Force-activate with a specific mode (for `screensaver <mode>` command).
    pub fn activate_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.activate();
    }

    /// Set idle timeout in seconds.
    pub fn set_timeout(&mut self, seconds: u64) {
        self.idle_timeout_ticks = seconds * 18;
        log::info!("[screensaver] timeout set to {}s ({} ticks)", seconds, self.idle_timeout_ticks);
    }

    /// Initialize state for the current mode.
    fn init_mode(&mut self) {
        let fb_w = framebuffer::width();
        let fb_h = framebuffer::height();
        let cols = fb_w / FONT_WIDTH;
        let rows = fb_h / FONT_HEIGHT;

        match self.mode {
            Mode::Starfield => self.init_starfield(),
            Mode::Matrix => self.init_matrix(cols, rows),
            Mode::BouncingLogo => self.init_logo(fb_w, fb_h),
            Mode::Pipes => self.init_pipes(cols, rows),
            Mode::Clock => {} // No special init needed
        }
    }

    // -- Starfield init --

    fn init_starfield(&mut self) {
        self.stars.clear();
        for _ in 0..NUM_STARS {
            let star = Self::random_star_from(&mut self.rng);
            self.stars.push(star);
        }
    }

    fn random_star_from(rng: &mut Rng) -> Star {
        Star {
            x: rng.next_signed(32768) as i32,
            y: rng.next_signed(32768) as i32,
            z: (rng.next_bounded(1023) + 1) as i32,
            speed: (rng.next_bounded(3) + 1) as i32,
        }
    }

    // -- Matrix init --

    fn init_matrix(&mut self, cols: usize, rows: usize) {
        self.matrix_cols.clear();
        self.matrix_grid_cols = cols;
        self.matrix_grid_rows = rows;
        self.matrix_grid = vec![0u8; cols * rows];

        let num_cols = cols.min(MAX_MATRIX_COLS);
        for i in 0..num_cols {
            let active = self.rng.next_bounded(3) == 0; // ~33% start active
            self.matrix_cols.push(MatrixCol {
                head: -(self.rng.next_bounded(rows as u64) as i32),
                length: (self.rng.next_bounded(20) + 5) as usize,
                speed: (self.rng.next_bounded(3) + 1) as u8,
                frame_acc: 0,
                active,
            });
            let _ = i; // used for iteration
        }
    }

    // -- Logo init --

    fn init_logo(&mut self, fb_w: usize, fb_h: usize) {
        self.logo = LogoState {
            x: (self.rng.next_bounded(fb_w.saturating_sub(200) as u64 + 1)) as i32,
            y: (self.rng.next_bounded(fb_h.saturating_sub(50) as u64 + 1)) as i32,
            vx: if self.rng.next_bounded(2) == 0 { 2 } else { -2 },
            vy: if self.rng.next_bounded(2) == 0 { 1 } else { -1 },
            color_idx: 0,
        };
    }

    // -- Pipes init --

    fn init_pipes(&mut self, cols: usize, rows: usize) {
        self.pipes.clear();
        self.pipe_grid_cols = cols;
        self.pipe_grid_rows = rows;
        self.pipe_grid = vec![0u8; cols * rows];

        // Start with 3 pipes
        for _ in 0..3 {
            self.spawn_pipe(cols, rows);
        }
    }

    fn spawn_pipe(&mut self, cols: usize, rows: usize) {
        if self.pipes.len() >= MAX_PIPES {
            return;
        }
        let colors = [
            Color::new(255, 80, 80),   // red
            Color::new(80, 255, 80),   // green
            Color::new(80, 80, 255),   // blue
            Color::new(255, 255, 80),  // yellow
            Color::new(255, 80, 255),  // magenta
            Color::new(80, 255, 255),  // cyan
            Color::new(255, 160, 80),  // orange
            Color::new(200, 200, 200), // white
        ];
        let dir = match self.rng.next_bounded(4) {
            0 => Dir::Up,
            1 => Dir::Down,
            2 => Dir::Left,
            _ => Dir::Right,
        };
        let ci = self.rng.next_bounded(colors.len() as u64) as usize;
        self.pipes.push(Pipe {
            col: self.rng.next_bounded(cols as u64) as usize,
            row: self.rng.next_bounded(rows as u64) as usize,
            dir,
            color: colors[ci],
            segments: 0,
            active: true,
        });
    }

    // -----------------------------------------------------------------------
    // render_frame — called from the dashboard loop when active
    // -----------------------------------------------------------------------

    /// Render one animation frame. Returns true if the screensaver is still active.
    pub fn render_frame(&mut self) -> bool {
        if !self.active {
            return false;
        }

        let fb_w = framebuffer::width();
        let fb_h = framebuffer::height();
        if fb_w == 0 || fb_h == 0 {
            return false;
        }

        let mut dt = FramebufferDrawTarget;

        match self.mode {
            Mode::Starfield => self.render_starfield(&mut dt, fb_w, fb_h),
            Mode::Matrix => self.render_matrix(&mut dt, fb_w, fb_h),
            Mode::BouncingLogo => self.render_bouncing_logo(&mut dt, fb_w, fb_h),
            Mode::Pipes => self.render_pipes(&mut dt, fb_w, fb_h),
            Mode::Clock => self.render_clock(&mut dt, fb_w, fb_h),
        }

        framebuffer::blit_full();
        self.frame += 1;
        true
    }

    // -----------------------------------------------------------------------
    // Starfield renderer
    // -----------------------------------------------------------------------

    fn render_starfield(&mut self, dt: &mut FramebufferDrawTarget, fb_w: usize, fb_h: usize) {
        // Clear to black
        fill_rect(dt, 0, 0, fb_w, fb_h, Color::new(0, 0, 0));

        let cx = (fb_w / 2) as i32;
        let cy = (fb_h / 2) as i32;

        for i in 0..self.stars.len() {
            // Move star closer (decrease z)
            self.stars[i].z -= self.stars[i].speed * 4;

            if self.stars[i].z <= 0 {
                self.stars[i] = Self::random_star_from(&mut self.rng);
                self.stars[i].z = 1024;
                continue;
            }

            let z = self.stars[i].z;
            // Project 3D -> 2D
            let sx = cx + (self.stars[i].x * 256) / z;
            let sy = cy + (self.stars[i].y * 256) / z;

            if sx < 0 || sx >= fb_w as i32 || sy < 0 || sy >= fb_h as i32 {
                // Off screen, respawn
                self.stars[i] = Self::random_star_from(&mut self.rng);
                self.stars[i].z = 1024;
                continue;
            }

            // Brightness based on distance (closer = brighter)
            let brightness = ((1024 - z) * 255 / 1024) as u8;
            let b = brightness.max(40);

            // Size based on distance: close stars are 2x2, far stars are 1x1
            let sx = sx as usize;
            let sy = sy as usize;
            framebuffer::put_pixel(sx, sy, b, b, b);
            if z < 400 && sx + 1 < fb_w {
                framebuffer::put_pixel(sx + 1, sy, b, b, b);
            }
            if z < 200 && sy + 1 < fb_h {
                framebuffer::put_pixel(sx, sy + 1, b, b, b);
                if sx + 1 < fb_w {
                    framebuffer::put_pixel(sx + 1, sy + 1, b, b, b);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Matrix renderer
    // -----------------------------------------------------------------------

    fn render_matrix(&mut self, dt: &mut FramebufferDrawTarget, fb_w: usize, fb_h: usize) {
        let cols = self.matrix_grid_cols;
        let rows = self.matrix_grid_rows;
        if cols == 0 || rows == 0 {
            return;
        }

        // Dim the whole screen slightly (fade effect)
        // We do this by re-rendering with dimmed colours instead of a true fade.
        fill_rect(dt, 0, 0, fb_w, fb_h, Color::new(0, 0, 0));

        // Advance columns
        for ci in 0..self.matrix_cols.len() {
            self.matrix_cols[ci].frame_acc += 1;
            if self.matrix_cols[ci].frame_acc < self.matrix_cols[ci].speed {
                continue;
            }
            self.matrix_cols[ci].frame_acc = 0;

            if !self.matrix_cols[ci].active {
                // Random chance to reactivate
                if self.rng.next_bounded(20) == 0 {
                    self.matrix_cols[ci].active = true;
                    self.matrix_cols[ci].head = 0;
                    self.matrix_cols[ci].length = (self.rng.next_bounded(20) + 5) as usize;
                    self.matrix_cols[ci].speed = (self.rng.next_bounded(3) + 1) as u8;
                }
                continue;
            }

            self.matrix_cols[ci].head += 1;

            // Place a random character at the head position
            let head_row = self.matrix_cols[ci].head;
            if head_row >= 0 && (head_row as usize) < rows {
                let ch = (self.rng.next_bounded(94) + 33) as u8; // printable ASCII
                let idx = (head_row as usize) * cols + ci;
                if idx < self.matrix_grid.len() {
                    self.matrix_grid[idx] = ch;
                }
            }

            // Deactivate if head has gone far enough past screen
            if head_row as usize > rows + self.matrix_cols[ci].length {
                self.matrix_cols[ci].active = false;
            }
        }

        // Render the grid
        for ci in 0..cols {
            let head = self.matrix_cols.get(ci).map(|c| c.head).unwrap_or(-1);
            let length = self.matrix_cols.get(ci).map(|c| c.length).unwrap_or(10);

            for ri in 0..rows {
                let idx = ri * cols + ci;
                let ch = self.matrix_grid.get(idx).copied().unwrap_or(0);
                if ch == 0 {
                    continue;
                }

                let dist_from_head = head - ri as i32;
                if dist_from_head < 0 {
                    continue; // not yet reached
                }
                if dist_from_head as usize > length {
                    // Past the trail — clear it
                    if idx < self.matrix_grid.len() {
                        self.matrix_grid[idx] = 0;
                    }
                    continue;
                }

                // Colour: head is bright white-green, trail fades to dark green
                let (r, g, b) = if dist_from_head == 0 {
                    (200u8, 255u8, 200u8) // bright head
                } else {
                    let fade = 255u8.saturating_sub((dist_from_head as u8).saturating_mul(10));
                    (0, fade, 0)
                };

                let px = ci * FONT_WIDTH;
                let py = ri * FONT_HEIGHT;
                render_char(dt, px, py, ch as char, Color::new(r, g, b), Color::new(0, 0, 0));
            }
        }
    }

    // -----------------------------------------------------------------------
    // Bouncing logo renderer
    // -----------------------------------------------------------------------

    fn render_bouncing_logo(&mut self, dt: &mut FramebufferDrawTarget, fb_w: usize, fb_h: usize) {
        const LOGO_TEXT: &str = "ClaudioOS";
        let logo_pixel_w = LOGO_TEXT.len() * FONT_WIDTH * 3; // 3x scale
        let logo_pixel_h = FONT_HEIGHT * 3;

        fill_rect(dt, 0, 0, fb_w, fb_h, Color::new(0, 0, 0));

        // Move
        self.logo.x += self.logo.vx;
        self.logo.y += self.logo.vy;

        // Bounce off edges
        if self.logo.x <= 0 {
            self.logo.x = 0;
            self.logo.vx = self.logo.vx.abs();
            self.logo.color_idx += 1;
        }
        if self.logo.x + logo_pixel_w as i32 >= fb_w as i32 {
            self.logo.x = (fb_w - logo_pixel_w) as i32;
            self.logo.vx = -(self.logo.vx.abs());
            self.logo.color_idx += 1;
        }
        if self.logo.y <= 0 {
            self.logo.y = 0;
            self.logo.vy = self.logo.vy.abs();
            self.logo.color_idx += 1;
        }
        if self.logo.y + logo_pixel_h as i32 >= fb_h as i32 {
            self.logo.y = (fb_h - logo_pixel_h) as i32;
            self.logo.vy = -(self.logo.vy.abs());
            self.logo.color_idx += 1;
        }

        // Cycle through colours on bounce
        let colors = [
            Color::new(255, 0, 0),
            Color::new(0, 255, 0),
            Color::new(0, 128, 255),
            Color::new(255, 255, 0),
            Color::new(255, 0, 255),
            Color::new(0, 255, 255),
            Color::new(255, 128, 0),
            Color::new(255, 255, 255),
        ];
        let fg = colors[self.logo.color_idx % colors.len()];
        let bg = Color::new(0, 0, 0);

        // Render 3x scaled text by drawing each character 3x3 font cells
        let base_x = self.logo.x as usize;
        let base_y = self.logo.y as usize;
        for (i, ch) in LOGO_TEXT.chars().enumerate() {
            // Render the character at 3 positions in a 3x3 grid for "scaling"
            for sy in 0..3usize {
                for sx in 0..3usize {
                    let px = base_x + i * FONT_WIDTH * 3 + sx * FONT_WIDTH;
                    let py = base_y + sy * FONT_HEIGHT;
                    if px + FONT_WIDTH <= fb_w && py + FONT_HEIGHT <= fb_h {
                        render_char(dt, px, py, ch, fg, bg);
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Pipes renderer
    // -----------------------------------------------------------------------

    fn render_pipes(&mut self, dt: &mut FramebufferDrawTarget, fb_w: usize, fb_h: usize) {
        let cols = self.pipe_grid_cols;
        let rows = self.pipe_grid_rows;
        if cols == 0 || rows == 0 {
            return;
        }

        // On first frame, clear screen
        if self.frame == 0 {
            fill_rect(dt, 0, 0, fb_w, fb_h, Color::new(0, 0, 0));
        }

        // Advance each pipe by a few segments per frame
        let steps_per_frame = 3;
        for _ in 0..steps_per_frame {
            for pi in 0..self.pipes.len() {
                if !self.pipes[pi].active {
                    continue;
                }

                // Draw current position
                let col = self.pipes[pi].col;
                let row = self.pipes[pi].row;
                let color = self.pipes[pi].color;

                // Pick pipe character based on direction
                let ch = match self.pipes[pi].dir {
                    Dir::Up | Dir::Down => '|',
                    Dir::Left | Dir::Right => '-',
                };

                let px = col * FONT_WIDTH;
                let py = row * FONT_HEIGHT;
                if px + FONT_WIDTH <= fb_w && py + FONT_HEIGHT <= fb_h {
                    render_char(dt, px, py, ch, color, Color::new(0, 0, 0));
                }

                // Mark grid
                let idx = row * cols + col;
                if idx < self.pipe_grid.len() {
                    self.pipe_grid[idx] = 1;
                }

                self.pipes[pi].segments += 1;

                // Random chance to turn
                let turn = self.rng.next_bounded(6) == 0;
                if turn {
                    // Draw a corner character
                    let corner = '+';
                    if px + FONT_WIDTH <= fb_w && py + FONT_HEIGHT <= fb_h {
                        render_char(dt, px, py, corner, color, Color::new(0, 0, 0));
                    }

                    let new_dir = match self.pipes[pi].dir {
                        Dir::Up | Dir::Down => {
                            if self.rng.next_bounded(2) == 0 { Dir::Left } else { Dir::Right }
                        }
                        Dir::Left | Dir::Right => {
                            if self.rng.next_bounded(2) == 0 { Dir::Up } else { Dir::Down }
                        }
                    };
                    self.pipes[pi].dir = new_dir;
                }

                // Move
                match self.pipes[pi].dir {
                    Dir::Up => {
                        if self.pipes[pi].row == 0 {
                            self.pipes[pi].active = false;
                        } else {
                            self.pipes[pi].row -= 1;
                        }
                    }
                    Dir::Down => {
                        self.pipes[pi].row += 1;
                        if self.pipes[pi].row >= rows {
                            self.pipes[pi].active = false;
                        }
                    }
                    Dir::Left => {
                        if self.pipes[pi].col == 0 {
                            self.pipes[pi].active = false;
                        } else {
                            self.pipes[pi].col -= 1;
                        }
                    }
                    Dir::Right => {
                        self.pipes[pi].col += 1;
                        if self.pipes[pi].col >= cols {
                            self.pipes[pi].active = false;
                        }
                    }
                }

                // Max segments — deactivate
                if self.pipes[pi].segments >= MAX_PIPE_SEGMENTS {
                    self.pipes[pi].active = false;
                }
            }
        }

        // Respawn dead pipes
        let any_active = self.pipes.iter().any(|p| p.active);
        if !any_active {
            // All pipes dead — clear and restart
            self.pipe_grid = vec![0u8; cols * rows];
            self.pipes.clear();
            fill_rect(dt, 0, 0, fb_w, fb_h, Color::new(0, 0, 0));
            for _ in 0..3 {
                self.spawn_pipe(cols, rows);
            }
        } else {
            // Spawn new pipes occasionally
            let dead_count = self.pipes.iter().filter(|p| !p.active).count();
            if dead_count > 0 && self.rng.next_bounded(10) == 0 {
                // Remove dead pipes and spawn a new one
                self.pipes.retain(|p| p.active);
                self.spawn_pipe(cols, rows);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Clock renderer
    // -----------------------------------------------------------------------

    fn render_clock(&mut self, dt: &mut FramebufferDrawTarget, fb_w: usize, fb_h: usize) {
        fill_rect(dt, 0, 0, fb_w, fb_h, Color::new(0, 0, 0));

        let time_str = crate::rtc::wall_clock_formatted();
        let date_part: String;
        let time_part: String;

        // wall_clock_formatted returns "YYYY-MM-DD HH:MM:SS"
        if time_str.len() >= 19 {
            date_part = String::from(&time_str[..10]);
            time_part = String::from(&time_str[11..19]);
        } else {
            date_part = String::from("----.--.--");
            time_part = time_str.clone();
        }

        // Render time in large 5x scale (centered)
        let scale = 5usize;
        let time_chars = time_part.len();
        let time_pixel_w = time_chars * FONT_WIDTH * scale;
        let time_pixel_h = FONT_HEIGHT * scale;

        let time_x = if fb_w > time_pixel_w { (fb_w - time_pixel_w) / 2 } else { 0 };
        let time_y = if fb_h > time_pixel_h + FONT_HEIGHT * 3 {
            (fb_h - time_pixel_h) / 2 - FONT_HEIGHT
        } else {
            0
        };

        // Pulsing brightness based on frame
        let pulse = ((self.frame % 120) as i32 - 60).unsigned_abs() as u8;
        let brightness = 180u8.saturating_add(pulse.min(75));

        let fg = Color::new(0, brightness, brightness);
        let bg = Color::new(0, 0, 0);

        // Draw the time at scale by rendering each char into a scale x scale block of cells
        for (i, ch) in time_part.chars().enumerate() {
            for sy in 0..scale {
                for sx in 0..scale {
                    let px = time_x + i * FONT_WIDTH * scale + sx * FONT_WIDTH;
                    let py = time_y + sy * FONT_HEIGHT;
                    if px + FONT_WIDTH <= fb_w && py + FONT_HEIGHT <= fb_h {
                        render_char(dt, px, py, ch, fg, bg);
                    }
                }
            }
        }

        // Date below, smaller (2x scale)
        let date_scale = 2usize;
        let date_pixel_w = date_part.len() * FONT_WIDTH * date_scale;
        let date_x = if fb_w > date_pixel_w { (fb_w - date_pixel_w) / 2 } else { 0 };
        let date_y = time_y + time_pixel_h + FONT_HEIGHT;

        let date_fg = Color::new(100, 100, 100);
        for (i, ch) in date_part.chars().enumerate() {
            for sy in 0..date_scale {
                for sx in 0..date_scale {
                    let px = date_x + i * FONT_WIDTH * date_scale + sx * FONT_WIDTH;
                    let py = date_y + sy * FONT_HEIGHT;
                    if px + FONT_WIDTH <= fb_w && py + FONT_HEIGHT <= fb_h {
                        render_char(dt, px, py, ch, date_fg, bg);
                    }
                }
            }
        }

        // "ClaudioOS" at the bottom
        let label = "ClaudioOS";
        let label_w = label.len() * FONT_WIDTH;
        let label_x = if fb_w > label_w { (fb_w - label_w) / 2 } else { 0 };
        let label_y = date_y + date_scale * FONT_HEIGHT + FONT_HEIGHT * 2;
        let label_fg = Color::new(60, 60, 60);
        for (i, ch) in label.chars().enumerate() {
            let px = label_x + i * FONT_WIDTH;
            let py = label_y;
            if px + FONT_WIDTH <= fb_w && py + FONT_HEIGHT <= fb_h {
                render_char(dt, px, py, ch, label_fg, bg);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Shell command handler — called from dashboard when `screensaver` is typed
// ---------------------------------------------------------------------------

/// Handle the `screensaver` shell command. Returns the output string.
pub fn handle_command(state: &mut ScreensaverState, args: &str) -> String {
    let args = args.trim();

    if args.is_empty() || args == "help" {
        return String::from(
            "Usage: screensaver [mode|off|on|timeout <secs>|list]\r\n\
             Modes: starfield, matrix, bounce, pipes, clock\r\n\
             Examples:\r\n\
             \x1b[90m  screensaver starfield\x1b[0m  — preview starfield mode\r\n\
             \x1b[90m  screensaver off\x1b[0m        — disable screensaver\r\n\
             \x1b[90m  screensaver on\x1b[0m         — re-enable screensaver\r\n\
             \x1b[90m  screensaver timeout 60\x1b[0m — set idle timeout to 60s\r\n\
             \x1b[90m  screensaver list\x1b[0m       — list available modes\r\n\
             Press any key to exit the screensaver."
        );
    }

    if args == "off" {
        state.disabled = true;
        state.active = false;
        return String::from("\x1b[93mScreensaver disabled.\x1b[0m");
    }

    if args == "on" {
        state.disabled = false;
        state.last_input_tick = crate::interrupts::tick_count();
        return String::from("\x1b[92mScreensaver enabled.\x1b[0m");
    }

    if args == "list" {
        return String::from(
            "Available modes:\r\n\
             \x1b[96m  starfield\x1b[0m — 3D star field flying through space\r\n\
             \x1b[96m  matrix\x1b[0m    — falling green characters (Matrix rain)\r\n\
             \x1b[96m  bounce\x1b[0m    — ClaudioOS logo bouncing (DVD style)\r\n\
             \x1b[96m  pipes\x1b[0m     — randomly growing coloured pipes\r\n\
             \x1b[96m  clock\x1b[0m     — large digital clock"
        );
    }

    if let Some(rest) = args.strip_prefix("timeout ") {
        let rest = rest.trim();
        if let Ok(secs) = rest.parse::<u64>() {
            if secs == 0 {
                state.disabled = true;
                return String::from("\x1b[93mScreensaver disabled (timeout=0).\x1b[0m");
            }
            state.set_timeout(secs);
            return alloc::format!("\x1b[92mScreensaver timeout set to {}s.\x1b[0m", secs);
        } else {
            return alloc::format!("\x1b[31mInvalid timeout: '{}'. Use a number in seconds.\x1b[0m", rest);
        }
    }

    // Try to parse as a mode name — preview it
    if let Some(mode) = Mode::from_name(args) {
        state.activate_mode(mode);
        return alloc::format!("\x1b[92mScreensaver: {} (press any key to exit)\x1b[0m", mode.name());
    }

    alloc::format!("\x1b[31mUnknown screensaver command: '{}'. Type 'screensaver help'.\x1b[0m", args)
}
