//! Built-in man pages system for ClaudioOS.
//!
//! Provides formatted help documentation for all shell commands, similar to
//! Unix `man` pages. Supports section display, keyword search (`man -k`),
//! and paged output.
//!
//! Shell commands:
//! - `man <command>` — display the manual page for a command
//! - `man -k <keyword>` — search all man pages for a keyword
//! - `man man` — how to use the man command itself
//! - `help` — enhanced to list available man pages

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

// ---------------------------------------------------------------------------
// ManPage structure
// ---------------------------------------------------------------------------

/// A single manual page entry.
pub struct ManPage {
    /// Command name.
    pub name: &'static str,
    /// Section number (1 = user commands, 8 = admin).
    pub section: u8,
    /// One-line synopsis.
    pub synopsis: &'static str,
    /// Full description.
    pub description: &'static str,
    /// Usage examples.
    pub examples: &'static str,
    /// See also references.
    pub see_also: &'static str,
}

// ---------------------------------------------------------------------------
// Built-in man page database
// ---------------------------------------------------------------------------

/// All built-in man pages. Sorted alphabetically by name.
static MAN_PAGES: &[ManPage] = &[
    ManPage {
        name: "cat",
        section: 1,
        synopsis: "cat <file> [file2 ...]",
        description: "Concatenate and display file contents.\n\n\
            Reads one or more files and writes their contents to the terminal.\n\
            If no files are specified, reads from standard input (pipe).",
        examples: "  cat /etc/config\n  cat file1.txt file2.txt\n  echo hello | cat",
        see_also: "head, tail, grep",
    },
    ManPage {
        name: "cd",
        section: 1,
        synopsis: "cd [directory]",
        description: "Change the current working directory.\n\n\
            With no arguments, changes to the home directory (/).\n\
            Supports relative and absolute paths.",
        examples: "  cd /home\n  cd ..\n  cd",
        see_also: "pwd, ls",
    },
    ManPage {
        name: "clear",
        section: 1,
        synopsis: "clear",
        description: "Clear the terminal screen.\n\n\
            Erases all text from the current terminal pane and moves\n\
            the cursor to the top-left corner.",
        examples: "  clear",
        see_also: "help",
    },
    ManPage {
        name: "cp",
        section: 1,
        synopsis: "cp <source> <destination>",
        description: "Copy files.\n\n\
            Copies the source file to the destination path. If the\n\
            destination is a directory, the file is copied into it\n\
            with the same name.",
        examples: "  cp file.txt backup.txt\n  cp /etc/config /tmp/",
        see_also: "mv, rm",
    },
    ManPage {
        name: "date",
        section: 1,
        synopsis: "date",
        description: "Display the current date and time.\n\n\
            Shows the system date and time from the hardware RTC\n\
            (Real-Time Clock). Time is in UTC.",
        examples: "  date",
        see_also: "uptime",
    },
    ManPage {
        name: "df",
        section: 1,
        synopsis: "df",
        description: "Display filesystem disk space usage.\n\n\
            Shows mounted filesystems with their total size, used space,\n\
            available space, and mount point.",
        examples: "  df",
        see_also: "free, mount",
    },
    ManPage {
        name: "echo",
        section: 1,
        synopsis: "echo [text ...]",
        description: "Display text.\n\n\
            Writes its arguments to standard output, separated by spaces,\n\
            followed by a newline. Supports environment variable expansion\n\
            with $VAR syntax.",
        examples: "  echo hello world\n  echo $HOME\n  echo \"quoted string\"",
        see_also: "set, export",
    },
    ManPage {
        name: "exit",
        section: 1,
        synopsis: "exit",
        description: "Exit the current shell session.\n\n\
            Closes the current shell pane. If this is the last pane,\n\
            the dashboard remains active.",
        examples: "  exit",
        see_also: "shutdown, reboot",
    },
    ManPage {
        name: "export",
        section: 1,
        synopsis: "export <NAME>=<value>",
        description: "Set an environment variable.\n\n\
            Creates or updates an environment variable that persists\n\
            for the current shell session. Variables are available\n\
            for command expansion with $NAME syntax.",
        examples: "  export PATH=/bin:/usr/bin\n  export EDITOR=nano",
        see_also: "set, unset, env",
    },
    ManPage {
        name: "free",
        section: 1,
        synopsis: "free",
        description: "Display memory usage information.\n\n\
            Shows total, used, and available kernel heap memory.\n\
            ClaudioOS uses a 16 MiB linked-list heap allocator.",
        examples: "  free",
        see_also: "df, ps",
    },
    ManPage {
        name: "fw",
        section: 8,
        synopsis: "fw <list|add|delete|default|flush|stats|ratelimit>",
        description: "Packet filter firewall management.\n\n\
            ClaudioOS includes a stateful packet filter firewall with\n\
            ordered rules, connection tracking, and rate limiting.\n\n\
            Subcommands:\n\
            \x20 list                              Show all rules\n\
            \x20 add <action> <dir> <proto> <src> <dst> <dport>\n\
            \x20                                   Add a new rule\n\
            \x20 delete <n>                        Delete rule by number\n\
            \x20 default allow|deny                Set default policy\n\
            \x20 flush                             Remove all rules and reset\n\
            \x20 stats                             Show packet counters\n\
            \x20 ratelimit <N>                     Max N conn/sec per IP\n\n\
            Actions: allow, deny, log\n\
            Directions: in, out, any\n\
            Protocols: tcp, udp, icmp, any\n\
            IPs: a.b.c.d, a.b.c.d/N, or 'any'\n\n\
            Rules are evaluated in order (first match wins). The firewall\n\
            tracks established connections and automatically allows return\n\
            traffic (stateful inspection).",
        examples: "  fw list\n\
            \x20 fw add allow in tcp any any 22\n\
            \x20 fw add deny in tcp any any 80\n\
            \x20 fw add allow out any any any 0\n\
            \x20 fw delete 2\n\
            \x20 fw default deny\n\
            \x20 fw ratelimit 10\n\
            \x20 fw stats\n\
            \x20 fw flush",
        see_also: "ifconfig, ping",
    },
    ManPage {
        name: "grep",
        section: 1,
        synopsis: "grep <pattern> [file ...]",
        description: "Search for patterns in files or input.\n\n\
            Prints lines matching the given pattern. If no file is\n\
            specified, reads from standard input (pipe). Pattern is\n\
            a simple substring match (not regex).",
        examples: "  grep error /var/log/messages\n  cat file.txt | grep hello",
        see_also: "cat, head, tail",
    },
    ManPage {
        name: "head",
        section: 1,
        synopsis: "head [-n <count>] [file]",
        description: "Display the first lines of a file.\n\n\
            Shows the first N lines (default 10) of a file or standard\n\
            input.",
        examples: "  head -n 5 /etc/config\n  cat file.txt | head",
        see_also: "tail, cat",
    },
    ManPage {
        name: "help",
        section: 1,
        synopsis: "help",
        description: "Display available commands.\n\n\
            Shows a summary of all built-in shell commands and keyboard\n\
            shortcuts. For detailed help on a specific command, use\n\
            'man <command>'.",
        examples: "  help\n  man ls",
        see_also: "man",
    },
    ManPage {
        name: "history",
        section: 1,
        synopsis: "history",
        description: "Display command history.\n\n\
            Shows a numbered list of previously executed commands in\n\
            the current shell session.",
        examples: "  history",
        see_also: "help",
    },
    ManPage {
        name: "ifconfig",
        section: 8,
        synopsis: "ifconfig",
        description: "Display network interface configuration.\n\n\
            Shows the status of all network interfaces including IP\n\
            address, netmask, and hardware address. ClaudioOS uses\n\
            VirtIO-net (QEMU) or Intel e1000/I219-V on real hardware.",
        examples: "  ifconfig",
        see_also: "ping, fw",
    },
    ManPage {
        name: "kill",
        section: 1,
        synopsis: "kill <agent_id>",
        description: "Terminate an agent session.\n\n\
            Sends a termination signal to the specified agent. Use 'ps'\n\
            to find agent IDs. Note: use Ctrl+B x to close the current\n\
            pane instead.",
        examples: "  kill 3",
        see_also: "ps",
    },
    ManPage {
        name: "ls",
        section: 1,
        synopsis: "ls [directory]",
        description: "List directory contents.\n\n\
            Displays the files and subdirectories in the specified\n\
            directory (or current directory if none given).",
        examples: "  ls\n  ls /home\n  ls -la /etc",
        see_also: "cd, pwd, cat",
    },
    ManPage {
        name: "man",
        section: 1,
        synopsis: "man [-k <keyword>] [command]",
        description: "Display manual pages.\n\n\
            The ClaudioOS manual page system provides formatted help for\n\
            all built-in commands.\n\n\
            With a command name, displays the full manual page including\n\
            synopsis, description, examples, and cross-references.\n\n\
            With -k, searches all man pages for entries matching the\n\
            given keyword (searches name and description).\n\n\
            Output is paged: press Space for next page, 'q' to quit\n\
            (when output exceeds one screen).",
        examples: "  man ls\n  man fw\n  man -k network\n  man man",
        see_also: "help",
    },
    ManPage {
        name: "mkdir",
        section: 1,
        synopsis: "mkdir <directory>",
        description: "Create a directory.\n\n\
            Creates a new directory at the specified path. Parent\n\
            directories must already exist.",
        examples: "  mkdir /home/user\n  mkdir projects",
        see_also: "ls, rm, touch",
    },
    ManPage {
        name: "mount",
        section: 8,
        synopsis: "mount <device> <path> <fstype>",
        description: "Mount a filesystem.\n\n\
            Attaches a filesystem on the specified device to the given\n\
            mount point. Supported filesystem types: fat32, ext4, btrfs,\n\
            ntfs.\n\n\
            Without arguments, lists currently mounted filesystems.",
        examples: "  mount /dev/sda1 /mnt fat32\n  mount",
        see_also: "umount, df",
    },
    ManPage {
        name: "mv",
        section: 1,
        synopsis: "mv <source> <destination>",
        description: "Move or rename files.\n\n\
            Moves a file from source to destination. Can be used to\n\
            rename files within the same directory.",
        examples: "  mv old.txt new.txt\n  mv file.txt /backup/",
        see_also: "cp, rm",
    },
    ManPage {
        name: "ping",
        section: 8,
        synopsis: "ping <host>",
        description: "Send ICMP echo requests to a host.\n\n\
            Tests network connectivity by sending ICMP echo request\n\
            packets to the specified host and measuring round-trip time.",
        examples: "  ping 10.0.2.2\n  ping api.anthropic.com",
        see_also: "ifconfig, fw",
    },
    ManPage {
        name: "ps",
        section: 1,
        synopsis: "ps",
        description: "List running agent sessions.\n\n\
            Displays all active agent and shell sessions with their\n\
            IDs, names, and current state (idle, thinking, streaming,\n\
            tool, error).",
        examples: "  ps",
        see_also: "kill",
    },
    ManPage {
        name: "pwd",
        section: 1,
        synopsis: "pwd",
        description: "Print the current working directory.\n\n\
            Displays the absolute path of the current working directory.",
        examples: "  pwd",
        see_also: "cd, ls",
    },
    ManPage {
        name: "reboot",
        section: 8,
        synopsis: "reboot",
        description: "Restart the system.\n\n\
            Performs an immediate system reboot using the ACPI reset\n\
            register. Falls back to keyboard controller reset if ACPI\n\
            is unavailable.",
        examples: "  reboot",
        see_also: "shutdown",
    },
    ManPage {
        name: "rm",
        section: 1,
        synopsis: "rm <file>",
        description: "Remove files.\n\n\
            Deletes the specified file. Does not remove directories\n\
            (use with caution as there is no recycle bin).",
        examples: "  rm temp.txt\n  rm /tmp/old_log",
        see_also: "cp, mv, mkdir",
    },
    ManPage {
        name: "screensaver",
        section: 1,
        synopsis: "screensaver [mode|off|timeout <secs>|list]",
        description: "Control the screensaver.\n\n\
            ClaudioOS includes TempleOS-inspired screensavers that\n\
            activate after a period of idle time.\n\n\
            Modes: starfield, matrix, bounce, pipes, clock\n\n\
            Subcommands:\n\
            \x20 screensaver               Activate with current mode\n\
            \x20 screensaver <mode>        Switch to specified mode\n\
            \x20 screensaver off           Disable screensaver\n\
            \x20 screensaver timeout <N>   Set idle timeout in seconds\n\
            \x20 screensaver list          List available modes",
        examples: "  screensaver matrix\n  screensaver timeout 60\n  screensaver list",
        see_also: "theme, clear",
    },
    ManPage {
        name: "set",
        section: 1,
        synopsis: "set <NAME>=<value>",
        description: "Set a shell variable.\n\n\
            Creates or updates a shell variable. Shell variables are\n\
            local to the current session and are not exported to child\n\
            processes.",
        examples: "  set FOO=bar\n  echo $FOO",
        see_also: "unset, export",
    },
    ManPage {
        name: "shutdown",
        section: 8,
        synopsis: "shutdown",
        description: "Power off the system.\n\n\
            Performs an ACPI S5 shutdown. On QEMU, writes to the debug\n\
            exit port as a fallback. Halts the CPU if both methods fail.",
        examples: "  shutdown",
        see_also: "reboot",
    },
    ManPage {
        name: "ssh",
        section: 1,
        synopsis: "ssh <host>",
        description: "Connect to a remote host via SSH.\n\n\
            Initiates an SSH connection to the specified host.\n\
            (Note: SSH client is not yet fully implemented.)",
        examples: "  ssh 192.168.1.1",
        see_also: "ifconfig, ping",
    },
    ManPage {
        name: "tail",
        section: 1,
        synopsis: "tail [-n <count>] [file]",
        description: "Display the last lines of a file.\n\n\
            Shows the last N lines (default 10) of a file or standard\n\
            input.",
        examples: "  tail -n 20 /var/log/messages\n  cat file.txt | tail",
        see_also: "head, cat",
    },
    ManPage {
        name: "theme",
        section: 1,
        synopsis: "theme [name|list]",
        description: "Change the terminal color theme.\n\n\
            ClaudioOS supports multiple color themes for the terminal.\n\
            Without arguments, shows the current theme.\n\n\
            Subcommands:\n\
            \x20 theme              Show current theme\n\
            \x20 theme <name>       Switch to named theme\n\
            \x20 theme list         List all available themes",
        examples: "  theme list\n  theme monokai\n  theme solarized",
        see_also: "screensaver, clear",
    },
    ManPage {
        name: "touch",
        section: 1,
        synopsis: "touch <file>",
        description: "Create an empty file.\n\n\
            Creates a new empty file at the specified path, or updates\n\
            the modification time if the file already exists.",
        examples: "  touch newfile.txt",
        see_also: "mkdir, rm, ls",
    },
    ManPage {
        name: "umount",
        section: 8,
        synopsis: "umount <path>",
        description: "Unmount a filesystem.\n\n\
            Detaches the filesystem mounted at the specified path.",
        examples: "  umount /mnt",
        see_also: "mount, df",
    },
    ManPage {
        name: "unset",
        section: 1,
        synopsis: "unset <NAME>",
        description: "Remove a shell variable.\n\n\
            Deletes the specified environment or shell variable.",
        examples: "  unset FOO",
        see_also: "set, export",
    },
    ManPage {
        name: "uptime",
        section: 1,
        synopsis: "uptime",
        description: "Display system uptime.\n\n\
            Shows how long the system has been running since boot,\n\
            measured from the PIT timer.",
        examples: "  uptime",
        see_also: "date, ps",
    },
];

