//! Errors returned by the skill module.

use std::fmt;

use lyquid::prelude::Hash;

/// Result type used throughout the skill module.
pub type SkillResult<T> = Result<T, SkillError>;

/// Errors that can occur while loading or using skills.
#[derive(Debug)]
pub enum SkillError {
    /// A filesystem operation failed.
    Io(std::io::Error),
    /// The Markdown frontmatter is missing or malformed.
    InvalidFrontmatter(String),
    /// The skill contents are not valid.
    InvalidSkill(String),
    /// The requested skill or resource does not exist.
    NotFound,
    /// A computed digest did not match the expected digest.
    DigestMismatch { expected: Hash, actual: Hash },
    /// The shared manager has already been initialized.
    AlreadyInitialized,
    /// The shared manager has not been initialized yet.
    NotInitialized,
}

impl fmt::Display for SkillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "skill I/O error: {error}"),
            Self::InvalidFrontmatter(message) => {
                write!(f, "invalid skill frontmatter: {message}")
            }
            Self::InvalidSkill(message) => write!(f, "invalid skill: {message}"),
            Self::NotFound => write!(f, "skill was not found"),
            Self::DigestMismatch { expected, actual } => {
                write!(
                    f,
                    "skill digest mismatch: expected {expected}, computed {actual}"
                )
            }
            Self::AlreadyInitialized => write!(f, "skill manager is already initialized"),
            Self::NotInitialized => write!(f, "skill manager has not been initialized"),
        }
    }
}

impl std::error::Error for SkillError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SkillError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
