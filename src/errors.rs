/*!
* Custom error type(s).
*/

/// Represents errors that may occur when expanding envvars inside a string.
#[derive(Debug, thiserror::Error)]
pub enum ExpandError {
    /// Represents an environment variable that could not be read and contains the variable name.
    #[error("failed to get value of envvar: {0}")]
    EnvvarReadError(String),
    /// Represents an error that occurred trying to read the environment variable from the OS.
    #[error("failed to expand envvar: {0}")]
    OsVarError(#[from] std::env::VarError),
}
