use crate::content::tokenizer::ContentToken;

#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Integer(i64),
    Real(f64),
    Boolean(bool),
    Name(String),
    String(Vec<u8>),
    Array(Vec<Operand>),
}

impl Operand {
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_real(&self) -> Option<f64> {
        match self {
            Self::Real(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Integer(value) => Some(*value as f64),
            Self::Real(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&str> {
        match self {
            Self::Name(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Operand]> {
        match self {
            Self::Array(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Boolean(value) => Some(*value),
            _ => None,
        }
    }
}

impl From<ContentToken> for Option<Operand> {
    fn from(value: ContentToken) -> Self {
        match value {
            ContentToken::Integer(value) => Some(Operand::Integer(value)),
            ContentToken::Real(value) => Some(Operand::Real(value)),
            ContentToken::Boolean(value) => Some(Operand::Boolean(value)),
            ContentToken::Name(value) => Some(Operand::Name(value)),
            ContentToken::LiteralString(value) | ContentToken::HexString(value) => {
                Some(Operand::String(value))
            }
            ContentToken::InlineImageData(value) => Some(Operand::String(value)),
            ContentToken::ArrayStart
            | ContentToken::ArrayEnd
            | ContentToken::DictStart
            | ContentToken::DictEnd
            | ContentToken::Operator(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ContentOperation {
    pub operator: String,
    pub operands: Vec<Operand>,
}

impl ContentOperation {
    pub fn new(operator: impl Into<String>, operands: Vec<Operand>) -> Self {
        Self {
            operator: operator.into(),
            operands,
        }
    }

    pub fn operand(&self, n: usize) -> Option<&Operand> {
        self.operands.get(n)
    }

    pub fn number(&self, n: usize) -> Option<f64> {
        self.operand(n).and_then(Operand::as_number)
    }

    pub fn name(&self, n: usize) -> Option<&str> {
        self.operand(n).and_then(Operand::as_name)
    }

    pub fn string_bytes(&self, n: usize) -> Option<&[u8]> {
        self.operand(n).and_then(Operand::as_bytes)
    }
}
