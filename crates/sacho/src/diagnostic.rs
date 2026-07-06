/// Process exit codes used by Sacho frontends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    /// Successful execution with no violations.
    Clean = 0,

    /// Check completed and found policy violations.
    Violations = 1,

    /// Usage, configuration, or runtime error.
    Error = 2,
}

impl ExitCode {
    /// Returns the numeric process exit code.
    pub fn code(self) -> i32 {
        self as i32
    }
}
