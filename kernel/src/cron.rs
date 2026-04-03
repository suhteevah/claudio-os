//! Cron scheduler — periodic job execution based on cron expressions.
//!
//! Provides a cron scheduler checked every minute via the RTC wall clock.
//! Jobs are persisted in a simple config format and executed through the
//! shell executor.

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use alloc::string::ToString;
use core::fmt;
use spin::Mutex;

// ── Cron expression fields ───────────────────────────────────────────

/// A single cron field that can match specific values.
#[derive(Debug, Clone)]
pub enum CronField {
    /// Matches any value (`*`).
    Any,
    /// Matches a specific value (e.g. `5`).
    Value(u8),
    /// Matches every N-th value from 0 (e.g. `*/5`).
    Step(u8),
    /// Matches a list of values (e.g. `1,3,5`).
    List(Vec<u8>),
    /// Matches a range (e.g. `1-5`).
    Range(u8, u8),
}

impl CronField {
    /// Parse a single cron field string.
    pub fn parse(s: &str) -> Result<Self, &'static str> {
        let s = s.trim();
        if s == "*" {
            return Ok(CronField::Any);
        }
        // Step: */N
        if let Some(step) = s.strip_prefix("*/") {
            let n = step.parse::<u8>().map_err(|_| "invalid step value")?;
            if n == 0 {
                return Err("step cannot be zero");
            }
            return Ok(CronField::Step(n));
        }
        // Range: A-B
        if s.contains('-') {
            let parts: Vec<&str> = s.splitn(2, '-').collect();
            if parts.len() == 2 {
                let a = parts[0].parse::<u8>().map_err(|_| "invalid range start")?;
                let b = parts[1].parse::<u8>().map_err(|_| "invalid range end")?;
                return Ok(CronField::Range(a, b));
            }
        }
        // List: A,B,C
        if s.contains(',') {
            let vals: Result<Vec<u8>, _> = s.split(',').map(|v| v.trim().parse::<u8>()).collect();
            return Ok(CronField::List(vals.map_err(|_| "invalid list value")?));
        }
        // Single value
        let v = s.parse::<u8>().map_err(|_| "invalid cron field value")?;
        Ok(CronField::Value(v))
    }

    /// Check if this field matches the given value.
    pub fn matches(&self, value: u8) -> bool {
        match self {
            CronField::Any => true,
            CronField::Value(v) => *v == value,
            CronField::Step(step) => value % step == 0,
            CronField::List(vals) => vals.contains(&value),
            CronField::Range(a, b) => value >= *a && value <= *b,
        }
    }
}

// ── Cron expression (5 fields) ───────────────────────────────────────

/// A parsed 5-field cron expression: minute hour day month weekday.
#[derive(Debug, Clone)]
pub struct CronExpression {
    pub minute: CronField,
    pub hour: CronField,
    pub day: CronField,
    pub month: CronField,
    pub weekday: CronField,
}

impl CronExpression {
    /// Parse a standard 5-field cron expression string.
    ///
    /// Format: `minute hour day month weekday`
    ///
    /// Examples:
    /// - `*/5 * * * *` — every 5 minutes
    /// - `0 3 * * *`   — at 3:00 AM daily
    /// - `30 8 * * 1-5` — 8:30 AM weekdays
    pub fn parse(expr: &str) -> Result<Self, &'static str> {
        let fields: Vec<&str> = expr.split_whitespace().collect();
        if fields.len() != 5 {
            return Err("cron expression must have exactly 5 fields");
        }
        Ok(CronExpression {
            minute: CronField::parse(fields[0])?,
            hour: CronField::parse(fields[1])?,
            day: CronField::parse(fields[2])?,
            month: CronField::parse(fields[3])?,
            weekday: CronField::parse(fields[4])?,
        })
    }

    /// Check if this expression matches the given date/time.
    /// `weekday` is 0=Sunday, 1=Monday, ..., 6=Saturday.
    pub fn matches(&self, minute: u8, hour: u8, day: u8, month: u8, weekday: u8) -> bool {
        self.minute.matches(minute)
            && self.hour.matches(hour)
            && self.day.matches(day)
            && self.month.matches(month)
            && self.weekday.matches(weekday)
    }
}

