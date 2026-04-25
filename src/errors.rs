#[derive(Debug)]
#[allow(clippy::enum_variant_names)]
pub enum CompilerError {
    IoError(std::io::Error),
    ParseError(String),
    LowerError(String),
    SemaError(String),
    CodegenError(String),
    LinkError(String),
    ToolchainError(String),
    ExecutionError(String),
}

impl std::fmt::Display for CompilerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompilerError::IoError(e) => write!(f, "IO error: {}", e),
            CompilerError::ParseError(msg) => write!(f, "Parse error: {}", msg),
            CompilerError::LowerError(msg) => write!(f, "Lower error: {}", msg),
            CompilerError::SemaError(msg) => write!(f, "Sema error: {}", msg),
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
