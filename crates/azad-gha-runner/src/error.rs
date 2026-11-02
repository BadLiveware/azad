use std::fmt;

/// A problem that stops a step or the job, with a message for the log.
#[derive(Debug)]
pub struct Error {
    message: String,
}

impl Error {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<crate::expr::Error> for Error {
    fn from(error: crate::expr::Error) -> Self {
        Self::new(error.to_string())
    }
}

/// Adds what the runner tried to do to an error from below.
pub(crate) trait WithContext<T> {
    fn with_context(self, describe: impl FnOnce() -> String) -> Result<T, Error>;
}

impl<T, E: fmt::Display> WithContext<T> for Result<T, E> {
    fn with_context(self, describe: impl FnOnce() -> String) -> Result<T, Error> {
        self.map_err(|error| Error::new(format!("{}: {error}", describe())))
    }
}