impl fmt::Display for CronExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fn field_str(field: &CronField) -> String {
            match field {
                CronField::Any => "*".to_string(),
                CronField::Value(v) => format!("{}", v),
                CronField::Step(s) => format!("*/{}", s),
                CronField::List(vals) => {
                    let parts: Vec<String> = vals.iter().map(|v| format!("{}", v)).collect();
                    parts.join(",")
                }
                CronField::Range(a, b) => format!("{}-{}", a, b),
            }
        }
        write!(
            f,
            "{} {} {} {} {}",
            field_str(&self.minute),
            field_str(&self.hour),
            field_str(&self.day),
            field_str(&self.month),
            field_str(&self.weekday),
        )
    }
}

// ── Cron job ─────────────────────────────────────────────────────────

/// A scheduled cron job.
#[derive(Debug, Clone)]
pub struct CronJob {
    /// Unique job ID (monotonically increasing).
    pub id: u32,
    /// Cron schedule expression.
    pub schedule: CronExpression,
    /// Shell command to execute.
    pub command: String,
    /// Whether this job is currently enabled.
    pub enabled: bool,
}

impl fmt::Display for CronJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] {} {} {}",
            self.id,
            self.schedule,
            self.command,
            if self.enabled { "(enabled)" } else { "(disabled)" },
        )
    }
}

// ── Cron scheduler ───────────────────────────────────────────────────

/// Global cron scheduler.
pub static SCHEDULER: Mutex<CronScheduler> = Mutex::new(CronScheduler::new());

/// The cron scheduler — holds all registered jobs and checks them each minute.
pub struct CronScheduler {
    jobs: Vec<CronJob>,
    next_id: u32,
    /// Last (minute, hour, day) we checked — avoids running jobs twice in the
    /// same minute.
    last_check: (u8, u8, u8),
}

impl CronScheduler {
    pub const fn new() -> Self {
        Self {
            jobs: Vec::new(),
            next_id: 1,
            last_check: (255, 255, 255), // impossible initial value
        }
    }

    /// Add a new cron job. Returns the assigned job ID.
    pub fn add_job(&mut self, schedule: CronExpression, command: String) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.jobs.push(CronJob {
            id,
            schedule,
            command,
            enabled: true,
        });
        log::info!("[cron] added job {}: {}", id, self.jobs.last().unwrap());
        id
    }

    /// Remove a job by ID. Returns true if found and removed.
    pub fn remove_job(&mut self, id: u32) -> bool {
        let len_before = self.jobs.len();
        self.jobs.retain(|j| j.id != id);
        let removed = self.jobs.len() < len_before;
        if removed {
            log::info!("[cron] removed job {}", id);
        }
        removed
    }

    /// Enable or disable a job by ID.
    pub fn set_enabled(&mut self, id: u32, enabled: bool) {
        if let Some(job) = self.jobs.iter_mut().find(|j| j.id == id) {
            job.enabled = enabled;
            log::info!("[cron] job {} {}", id, if enabled { "enabled" } else { "disabled" });
        }
    }

    /// List all registered jobs.
    pub fn list_jobs(&self) -> &[CronJob] {
        &self.jobs
    }

    /// Remove all jobs.
    pub fn clear(&mut self) {
        self.jobs.clear();
        log::info!("[cron] all jobs cleared");
    }

    /// Check the current time and return commands that should execute now.
    ///
    /// Call this once per minute (e.g. from a timer interrupt or executor tick).
    /// It de-duplicates so that calling multiple times in the same minute is safe.
    pub fn tick(&mut self) -> Vec<String> {
        let dt = crate::rtc::wall_clock();
        let key = (dt.minute, dt.hour, dt.day);

        // Already checked this minute
        if key == self.last_check {
            return Vec::new();
        }
        self.last_check = key;

        let weekday = day_of_week(dt.year, dt.month, dt.day);

        let mut commands = Vec::new();
        for job in &self.jobs {
            if job.enabled && job.schedule.matches(dt.minute, dt.hour, dt.day, dt.month, weekday) {
                log::info!("[cron] firing job {}: {}", job.id, job.command);
                commands.push(job.command.clone());
            }
        }
        commands
    }
}

