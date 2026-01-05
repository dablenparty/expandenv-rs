/// Represents errors that may occur when expanding envvars inside a string.
#[derive(Debug, thiserror::Error)]
pub enum ExpandError {
    #[error("failed to get value of envvar: {0}")]
    EnvvarReadError(String),
    #[error("failed to expand envvar: {0}")]
    OsVarError(#[from] std::env::VarError),
}
