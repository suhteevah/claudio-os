//! Packet filter firewall for ClaudioOS.
//!
//! Provides stateful packet filtering with ordered rules, connection tracking,
//! and rate limiting. Integrates with smoltcp at the device wrapper level.
//!
//! Shell commands:
//! - `fw list` — show all rules
//! - `fw add allow|deny|log <proto> <src> <dst> <dport>` — add a rule
//! - `fw delete <n>` — delete rule by number
//! - `fw default allow|deny` — set default policy
//! - `fw flush` — remove all rules
//! - `fw stats` — show connection tracking and rate limit stats

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Packet direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
    Any,
}

impl Direction {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "in" => Some(Direction::In),
            "out" => Some(Direction::Out),
            "any" => Some(Direction::Any),
            _ => None,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Direction::In => "in",
            Direction::Out => "out",
            Direction::Any => "any",
        }
    }
}

/// Protocol filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Tcp,
    Udp,
    Icmp,
    Any,
}

impl Protocol {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "tcp" => Some(Protocol::Tcp),
            "udp" => Some(Protocol::Udp),
            "icmp" => Some(Protocol::Icmp),
            "any" => Some(Protocol::Any),
            _ => None,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Protocol::Tcp => "tcp",
            Protocol::Udp => "udp",
            Protocol::Icmp => "icmp",
            Protocol::Any => "any",
        }
    }
}

/// Firewall action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Allow,
    Deny,
    Log,
}

impl Action {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "allow" => Some(Action::Allow),
            "deny" => Some(Action::Deny),
            "log" => Some(Action::Log),
            _ => None,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Action::Allow => "allow",
            Action::Deny => "deny",
            Action::Log => "log",
        }
    }
}

/// An IPv4 address with optional CIDR prefix for matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpMatch {
    /// IPv4 address as 4 octets.
    pub addr: [u8; 4],
    /// CIDR prefix length (0-32). 0 means match any.
    pub prefix_len: u8,
}

impl IpMatch {
    /// Match any address (0.0.0.0/0).
    pub const ANY: Self = Self {
        addr: [0, 0, 0, 0],
        prefix_len: 0,
    };

    /// Parse "a.b.c.d" or "a.b.c.d/n" or "any".
    pub fn parse(s: &str) -> Option<Self> {
        if s == "any" || s == "0.0.0.0/0" {
            return Some(Self::ANY);
        }

        let (ip_part, prefix) = if let Some(idx) = s.find('/') {
            let prefix: u8 = s[idx + 1..].parse().ok()?;
            if prefix > 32 {
                return None;
            }
            (&s[..idx], prefix)
        } else {
            (s, 32)
        };

        let mut octets = [0u8; 4];
        let parts: Vec<&str> = ip_part.split('.').collect();
        if parts.len() != 4 {
            return None;
        }
        for (i, p) in parts.iter().enumerate() {
            octets[i] = p.parse().ok()?;
        }

        Some(Self {
            addr: octets,
            prefix_len: prefix,
        })
    }

    /// Check if a given IPv4 address matches this rule.
    pub fn matches(&self, addr: [u8; 4]) -> bool {
        if self.prefix_len == 0 {
            return true; // 0.0.0.0/0 matches everything
        }
        if self.prefix_len == 32 {
            return self.addr == addr;
        }
        let rule_u32 = u32::from_be_bytes(self.addr);
        let addr_u32 = u32::from_be_bytes(addr);
        let mask = !((1u32 << (32 - self.prefix_len)) - 1);
        (rule_u32 & mask) == (addr_u32 & mask)
    }

    fn format(&self) -> String {
        if self.prefix_len == 0 {
            String::from("any")
        } else if self.prefix_len == 32 {
            format!(
                "{}.{}.{}.{}",
                self.addr[0], self.addr[1], self.addr[2], self.addr[3]
            )
        } else {
            format!(
                "{}.{}.{}.{}/{}",
                self.addr[0], self.addr[1], self.addr[2], self.addr[3], self.prefix_len
            )
        }
    }
}

/// A single firewall rule.
#[derive(Debug, Clone)]
pub struct FirewallRule {
    pub direction: Direction,
    pub protocol: Protocol,
    pub src_ip: IpMatch,
    pub dst_ip: IpMatch,
    pub src_port: u16, // 0 = any
    pub dst_port: u16, // 0 = any
    pub action: Action,
}

impl FirewallRule {
    fn format_port(p: u16) -> String {
        if p == 0 {
            String::from("*")
        } else {
            format!("{}", p)
        }
    }
}

