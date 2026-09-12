use std::fmt;

pub type Result<T> = std::result::Result<T,ApiError>;

#[derive(Clone, Debug)]
pub struct ApiError(String);

impl ApiError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ApiError {}

impl From<ureq::Error> for ApiError {
    fn from(error: ureq::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}