// ---------------------------------------------------------------------------
// Lookup and search
// ---------------------------------------------------------------------------

/// Find a man page by name (case-insensitive).
pub fn find_page(name: &str) -> Option<&'static ManPage> {
    let lower = name.to_ascii_lowercase();
    MAN_PAGES.iter().find(|p| p.name == lower.as_str())
}

/// Search man pages by keyword (matches name and description).
pub fn search_keyword(keyword: &str) -> Vec<&'static ManPage> {
    let lower = keyword.to_ascii_lowercase();
    MAN_PAGES
        .iter()
        .filter(|p| {
            p.name.contains(lower.as_str())
                || p.description.to_ascii_lowercase().contains(lower.as_str())
                || p.synopsis.to_ascii_lowercase().contains(lower.as_str())
        })
        .collect()
}

/// List all available man pages (name + synopsis).
pub fn list_all() -> Vec<(&'static str, &'static str)> {
    MAN_PAGES.iter().map(|p| (p.name, p.synopsis)).collect()
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Extension trait for in-place lowercase on str bytes (ASCII only).
trait AsciiLower {
    fn to_ascii_lowercase(&self) -> String;
}

impl AsciiLower for str {
    fn to_ascii_lowercase(&self) -> String {
        let mut s = String::with_capacity(self.len());
        for c in self.chars() {
            if c.is_ascii_uppercase() {
                s.push((c as u8 + 32) as char);
            } else {
                s.push(c);
            }
        }
        s
    }
}

/// Format a man page for terminal display.
fn format_page(page: &ManPage) -> String {
    let mut out = String::new();

    // Header.
    out.push_str(&format!(
        "\x1b[1;97m{}({})\x1b[0m",
        page.name.to_ascii_uppercase(),
        page.section
    ));
    out.push_str("                    ");
    out.push_str(&format!(
        "\x1b[1;97mClaudioOS Manual\x1b[0m"));
    out.push_str("                    ");
    out.push_str(&format!(
        "\x1b[1;97m{}({})\x1b[0m\n",
        page.name.to_ascii_uppercase(),
        page.section
    ));
    out.push('\n');

    // Name.
    out.push_str("\x1b[1;93mNAME\x1b[0m\n");
    out.push_str(&format!("    {} - {}\n\n",
        page.name,
        page.description.lines().next().unwrap_or("")
    ));

    // Synopsis.
    out.push_str("\x1b[1;93mSYNOPSIS\x1b[0m\n");
    out.push_str(&format!("    \x1b[1m{}\x1b[0m\n\n", page.synopsis));

    // Description.
    out.push_str("\x1b[1;93mDESCRIPTION\x1b[0m\n");
    for line in page.description.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(&format!("    {}\n", line));
        }
    }
    out.push('\n');

    // Examples.
    if !page.examples.is_empty() {
        out.push_str("\x1b[1;93mEXAMPLES\x1b[0m\n");
        for line in page.examples.lines() {
            out.push_str(&format!("    \x1b[32m{}\x1b[0m\n", line.trim()));
        }
        out.push('\n');
    }

    // See also.
    if !page.see_also.is_empty() {
        out.push_str("\x1b[1;93mSEE ALSO\x1b[0m\n");
        out.push_str(&format!("    {}\n\n", page.see_also));
    }

    // Footer.
    out.push_str(&format!(
        "\x1b[90mClaudioOS v0.1.0                                              {}({})\x1b[0m\n",
        page.name.to_ascii_uppercase(),
        page.section,
    ));

    out
}

