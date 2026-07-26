use std::fmt;
use std::io;

pub type Result<T> = std::result::Result<T, EmoteError>;

#[derive(Debug)]
pub enum EmoteError {
    Io(io::Error),
    InvalidFormat(String),
    Unsupported(String),
}

impl fmt::Display for EmoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::InvalidFormat(message) => write!(f, "invalid E-Mote data: {message}"),
            Self::Unsupported(message) => write!(f, "unsupported E-Mote feature: {message}"),
        }
    }
}

impl std::error::Error for EmoteError {}

impl From<io::Error> for EmoteError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
