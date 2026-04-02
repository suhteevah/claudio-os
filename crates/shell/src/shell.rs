//! Main shell loop — ties everything together.
//!
//! The `Shell` struct holds environment, history, VFS reference, and AI callback.
//! `run()` reads a line, parses, executes, prints output, and loops.
//! Handles Ctrl+C (cancel), Ctrl+D (exit), startup rc files.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;

use crate::ai::{self, AiContext, AiShellCallback, NoOpAiCallback};
use crate::builtin::{self, SystemInfo, Vfs};
use crate::env::Environment;
use crate::parser::{History, Pipeline, TabCompleter};
use crate::pipe::{CommandExecutor, CommandOutput, PipelineExecutor};
use crate::prompt::Prompt;
use crate::script;

/// Trait for reading a line of input from the user.
/// The kernel terminal pane implements this.
pub trait LineReader {
    /// Read a line of user input. Returns `None` on Ctrl+D (EOF).
    fn read_line(&mut self, prompt: &str) -> Option<String>;

    /// Write output text to the terminal.
    fn write_output(&mut self, text: &str);

    /// Check if Ctrl+C was pressed (cancel current operation).
    fn check_interrupt(&self) -> bool;

    /// Clear the interrupt flag.
    fn clear_interrupt(&mut self);
}

/// The main shell state machine.
pub struct Shell {
    /// Environment variables.
    pub env: Environment,
    /// Command history.
    pub history: History,
    /// Prompt generator.
    pub prompt: Prompt,
    /// Tab completer.
    pub completer: TabCompleter,
    /// Last exit code.
    pub last_exit_code: i32,
    /// Whether the shell should keep running.
    pub running: bool,
    /// AI callback (optional — defaults to NoOp).
    ai_callback: Box<dyn AiShellCallback + Send>,
}

impl Shell {
    /// Create a new shell with default settings.
    pub fn new() -> Self {
        log::info!("shell: initializing ClaudioOS shell");

        let env = Environment::new();
        let history = History::new(1000);
        let prompt = Prompt::new();
        let completer = TabCompleter::new();

        log::info!("shell: ready");

        Shell {
            env,
            history,
            prompt,
            completer,
            last_exit_code: 0,
            running: true,
            ai_callback: Box::new(NoOpAiCallback),
        }
    }

    /// Set the AI callback for natural language processing.
    pub fn set_ai_callback(&mut self, callback: Box<dyn AiShellCallback + Send>) {
        log::info!("shell: AI callback connected");
        self.ai_callback = callback;
    }

    /// Set the agent name shown in the prompt.
    pub fn set_agent_name(&mut self, name: Option<String>) {
        self.prompt.set_agent(name);
    }

    /// Run the main shell loop.
    /// Takes mutable references to VFS, SystemInfo, and LineReader.
    pub fn run(
        &mut self,
        vfs: &mut dyn Vfs,
        sys: &mut dyn SystemInfo,
        reader: &mut dyn LineReader,
    ) {
        log::info!("shell: entering main loop");

        // Run startup rc file if it exists
        self.run_startup_rc(vfs, sys, reader);

        while self.running {
            let prompt_str = self.prompt.render(&self.env);

            let line = match reader.read_line(&prompt_str) {
                Some(line) => line,
                None => {
                    // Ctrl+D — exit
                    log::info!("shell: EOF received (Ctrl+D), exiting");
                    reader.write_output("exit\n");
                    break;
                }
            };

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Add to history
            self.history.push(trimmed);

            // Check for interrupt
            if reader.check_interrupt() {
                log::debug!("shell: interrupt detected, cancelling");
                reader.clear_interrupt();
                reader.write_output("^C\n");
                continue;
            }

            // Execute the input
            let (output, exit_code) = self.execute_input(trimmed, vfs, sys, reader);

            self.last_exit_code = exit_code;

            if !output.is_empty() {
                reader.write_output(&output);
                if !output.ends_with('\n') {
                    reader.write_output("\n");
                }
            }

            // Check if we should exit
            if trimmed == "exit" {
                log::info!("shell: exit command received");
                self.running = false;
            }
        }

        log::info!("shell: main loop ended");
    }

