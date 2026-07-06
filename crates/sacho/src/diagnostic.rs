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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_process_values() {
        assert_eq!(ExitCode::Clean.code(), 0);
        assert_eq!(ExitCode::Violations.code(), 1);
        assert_eq!(ExitCode::Error.code(), 2);
    }
}