// ---------------------------------------------------------------------------
// Connection tracking (stateful firewall)
// ---------------------------------------------------------------------------

/// A tracked connection (simplified 5-tuple).
#[derive(Debug, Clone)]
struct ConnTrack {
    protocol: Protocol,
    src_ip: [u8; 4],
    dst_ip: [u8; 4],
    src_port: u16,
    dst_port: u16,
    /// Timestamp (PIT ticks) when this entry was created.
    created_ticks: u64,
    /// Timestamp of last packet seen on this connection.
    last_seen_ticks: u64,
}

impl ConnTrack {
    /// Connection timeout: ~120 seconds at 18.2 Hz PIT.
    const TIMEOUT_TICKS: u64 = 120 * 18;

    fn is_expired(&self, now_ticks: u64) -> bool {
        now_ticks.saturating_sub(self.last_seen_ticks) > Self::TIMEOUT_TICKS
    }

    /// Check if a packet is return traffic for this connection.
    fn is_return_traffic(
        &self,
        proto: Protocol,
        src: [u8; 4],
        dst: [u8; 4],
        sport: u16,
        dport: u16,
    ) -> bool {
        self.protocol == proto
            && self.src_ip == dst
            && self.dst_ip == src
            && self.src_port == dport
            && self.dst_port == sport
    }
}

// ---------------------------------------------------------------------------
// Rate limiting
// ---------------------------------------------------------------------------

/// Per-IP rate limit tracker.
#[derive(Debug, Clone)]
struct RateLimitEntry {
    ip: [u8; 4],
    /// Ring buffer of connection timestamps (PIT ticks).
    timestamps: [u64; 32],
    /// Next write index.
    idx: usize,
    /// Total connections tracked in current window.
    count: usize,
}

impl RateLimitEntry {
    fn new(ip: [u8; 4], now_ticks: u64) -> Self {
        let mut timestamps = [0u64; 32];
        timestamps[0] = now_ticks;
        Self {
            ip,
            timestamps,
            idx: 1,
            count: 1,
        }
    }

    /// Record a new connection. Returns the number of connections in the last
    /// `window_ticks` interval.
    fn record(&mut self, now_ticks: u64, window_ticks: u64) -> usize {
        self.timestamps[self.idx % 32] = now_ticks;
        self.idx = (self.idx + 1) % 32;
        if self.count < 32 {
            self.count += 1;
        }
        // Count entries within the window.
        let mut in_window = 0usize;
        for i in 0..self.count {
            if now_ticks.saturating_sub(self.timestamps[i]) <= window_ticks {
                in_window += 1;
            }
        }
        in_window
    }
}

// ---------------------------------------------------------------------------
// RuleSet — the complete firewall state
// ---------------------------------------------------------------------------

/// The firewall rule set with connection tracking and rate limiting.
pub struct RuleSet {
    /// Ordered list of rules. First match wins.
    pub rules: Vec<FirewallRule>,
    /// Default policy when no rule matches.
    pub default_policy: Action,
    /// Connection tracking table.
    conn_table: Vec<ConnTrack>,
    /// Rate limit entries (per source IP).
    rate_limits: Vec<RateLimitEntry>,
    /// Max new connections per second from a single IP (0 = unlimited).
    pub rate_limit_per_sec: u16,
    /// Counters.
    pub packets_allowed: u64,
    pub packets_denied: u64,
    pub packets_logged: u64,
}

