/// Top-level error type returned by the public Rust API.
///
/// Variants are intentionally coarse enough for stable programmatic handling
/// while their display strings carry the operation-specific detail. Prefer
/// matching [`OxideError::kind`] or [`OxideError::code`] in application logic
/// when you do not need a variant's fields.
#[derive(thiserror::Error, Debug)]
pub enum OxideError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed PDF: {0}")]
    MalformedPdf(String),
    #[error("parse error: {0}")]
    ParseError(String),
    #[error("missing object {number} {generation}")]
    MissingObject { number: u32, generation: u16 },
    #[error("unsupported feature: {0}")]
    UnsupportedFeature(String),
    #[error("document is encrypted")]
    EncryptedDocument,
    #[error("encrypted PDF: {0}")]
    EncryptedPdf(String),
    #[error("operation cancelled: {0}")]
    Cancelled(String),
    #[error("resource limit exceeded: {0}")]
    ResourceLimit(String),
}

/// Stable high-level error categories for integrators.
///
/// `OxideError` variants may carry detailed context; this category enum is the
/// compact taxonomy suitable for metrics, HTTP mapping, retry policy, and SDK
/// bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    Io,
    MalformedPdf,
    Parse,
    MissingObject,
    UnsupportedFeature,
    Encrypted,
    Cancelled,
    ResourceLimit,
}

impl ErrorKind {
    /// Stable snake_case code for logs, JSON APIs, and FFI bindings.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::MalformedPdf => "malformed_pdf",
            Self::Parse => "parse",
            Self::MissingObject => "missing_object",
            Self::UnsupportedFeature => "unsupported_feature",
            Self::Encrypted => "encrypted",
            Self::Cancelled => "cancelled",
            Self::ResourceLimit => "resource_limit",
        }
    }

    /// True when the caller can usually fix the request/input without retrying
    /// the same bytes unchanged.
    pub const fn is_input_error(self) -> bool {
        matches!(
            self,
            Self::MalformedPdf
                | Self::Parse
                | Self::MissingObject
                | Self::UnsupportedFeature
                | Self::Encrypted
                | Self::ResourceLimit
        )
    }
}

impl OxideError {
    /// Return this error's stable high-level category.
    pub const fn kind(&self) -> ErrorKind {
        match self {
            Self::Io(_) => ErrorKind::Io,
            Self::MalformedPdf(_) => ErrorKind::MalformedPdf,
            Self::ParseError(_) => ErrorKind::Parse,
            Self::MissingObject { .. } => ErrorKind::MissingObject,
            Self::UnsupportedFeature(_) => ErrorKind::UnsupportedFeature,
            Self::EncryptedDocument | Self::EncryptedPdf(_) => ErrorKind::Encrypted,
            Self::Cancelled(_) => ErrorKind::Cancelled,
            Self::ResourceLimit(_) => ErrorKind::ResourceLimit,
        }
    }

    /// Stable snake_case error code for logs, JSON APIs, and FFI bindings.
    pub const fn code(&self) -> &'static str {
        self.kind().code()
    }

    /// True when the caller can usually fix the request/input without retrying
    /// the same bytes unchanged.
    pub const fn is_input_error(&self) -> bool {
        self.kind().is_input_error()
    }
}

pub type OxideResult<T> = std::result::Result<T, OxideError>;
pub type Result<T> = OxideResult<T>;

#[cfg(test)]
mod tests {
    use super::{ErrorKind, OxideError};

    #[test]
    fn error_kind_and_code_are_stable() {
        let err = OxideError::UnsupportedFeature("JBIG2".to_string());
        assert_eq!(err.kind(), ErrorKind::UnsupportedFeature);
        assert_eq!(err.code(), "unsupported_feature");
        assert!(err.is_input_error());
    }

    #[test]
    fn io_is_not_classified_as_input_error() {
        let err = OxideError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"));
        assert_eq!(err.kind(), ErrorKind::Io);
        assert_eq!(err.code(), "io");
        assert!(!err.is_input_error());
    }
}
