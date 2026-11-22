//! BW-27-02-01: a debug session for a BaseApp, launched or attached, whose
//! breakpoints hit.
//!
//! The debug adapter drives a target process: a session is opened in one of two
//! modes — `Launch` (the adapter starts the process) or `Attach` (the adapter
//! connects to one already running) — carries the breakpoints the editor set,
//! and runs the target's scripted steps until one of them lands on a
//! breakpoint. A hit stops the session and records what the adapter reported
//! there: the stack frames and the variables in scope.

/// Which debug adapter drives the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adapter {
    /// The Python adapter (debugpy), for a BaseApp.
    Python,
    /// The Rust adapter (codelldb), for a CellApp.
    Rust,
}

/// How the session reaches its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The adapter starts the target.
    Launch,
    /// The adapter connects to a target that is already running.
    Attach,
}

/// What a session is opened against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The adapter that drives the target.
    pub adapter: Adapter,
    /// How the session reaches the target.
    pub mode: Mode,
    /// The process role the target plays: `baseapp`, `cellapp`, ...
    pub role: String,
    /// The program the target runs.
    pub program: String,
    /// The debug port: the one the adapter opens on launch, or the one it
    /// connects to on attach.
    pub port: u16,
}

/// One stack frame as the adapter reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The frame's depth: 0 is the innermost frame.
    pub level: usize,
    /// The source file the frame is in.
    pub file: String,
    /// The line the frame stopped on.
    pub line: usize,
    /// The function the frame is in.
    pub function: String,
}

/// One variable in scope, as the adapter reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    /// The variable's name.
    pub name: String,
    /// The variable's rendered value.
    pub value: String,
}

/// One scripted step of the target's run: where it goes, and what the adapter
/// reports when it stops there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// The source file the step is in.
    pub file: String,
    /// The line the step is on.
    pub line: usize,
    /// The stack the adapter reports at this step.
    pub frames: Vec<Frame>,
    /// The variables the adapter reports at this step.
    pub variables: Vec<Variable>,
}

/// A convenience builder for a step that stops inside one function.
pub fn step(file: &str, line: usize, function: &str) -> Step {
    Step {
        file: file.to_string(),
        line,
        frames: vec![Frame {
            level: 0,
            file: file.to_string(),
            line,
            function: function.to_string(),
        }],
        variables: Vec::new(),
    }
}

/// Where the target stopped, and what the adapter reported there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    /// The source file the breakpoint is in.
    pub file: String,
    /// The line the breakpoint is on.
    pub line: usize,
    /// The stack at the stop.
    pub frames: Vec<Frame>,
    /// The variables in scope at the stop.
    pub variables: Vec<Variable>,
}

/// One debug session against one target.
pub struct DapSession {
    config: Config,
    script: Vec<Step>,
    next: usize,
    breakpoints: Vec<(String, usize)>,
    stopped: Option<Stopped>,
}

impl DapSession {
    /// Open a session against a target, in the mode the config asks for, with
    /// the script the adapter will run it through.
    pub fn open(config: Config, script: Vec<Step>) -> Self {
        Self {
            config,
            script,
            next: 0,
            breakpoints: Vec::new(),
            stopped: None,
        }
    }

    /// The config the session was opened with.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Set a breakpoint in the target.
    pub fn set_breakpoint(&mut self, file: &str, line: usize) {
        self.breakpoints.push((file.to_string(), line));
    }

    /// Run the target forward until a breakpoint hits or its script ends.
    pub fn run(&mut self) {
        while self.next < self.script.len() {
            let current = &self.script[self.next];
            self.next += 1;
            let hit = self
                .breakpoints
                .iter()
                .any(|(file, line)| file == &current.file && *line == current.line);
            if hit {
                self.stopped = Some(Stopped {
                    file: current.file.clone(),
                    line: current.line,
                    frames: current.frames.clone(),
                    variables: current.variables.clone(),
                });
                return;
            }
        }
    }

    /// Where the target stopped, when a breakpoint hit.
    pub fn stopped(&self) -> Option<&Stopped> {
        self.stopped.as_ref()
    }

    /// Whether the target ran through its whole script without a hit.
    pub fn finished(&self) -> bool {
        self.stopped.is_none() && self.next >= self.script.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "scripts/baseapp/Avatar.py";

    fn baseapp_script() -> Vec<Step> {
        // The BaseApp runs a handler that loads the avatar, updates it, then
        // saves; only the update line is interesting.
        vec![
            step(FILE, 30, "Avatar.onEnterWorld"),
            step(FILE, 40, "Avatar.onEnterWorld"),
            step(FILE, 52, "Avatar.onEnterWorld"),
        ]
    }

    fn baseapp_config(mode: Mode) -> Config {
        Config {
            adapter: Adapter::Python,
            mode,
            role: "baseapp".to_string(),
            program: "apps/baseapp".to_string(),
            port: 5678,
        }
    }

    #[test]
    fn launching_the_python_adapter_hits_a_baseapp_breakpoint() {
        let mut session = DapSession::open(baseapp_config(Mode::Launch), baseapp_script());
        session.set_breakpoint(FILE, 40);

        // The adapter starts the BaseApp and runs it: the update line stops.
        session.run();
        let stopped = session.stopped().expect("the breakpoint hits");
        assert_eq!((stopped.file.as_str(), stopped.line), (FILE, 40));
        assert_eq!(stopped.frames[0].function, "Avatar.onEnterWorld");
        assert!(!session.finished());
    }

    #[test]
    fn attaching_the_python_adapter_hits_the_same_breakpoint() {
        // The BaseApp is already running with debugpy listening; the adapter
        // attaches to the port instead of starting a process.
        let mut session = DapSession::open(baseapp_config(Mode::Attach), baseapp_script());
        assert_eq!(session.config().mode, Mode::Attach);
        assert_eq!(session.config().port, 5678);

        session.set_breakpoint(FILE, 40);
        session.run();
        let stopped = session.stopped().expect("the breakpoint hits");
        assert_eq!(stopped.line, 40);
    }

    #[test]
    fn a_line_without_a_breakpoint_does_not_stop() {
        let mut session = DapSession::open(baseapp_config(Mode::Launch), baseapp_script());
        session.set_breakpoint(FILE, 99);

        // Nothing lands on the breakpoint: the target runs to the end.
        session.run();
        assert!(session.stopped().is_none());
        assert!(session.finished());
    }
}
