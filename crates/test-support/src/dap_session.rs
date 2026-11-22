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

    /// The stack the adapter reported at the stop, when a breakpoint hit.
    pub fn stack(&self) -> &[Frame] {
        self.stopped
            .as_ref()
            .map(|stopped| stopped.frames.as_slice())
            .unwrap_or(&[])
    }

    /// The variables in scope the adapter reported at the stop, when a
    /// breakpoint hit.
    pub fn variables(&self) -> &[Variable] {
        self.stopped
            .as_ref()
            .map(|stopped| stopped.variables.as_slice())
            .unwrap_or(&[])
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

    const CELLAPP_FILE: &str = "crates/server/src/cellapp.rs";

    fn cellapp_script() -> Vec<Step> {
        // The CellApp runs its space update twice a tick.
        vec![
            step(CELLAPP_FILE, 120, "space::CellApp::update"),
            step(CELLAPP_FILE, 134, "space::CellApp::update"),
        ]
    }

    fn cellapp_config(mode: Mode) -> Config {
        Config {
            adapter: Adapter::Rust,
            mode,
            role: "cellapp".to_string(),
            program: "apps/cellapp".to_string(),
            port: 12345,
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

    #[test]
    fn launching_the_rust_adapter_hits_a_cellapp_breakpoint() {
        let mut session = DapSession::open(cellapp_config(Mode::Launch), cellapp_script());
        session.set_breakpoint(CELLAPP_FILE, 120);

        // The Rust adapter starts the CellApp and runs it: the first update
        // line stops, in the right file and function.
        session.run();
        let stopped = session.stopped().expect("the breakpoint hits");
        assert_eq!((stopped.file.as_str(), stopped.line), (CELLAPP_FILE, 120));
        assert_eq!(stopped.frames[0].function, "space::CellApp::update");
        assert!(!session.finished());
    }

    #[test]
    fn attaching_the_rust_adapter_hits_the_same_breakpoint() {
        // The CellApp is already running; codelldb attaches to its debug port.
        let mut session = DapSession::open(cellapp_config(Mode::Attach), cellapp_script());
        assert_eq!(session.config().adapter, Adapter::Rust);
        assert_eq!(session.config().port, 12345);

        session.set_breakpoint(CELLAPP_FILE, 120);
        session.run();
        let stopped = session.stopped().expect("the breakpoint hits");
        assert_eq!(stopped.line, 120);
    }

    fn populated_script() -> Vec<Step> {
        // One fixed breakpoint with a full report: three frames and three
        // variables in scope.
        vec![Step {
            file: FILE.to_string(),
            line: 40,
            frames: vec![
                Frame {
                    level: 0,
                    file: FILE.to_string(),
                    line: 40,
                    function: "Avatar.onEnterWorld".to_string(),
                },
                Frame {
                    level: 1,
                    file: "scripts/baseapp/BaseApp.py".to_string(),
                    line: 88,
                    function: "BaseApp.onAvatarEnter".to_string(),
                },
                Frame {
                    level: 2,
                    file: "apps/baseapp/main.py".to_string(),
                    line: 12,
                    function: "<module>".to_string(),
                },
            ],
            variables: vec![
                Variable {
                    name: "name".to_string(),
                    value: "Avatar".to_string(),
                },
                Variable {
                    name: "hp".to_string(),
                    value: "100".to_string(),
                },
                Variable {
                    name: "mood".to_string(),
                    value: "calm".to_string(),
                },
            ],
        }]
    }

    #[test]
    fn a_stopped_session_reports_the_frames_at_the_breakpoint() {
        let mut session = DapSession::open(baseapp_config(Mode::Launch), populated_script());
        session.set_breakpoint(FILE, 40);
        session.run();

        // The fixed breakpoint carries a fixed stack: three frames, innermost
        // first, each with its own file and function.
        let stack = session.stack();
        assert_eq!(stack.len(), 3);
        assert_eq!(stack[0].level, 0);
        assert_eq!(stack[0].function, "Avatar.onEnterWorld");
        assert_eq!(stack[1].function, "BaseApp.onAvatarEnter");
        assert_eq!(stack[2].function, "<module>");
        assert_eq!(stack[0].line, 40);
        assert_eq!(stack[1].line, 88);
    }

    #[test]
    fn a_stopped_session_reports_the_variables_in_scope() {
        let mut session = DapSession::open(baseapp_config(Mode::Launch), populated_script());
        session.set_breakpoint(FILE, 40);
        session.run();

        // The same fixed breakpoint carries a fixed scope: the three variables
        // in the order the adapter reported them.
        let variables = session.variables();
        let names: Vec<(&str, &str)> = variables
            .iter()
            .map(|v| (v.name.as_str(), v.value.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![("name", "Avatar"), ("hp", "100"), ("mood", "calm")]
        );
    }

    #[test]
    fn before_a_stop_or_after_a_clean_finish_there_is_no_report() {
        let mut session = DapSession::open(baseapp_config(Mode::Launch), populated_script());
        assert!(session.stack().is_empty());
        assert!(session.variables().is_empty());

        // The breakpoint never matches: the target finishes, still no report.
        session.set_breakpoint(FILE, 99);
        session.run();
        assert!(session.finished());
        assert!(session.stack().is_empty());
    }
}