// ---------------------------------------------------------------------------
// Shell command handler
// ---------------------------------------------------------------------------

/// Handle `man` shell command. Returns the output string.
pub fn handle_command(args: &str) -> String {
    let parts: Vec<&str> = args.split_whitespace().collect();

    if parts.is_empty() {
        return String::from(
            "What manual page do you want?\nUsage: man <command>  or  man -k <keyword>\n"
        );
    }

    // man -k <keyword> — search mode.
    if parts[0] == "-k" {
        if parts.len() < 2 {
            return String::from("Usage: man -k <keyword>\n");
        }
        let keyword = parts[1];
        let results = search_keyword(keyword);
        if results.is_empty() {
            return format!("No manual entry matching '{}'.\n", keyword);
        }
        let mut out = String::new();
        for page in results {
            out.push_str(&format!(
                "\x1b[1m{}({})\x1b[0m - {}\n",
                page.name,
                page.section,
                page.description.lines().next().unwrap_or(""),
            ));
        }
        return out;
    }

    // man <command> — display mode.
    let name = parts[0];
    match find_page(name) {
        Some(page) => format_page(page),
        None => format!(
            "No manual entry for '{}'.\nTry 'man -k {}' to search, or 'help' to list commands.\n",
            name, name
        ),
    }
}