/// Compute the day of week for a given date using Zeller-like formula.
/// Returns 0=Sunday, 1=Monday, ..., 6=Saturday.
fn day_of_week(year: u16, month: u8, day: u8) -> u8 {
    // Tomohiko Sakamoto's algorithm
    let t = [0i32, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let mut y = year as i32;
    if month < 3 {
        y -= 1;
    }
    let dow = (y + y / 4 - y / 100 + y / 400 + t[(month - 1) as usize] + day as i32) % 7;
    dow as u8
}

// ── Shell command handlers ───────────────────────────────────────────

/// Execute the `crontab` shell command.
///
/// Usage:
/// - `crontab -l`              — list all jobs
/// - `crontab -e <expr> <cmd>` — add a new job (e.g. `crontab -e "*/5 * * * *" echo hello`)
/// - `crontab -r <id>`         — remove a job by ID
/// - `crontab -r all`          — remove all jobs
pub fn shell_crontab(args: &str) -> String {
    let args = args.trim();

    if args == "-l" || args.is_empty() {
        let sched = SCHEDULER.lock();
        let jobs = sched.list_jobs();
        if jobs.is_empty() {
            return "no cron jobs configured\n".to_string();
        }
        let mut out = String::new();
        for job in jobs {
            out.push_str(&format!("{}\n", job));
        }
        return out;
    }

    if let Some(rest) = args.strip_prefix("-r") {
        let rest = rest.trim();
        if rest == "all" {
            SCHEDULER.lock().clear();
            return "all cron jobs removed\n".to_string();
        }
        match rest.parse::<u32>() {
            Ok(id) => {
                if SCHEDULER.lock().remove_job(id) {
                    format!("removed job {}\n", id)
                } else {
                    format!("job {} not found\n", id)
                }
            }
            Err(_) => "usage: crontab -r <id|all>\n".to_string(),
        }
    } else if let Some(rest) = args.strip_prefix("-e") {
        let rest = rest.trim();
        // The cron expression is the first 5 space-separated tokens, rest is command
        let tokens: Vec<&str> = rest.splitn(6, ' ').collect();
        if tokens.len() < 6 {
            return "usage: crontab -e <min> <hour> <day> <month> <wday> <command>\n".to_string();
        }
        let expr_str = format!("{} {} {} {} {}", tokens[0], tokens[1], tokens[2], tokens[3], tokens[4]);
        match CronExpression::parse(&expr_str) {
            Ok(expr) => {
                let command = tokens[5].to_string();
                let id = SCHEDULER.lock().add_job(expr, command);
                format!("added job {}\n", id)
            }
            Err(e) => format!("invalid cron expression: {}\n", e),
        }
    } else {
        "usage: crontab [-l | -e <schedule> <cmd> | -r <id|all>]\n".to_string()
    }
}

/// Execute all pending cron jobs for the current minute.
/// Call this from the executor's main loop once per tick.
pub fn execute_pending() {
    let commands = SCHEDULER.lock().tick();
    for cmd in commands {
        log::info!("[cron] executing: {}", cmd);
        // TODO: Wire into shell executor once integrated.
        // For now, log the command. The shell crate will be called when
        // the integration is complete.
        let _ = cmd;
    }
}

// ── Persistence ──────────────────────────────────────────────────────

/// Serialize all cron jobs to a config string for persistence.
pub fn serialize_jobs() -> String {
    let sched = SCHEDULER.lock();
    let mut out = String::new();
    for job in sched.list_jobs() {
        // Format: enabled|schedule|command
        out.push_str(&format!(
            "{}|{}|{}\n",
            if job.enabled { '1' } else { '0' },
            job.schedule,
            job.command,
        ));
    }
    out
}

/// Load cron jobs from a serialized config string.
pub fn deserialize_jobs(data: &str) {
    let mut sched = SCHEDULER.lock();
    sched.clear();
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.splitn(3, '|').collect();
        if parts.len() != 3 {
            log::warn!("[cron] skipping malformed config line: {}", line);
            continue;
        }
        let enabled = parts[0] == "1";
        match CronExpression::parse(parts[1]) {
            Ok(expr) => {
                let id = sched.add_job(expr, parts[2].to_string());
                if !enabled {
                    sched.set_enabled(id, false);
                }
            }
            Err(e) => {
                log::warn!("[cron] skipping job with bad schedule '{}': {}", parts[1], e);
            }
        }
    }
}

/// Initialize the cron subsystem. Call once at boot after RTC init.
pub fn init() {
    log::info!("[cron] scheduler initialized");
    // TODO: Load persisted jobs from FAT32 config via fs-persist
}