    /// Execute a single input line (command or natural language).
    /// Returns (output_text, exit_code).
    pub fn execute_input(
        &mut self,
        input: &str,
        vfs: &mut dyn Vfs,
        sys: &mut dyn SystemInfo,
        reader: &mut dyn LineReader,
    ) -> (String, i32) {
        log::debug!("shell: executing input: {:?}", input);

        // Expand environment variables
        let expanded = self.env.expand_vars(input);
        log::trace!("shell: after expansion: {:?}", expanded);

        // Classify: command or natural language?
        let kind = ai::classify_input(&expanded);

        match kind {
            ai::InputKind::Empty => (String::new(), 0),

            ai::InputKind::Command => {
                self.execute_command(&expanded, vfs, sys)
            }

            ai::InputKind::NaturalLanguage => {
                self.execute_natural_language(&expanded, vfs, sys, reader)
            }
        }
    }

    /// Execute a traditional shell command (may be a pipeline).
    fn execute_command(
        &mut self,
        input: &str,
        vfs: &mut dyn Vfs,
        sys: &mut dyn SystemInfo,
    ) -> (String, i32) {
        log::debug!("shell: parsing command: {:?}", input);

        let pipeline = match Pipeline::parse(input) {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("claudio-shell: parse error: {}", e);
                log::warn!("shell: {}", msg);
                return (msg, 1);
            }
        };

        // For single commands, try builtins first
        if pipeline.is_single() {
            let cmd = &pipeline.commands[0];

            // Handle special builtins that modify shell state
            if cmd.name == "cd" {
                return match builtin::execute_builtin(
                    &cmd.name, &cmd.args, None, &mut self.env, vfs, sys,
                ) {
                    Some(Ok(out)) => (out, 0),
                    Some(Err(e)) => (e, 1),
                    None => (format!("{}: command not found", cmd.name), 127),
                };
            }

            if cmd.name == "exit" {
                self.running = false;
                return (String::new(), 0);
            }

            if let Some(result) = builtin::execute_builtin(
                &cmd.name, &cmd.args, None, &mut self.env, vfs, sys,
            ) {
                return match result {
                    Ok(out) => (out, 0),
                    Err(e) => (e, 1),
                };
            }

            // Not a builtin — unknown command
            let msg = format!("{}: command not found", cmd.name);
            log::warn!("shell: {}", msg);
            return (msg, 127);
        }

        // Pipeline execution
        let mut executor = ShellCommandExecutor {
            env: &mut self.env,
            vfs,
            sys,
        };

        let output = PipelineExecutor::execute(&pipeline, &mut executor);

        let text = if !output.stderr.is_empty() {
            let mut combined = output.stderr_str();
            let stdout = output.stdout_str();
            if !stdout.is_empty() {
                combined.push('\n');
                combined.push_str(&stdout);
            }
            combined
        } else {
            output.stdout_str()
        };