impl RuleSet {
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            default_policy: Action::Allow,
            conn_table: Vec::new(),
            rate_limits: Vec::new(),
            rate_limit_per_sec: 0, // unlimited
            packets_allowed: 0,
            packets_denied: 0,
            packets_logged: 0,
        }
    }

    /// Add a rule to the end of the rule list.
    pub fn add_rule(&mut self, rule: FirewallRule) {
        self.rules.push(rule);
    }

    /// Delete a rule by 1-based index.
    pub fn delete_rule(&mut self, index: usize) -> bool {
        if index == 0 || index > self.rules.len() {
            return false;
        }
        self.rules.remove(index - 1);
        true
    }

    /// Remove all rules.
    pub fn flush(&mut self) {
        self.rules.clear();
        self.conn_table.clear();
        self.rate_limits.clear();
        self.packets_allowed = 0;
        self.packets_denied = 0;
        self.packets_logged = 0;
    }

    /// Expire old connection tracking entries.
    fn expire_connections(&mut self, now_ticks: u64) {
        self.conn_table.retain(|c| !c.is_expired(now_ticks));
    }

    /// Record an outbound connection for stateful tracking.
    fn track_connection(
        &mut self,
        proto: Protocol,
        src: [u8; 4],
        dst: [u8; 4],
        sport: u16,
        dport: u16,
        now_ticks: u64,
    ) {
        // Check if already tracked.
        for c in &mut self.conn_table {
            if c.protocol == proto
                && c.src_ip == src
                && c.dst_ip == dst
                && c.src_port == sport
                && c.dst_port == dport
            {
                c.last_seen_ticks = now_ticks;
                return;
            }
        }
        // Limit table size.
        if self.conn_table.len() >= 1024 {
            self.expire_connections(now_ticks);
            if self.conn_table.len() >= 1024 {
                self.conn_table.remove(0); // evict oldest
            }
        }
        self.conn_table.push(ConnTrack {
            protocol: proto,
            src_ip: src,
            dst_ip: dst,
            src_port: sport,
            dst_port: dport,
            created_ticks: now_ticks,
            last_seen_ticks: now_ticks,
        });
    }

    /// Check if this packet is return traffic for an established connection.
    fn is_established(
        &mut self,
        proto: Protocol,
        src: [u8; 4],
        dst: [u8; 4],
        sport: u16,
        dport: u16,
        now_ticks: u64,
    ) -> bool {
        for c in &mut self.conn_table {
            if c.is_return_traffic(proto, src, dst, sport, dport) && !c.is_expired(now_ticks) {
                c.last_seen_ticks = now_ticks;
                return true;
            }
        }
        false
    }

    /// Check rate limit for a source IP. Returns true if the packet should be
    /// rate-limited (denied).
    fn check_rate_limit(&mut self, src: [u8; 4], now_ticks: u64) -> bool {
        if self.rate_limit_per_sec == 0 {
            return false; // rate limiting disabled
        }
        let window_ticks = 18u64; // ~1 second at 18.2 Hz PIT

        for entry in &mut self.rate_limits {
            if entry.ip == src {
                let count = entry.record(now_ticks, window_ticks);
                return count > self.rate_limit_per_sec as usize;
            }
        }

        // New IP — add entry.
        if self.rate_limits.len() >= 256 {
            self.rate_limits.remove(0); // evict oldest
        }
        self.rate_limits.push(RateLimitEntry::new(src, now_ticks));
        false
    }

    /// Main packet check. Returns the action to take.
    ///
    /// `now_ticks` is the current PIT tick count for connection timeout tracking.
    pub fn check_packet(
        &mut self,
        direction: Direction,
        protocol: Protocol,
        src: [u8; 4],
        dst: [u8; 4],
        sport: u16,
        dport: u16,
        now_ticks: u64,
    ) -> Action {
        // Periodic cleanup.
        if now_ticks % 182 == 0 {
            self.expire_connections(now_ticks);
        }

        // Stateful: allow return traffic for established connections.
        if direction == Direction::In {
            if self.is_established(protocol, src, dst, sport, dport, now_ticks) {
                self.packets_allowed += 1;
                return Action::Allow;
            }
        }

        // Rate limiting on inbound connections.
        if direction == Direction::In && self.check_rate_limit(src, now_ticks) {
            log::warn!(
                "[firewall] rate limit exceeded for {}.{}.{}.{}",
                src[0],
                src[1],
                src[2],
                src[3]
            );
            self.packets_denied += 1;
            return Action::Deny;
        }

        // Walk rules in order — first match wins.
        for rule in &self.rules {
            // Direction match.
            if rule.direction != Direction::Any && rule.direction != direction {
                continue;
            }
            // Protocol match.
            if rule.protocol != Protocol::Any && rule.protocol != protocol {
                continue;
            }
            // Source IP match.
            if !rule.src_ip.matches(src) {
                continue;
            }
            // Destination IP match.
            if !rule.dst_ip.matches(dst) {
                continue;
            }
            // Source port match.
            if rule.src_port != 0 && rule.src_port != sport {
                continue;
            }
            // Destination port match.
            if rule.dst_port != 0 && rule.dst_port != dport {
                continue;
            }

            // Match found.
            match rule.action {
                Action::Allow => {
                    self.packets_allowed += 1;
                    // Track outbound connections for stateful return traffic.
                    if direction == Direction::Out {
                        self.track_connection(protocol, src, dst, sport, dport, now_ticks);
                    }
                    return Action::Allow;
                }
                Action::Deny => {
                    self.packets_denied += 1;
                    return Action::Deny;
                }
                Action::Log => {
                    self.packets_logged += 1;
                    log::info!(
                        "[firewall] LOG: {} {} {}.{}.{}.{}:{} -> {}.{}.{}.{}:{}",
                        direction.as_str(),
                        protocol.as_str(),
                        src[0], src[1], src[2], src[3], sport,
                        dst[0], dst[1], dst[2], dst[3], dport,
                    );
                    // Log action continues to next rule (or default policy).
                    continue;
                }
            }
        }

        // No rule matched — apply default policy.
        match self.default_policy {
            Action::Allow => {
                self.packets_allowed += 1;
                if direction == Direction::Out {
                    self.track_connection(protocol, src, dst, sport, dport, now_ticks);
                }
                Action::Allow
            }
            Action::Deny => {
                self.packets_denied += 1;
                Action::Deny
            }
            Action::Log => {
                self.packets_logged += 1;
                Action::Allow // log + allow for default
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Global firewall instance
// ---------------------------------------------------------------------------

/// Global firewall rule set, protected by a spin mutex.
pub static FIREWALL: Mutex<RuleSet> = Mutex::new(RuleSet {
    rules: Vec::new(),
    default_policy: Action::Allow,
    conn_table: Vec::new(),
    rate_limits: Vec::new(),
    rate_limit_per_sec: 0,
    packets_allowed: 0,
    packets_denied: 0,
    packets_logged: 0,
});

// ---------------------------------------------------------------------------
// Shell command handler
// ---------------------------------------------------------------------------

/// Handle `fw` shell commands. Returns the output string.
pub fn handle_command(args: &str) -> String {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        return String::from(
            "Usage: fw <list|add|delete|default|flush|stats|ratelimit>\n\
             \n\
             Commands:\n\
             \x20 fw list                           Show all rules\n\
             \x20 fw add <action> <dir> <proto> <src> <dst> <dport>  Add rule\n\
             \x20 fw delete <rule_number>            Delete rule by number\n\
             \x20 fw default allow|deny              Set default policy\n\
             \x20 fw flush                           Remove all rules\n\
             \x20 fw stats                           Show firewall statistics\n\
             \x20 fw ratelimit <N>                   Max N new conn/sec per IP (0=off)\n"
        );
    }

    match parts[0] {
        "list" => cmd_list(),
        "add" => cmd_add(&parts[1..]),
        "delete" => cmd_delete(&parts[1..]),
        "default" => cmd_default(&parts[1..]),
        "flush" => cmd_flush(),
        "stats" => cmd_stats(),
        "ratelimit" => cmd_ratelimit(&parts[1..]),
        _ => format!("fw: unknown subcommand '{}'. Try 'fw' for usage.", parts[0]),
    }
}

fn cmd_list() -> String {
    let fw = FIREWALL.lock();
    if fw.rules.is_empty() {
        return format!(
            "No firewall rules configured. Default policy: {}\n",
            fw.default_policy.as_str()
        );
    }

    let mut out = String::new();
    out.push_str(&format!(
        "Default policy: {}\n",
        fw.default_policy.as_str()
    ));
    out.push_str(&format!(
        "{:<4} {:<6} {:<5} {:<5} {:<18} {:<18} {:<6} {:<6}\n",
        "#", "Action", "Dir", "Proto", "Source", "Dest", "SPort", "DPort"
    ));
    out.push_str(&"-".repeat(68));
    out.push('\n');

    for (i, rule) in fw.rules.iter().enumerate() {
        out.push_str(&format!(
            "{:<4} {:<6} {:<5} {:<5} {:<18} {:<18} {:<6} {:<6}\n",
            i + 1,
            rule.action.as_str(),
            rule.direction.as_str(),
            rule.protocol.as_str(),
            rule.src_ip.format(),
            rule.dst_ip.format(),
            FirewallRule::format_port(rule.src_port),
            FirewallRule::format_port(rule.dst_port),
        ));
    }

    out
}

fn cmd_add(args: &[&str]) -> String {
    // fw add <action> <direction> <proto> <src> <dst> <dport>
    // Minimum: fw add allow in tcp any any 22
    if args.len() < 6 {
        return String::from(
            "Usage: fw add <action> <dir> <proto> <src_ip> <dst_ip> <dst_port>\n\
             \n\
             Actions: allow, deny, log\n\
             Directions: in, out, any\n\
             Protocols: tcp, udp, icmp, any\n\
             IPs: a.b.c.d, a.b.c.d/N, any\n\
             Ports: number or 0 for any\n\
             \n\
             Example: fw add allow in tcp any 0.0.0.0/0 22\n"
        );
    }

    let action = match Action::from_str(args[0]) {
        Some(a) => a,
        None => return format!("fw add: invalid action '{}' (allow/deny/log)", args[0]),
    };

    let direction = match Direction::from_str(args[1]) {
        Some(d) => d,
        None => return format!("fw add: invalid direction '{}' (in/out/any)", args[1]),
    };

    let protocol = match Protocol::from_str(args[2]) {
        Some(p) => p,
        None => return format!("fw add: invalid protocol '{}' (tcp/udp/icmp/any)", args[2]),
    };

    let src_ip = match IpMatch::parse(args[3]) {
        Some(ip) => ip,
        None => return format!("fw add: invalid source IP '{}'", args[3]),
    };

    let dst_ip = match IpMatch::parse(args[4]) {
        Some(ip) => ip,
        None => return format!("fw add: invalid destination IP '{}'", args[4]),
    };

    let dst_port: u16 = match args[5].parse() {
        Ok(p) => p,
        Err(_) => return format!("fw add: invalid port '{}'", args[5]),
    };

    let rule = FirewallRule {
        direction,
        protocol,
        src_ip,
        dst_ip,
        src_port: 0,
        dst_port,
        action,
    };

    let mut fw = FIREWALL.lock();
    let idx = fw.rules.len() + 1;
    fw.add_rule(rule);

    format!("Rule #{} added.\n", idx)
}

fn cmd_delete(args: &[&str]) -> String {
    if args.is_empty() {
        return String::from("Usage: fw delete <rule_number>\n");
    }
    let idx: usize = match args[0].parse() {
        Ok(n) => n,
        Err(_) => return format!("fw delete: invalid number '{}'", args[0]),
    };
    let mut fw = FIREWALL.lock();
    if fw.delete_rule(idx) {
        format!("Rule #{} deleted.\n", idx)
    } else {
        format!("fw delete: rule #{} does not exist.\n", idx)
    }
}

fn cmd_default(args: &[&str]) -> String {
    if args.is_empty() {
        return String::from("Usage: fw default allow|deny\n");
    }
    match args[0] {
        "allow" => {
            FIREWALL.lock().default_policy = Action::Allow;
            String::from("Default policy set to ALLOW.\n")
        }
        "deny" => {
            FIREWALL.lock().default_policy = Action::Deny;
            String::from("Default policy set to DENY.\n")
        }
        _ => format!("fw default: invalid policy '{}' (allow/deny)", args[0]),
    }
}

fn cmd_flush() -> String {
    FIREWALL.lock().flush();
    String::from("All firewall rules flushed.\n")
}

fn cmd_stats() -> String {
    let fw = FIREWALL.lock();
    let mut out = String::new();
    out.push_str(&format!("Default policy: {}\n", fw.default_policy.as_str()));
    out.push_str(&format!("Rules:          {}\n", fw.rules.len()));
    out.push_str(&format!("Connections:    {} tracked\n", fw.conn_table.len()));
    out.push_str(&format!("Rate limit:     {} conn/sec/IP", fw.rate_limit_per_sec));
    if fw.rate_limit_per_sec == 0 {
        out.push_str(" (disabled)");
    }
    out.push('\n');
    out.push_str(&format!("Packets allowed: {}\n", fw.packets_allowed));
    out.push_str(&format!("Packets denied:  {}\n", fw.packets_denied));
    out.push_str(&format!("Packets logged:  {}\n", fw.packets_logged));
    out
}

fn cmd_ratelimit(args: &[&str]) -> String {
    if args.is_empty() {
        let current = FIREWALL.lock().rate_limit_per_sec;
        return format!(
            "Current rate limit: {} conn/sec/IP{}\nUsage: fw ratelimit <N>  (0 to disable)\n",
            current,
            if current == 0 { " (disabled)" } else { "" }
        );
    }
    let n: u16 = match args[0].parse() {
        Ok(v) => v,
        Err(_) => return format!("fw ratelimit: invalid number '{}'", args[0]),
    };
    FIREWALL.lock().rate_limit_per_sec = n;
    if n == 0 {
        String::from("Rate limiting disabled.\n")
    } else {
        format!("Rate limit set to {} connections/second per IP.\n", n)
    }
}
