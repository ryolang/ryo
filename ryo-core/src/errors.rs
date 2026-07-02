use crate::diag::Diag;

#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
pub enum CompilerError {
    IoError(std::io::Error),
    /// Parse / lower / sema diagnostics. Already rendered to stderr
    /// by the driver before this is constructed; the variant exists
    /// so the process exit code reflects the failure and the count
    /// is available for tests.
    Diagnostics(Vec<Diag>),
    CodegenError(String),
    LinkError(String),
    ToolchainError(String),
    ExecutionError(String),
}

impl std::fmt::Display for CompilerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompilerError::IoError(e) => write!(f, "IO error: {}", e),
            CompilerError::Diagnostics(diags) => {
                let errs = diags
                    .iter()
                    .filter(|d| d.severity == crate::diag::Severity::Error)
                    .count();
                let noun = if errs == 1 { "error" } else { "errors" };
                write!(f, "compilation failed: {} {}", errs, noun)
            }
            CompilerError::CodegenError(msg) => write!(f, "Codegen error: {}", msg),
            CompilerError::LinkError(msg) => write!(f, "Link error: {}", msg),
            CompilerError::ToolchainError(msg) => write!(f, "Toolchain error: {}", msg),
            CompilerError::ExecutionError(msg) => write!(f, "Execution error: {}", msg),
        }
    }
}

impl std::error::Error for CompilerError {}

impl From<std::io::Error> for CompilerError {
    fn from(error: std::io::Error) -> Self {
        CompilerError::IoError(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::{Diag, DiagCode};
    use chumsky::span::{SimpleSpan, Span as _};

    fn err_diag() -> Diag {
        Diag::error(SimpleSpan::new((), 0..0), DiagCode::TypeMismatch, "x")
    }

    #[test]
    fn diagnostics_display_uses_singular_for_one_error() {
        let e = CompilerError::Diagnostics(vec![err_diag()]);
        assert_eq!(format!("{}", e), "compilation failed: 1 error");
    }

    #[test]
    fn diagnostics_display_uses_plural_for_zero_or_many_errors() {
        let zero = CompilerError::Diagnostics(vec![]);
        assert_eq!(format!("{}", zero), "compilation failed: 0 errors");
        let many = CompilerError::Diagnostics(vec![err_diag(), err_diag(), err_diag()]);
        assert_eq!(format!("{}", many), "compilation failed: 3 errors");
    }
}