        (text, output.exit_code)
    }

    /// Execute a natural language query through the AI.
    fn execute_natural_language(
        &mut self,
        input: &str,
        vfs: &mut dyn Vfs,
        sys: &mut dyn SystemInfo,
        reader: &mut dyn LineReader,
    ) -> (String, i32) {
        log::info!("shell: AI mode — processing {:?}", input);

        let context = AiContext {
            pwd: String::from(self.env.pwd()),
            recent_history: self
                .history
                .entries()
                .iter()
                .rev()
                .take(10)
                .cloned()
                .collect(),
            env_vars: self
                .env
                .all()
                .into_iter()
                .map(|(k, v)| (String::from(k), String::from(v)))
                .collect(),
        };

        let proposal = match ai::process_natural_language(
            input,
            &context,
            self.ai_callback.as_mut(),
        ) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("shell: AI processing failed: {}", e);
                return (e, 1);
            }
        };

        // Display proposal and ask for confirmation
        let display = ai::format_proposal(&proposal);
        reader.write_output(&display);

        // Read confirmation
        let confirmation = match reader.read_line("") {
            Some(line) => line,
            None => return (String::from("Cancelled."), 1),
        };

        let confirmed = matches!(
            confirmation.trim().to_lowercase().as_str(),
            "y" | "yes"
        );

        if !confirmed {
            log::info!("shell: AI proposal rejected by user");
            return (String::from("Cancelled."), 0);
        }

        log::info!("shell: AI proposal confirmed, executing {} command(s)", proposal.commands.len());

        // Execute each proposed command
        let mut output = String::new();
        let mut last_code = 0;

        for cmd_line in &proposal.commands {
            log::debug!("shell: AI exec: {:?}", cmd_line);
            reader.write_output(&format!("\x1b[2m$ {}\x1b[0m\n", cmd_line));

            let (out, code) = self.execute_command(cmd_line, vfs, sys);
            if !out.is_empty() {
                reader.write_output(&out);
                if !out.ends_with('\n') {
                    reader.write_output("\n");
                }
                output.push_str(&out);
                output.push('\n');
            }
            last_code = code;

            if code != 0 {
                log::warn!(
                    "shell: AI command {:?} failed with exit code {}",
                    cmd_line,
                    code
                );
                break;
            }
        }

        (output, last_code)
    }

    /// Run the startup rc file (/etc/claudio.rc) if it exists.
    fn run_startup_rc(
        &mut self,
        vfs: &mut dyn Vfs,
        sys: &mut dyn SystemInfo,
        reader: &mut dyn LineReader,
    ) {
        const RC_PATH: &str = "/etc/claudio.rc";

        log::info!("shell: checking for startup rc file at {}", RC_PATH);

        if !vfs.exists(RC_PATH) {
            log::debug!("shell: no rc file found at {}", RC_PATH);
            return;
        }

        match vfs.read_file(RC_PATH) {
            Ok(data) => {
                let content = String::from_utf8_lossy(&data);
                log::info!(
                    "shell: running rc file {} ({} bytes)",
                    RC_PATH,
                    data.len()
                );

                match script::parse_script(&content) {
                    Ok(parsed) => {
                        let mut executor = ShellScriptExecutor {
                            shell: self,
                            vfs,
                            sys,
                            reader,
                        };
                        let (output, code) = script::ScriptRunner::run(&parsed, &mut executor);
                        if !output.is_empty() {
                            reader.write_output(&output);
                        }
                        log::info!("shell: rc file finished with exit code {}", code);
                    }
                    Err(e) => {
                        log::error!("shell: rc file parse error: {}", e);
                        reader.write_output(&format!(
                            "claudio-shell: error in {}: {}\n",
                            RC_PATH, e
                        ));
                    }
                }
            }
            Err(e) => {
                log::error!("shell: failed to read rc file: {}", e);
            }
        }
    }
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}

/// Adapter that implements `CommandExecutor` for pipeline execution,
/// delegating to builtins and VFS.
struct ShellCommandExecutor<'a> {
    env: &'a mut Environment,
    vfs: &'a mut dyn Vfs,
    sys: &'a mut dyn SystemInfo,
}

impl<'a> CommandExecutor for ShellCommandExecutor<'a> {
    fn execute(
        &mut self,
        name: &str,
        args: &[String],
        stdin: Option<&[u8]>,
    ) -> CommandOutput {
        log::debug!(
            "shell::executor: executing {:?} with {} arg(s)",
            name,
            args.len()
        );

        match builtin::execute_builtin(name, args, stdin, self.env, self.vfs, self.sys) {
            Some(Ok(output)) => CommandOutput::success(output.into_bytes()),
            Some(Err(e)) => CommandOutput::error(&e, 1),
            None => CommandOutput::error(
                &format!("{}: command not found", name),
                127,
            ),
        }
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>, String> {
        self.vfs.read_file(path)
    }

    fn write_file(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        self.vfs.write_file(path, data)
    }

    fn append_file(&mut self, path: &str, data: &[u8]) -> Result<(), String> {
        self.vfs.append_file(path, data)
    }
}

/// Adapter that implements `ScriptExecutor` for rc file execution.
struct ShellScriptExecutor<'a, 'b> {
    shell: &'a mut Shell,
    vfs: &'b mut dyn Vfs,
    sys: &'b mut dyn SystemInfo,
    reader: &'b mut dyn LineReader,
}

impl<'a, 'b> script::ScriptExecutor for ShellScriptExecutor<'a, 'b> {
    fn execute_line(&mut self, line: &str) -> (String, i32) {
        self.shell.execute_command(line, self.vfs, self.sys)
    }

    fn set_var(&mut self, name: &str, value: &str) {
        self.shell.env.set(name, value);
    }

    fn get_var(&self, name: &str) -> Option<String> {
        self.shell.env.get(name).map(String::from)
    }
}

// Note: Shell::run takes `reader` but the borrow checker for ShellScriptExecutor
// requires careful lifetime management. In ClaudioOS, the actual LineReader
// implementation lives in the kernel terminal pane, which has a 'static-ish lifetime.
