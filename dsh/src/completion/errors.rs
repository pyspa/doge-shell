use std::fmt;

#[derive(Debug)]
pub enum GeneratorError {
    MissingCommand(String),
    Other(anyhow::Error),
}

impl fmt::Display for GeneratorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GeneratorError::MissingCommand(cmd) => write!(f, "Missing command definition: {}", cmd),
            GeneratorError::Other(e) => write!(f, "Generator error: {}", e),
        }
    }
}

impl std::error::Error for GeneratorError {}

impl From<anyhow::Error> for GeneratorError {
    fn from(error: anyhow::Error) -> Self {
        GeneratorError::Other(error)
    }
}