/// Enhanced help output that lists available man pages.
pub fn help_with_manpages() -> String {
    let mut out = String::new();
    out.push_str("\x1b[1;96mClaudioOS Shell Commands\x1b[0m\n");
    out.push_str("\x1b[90m");
    out.push_str(&"-".repeat(52));
    out.push_str("\x1b[0m\n\n");

    let pages = list_all();

    // Group by section.
    out.push_str("\x1b[1;93mUser Commands:\x1b[0m\n");
    for (name, synopsis) in &pages {
        if let Some(page) = find_page(name) {
            if page.section == 1 {
                out.push_str(&format!("  \x1b[32m{:<14}\x1b[0m {}\n", name, synopsis));
            }
        }
    }

    out.push_str("\n\x1b[1;93mSystem Administration:\x1b[0m\n");
    for (name, synopsis) in &pages {
        if let Some(page) = find_page(name) {
            if page.section == 8 {
                out.push_str(&format!("  \x1b[32m{:<14}\x1b[0m {}\n", name, synopsis));
            }
        }
    }

    out.push_str("\n\x1b[1;93mDashboard Shortcuts:\x1b[0m\n");
    out.push_str("  \x1b[32mCtrl+B \"\x1b[0m      Split pane horizontally\n");
    out.push_str("  \x1b[32mCtrl+B %\x1b[0m      Split pane vertically\n");
    out.push_str("  \x1b[32mCtrl+B n/p\x1b[0m    Next/previous pane\n");
    out.push_str("  \x1b[32mCtrl+B c\x1b[0m      New agent pane\n");
    out.push_str("  \x1b[32mCtrl+B s\x1b[0m      New shell pane\n");
    out.push_str("  \x1b[32mCtrl+B f\x1b[0m      File manager\n");
    out.push_str("  \x1b[32mCtrl+B w\x1b[0m      Web browser\n");
    out.push_str("  \x1b[32mCtrl+B x\x1b[0m      Close pane\n");

    out.push_str("\n\x1b[90mType 'man <command>' for detailed help.\x1b[0m\n");
    out
}
