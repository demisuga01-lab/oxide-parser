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
}

pub type OxideResult<T> = std::result::Result<T, OxideError>;
pub type Result<T> = OxideResult<T>;
