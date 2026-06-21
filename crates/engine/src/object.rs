use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PdfDictionary {
    entries: BTreeMap<String, PdfObject>,
}

impl PdfDictionary {
    pub fn new(entries: BTreeMap<String, PdfObject>) -> Self {
        Self { entries }
    }

    pub fn empty() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, key: impl Into<String>, value: PdfObject) -> Option<PdfObject> {
        self.entries.insert(key.into(), value)
    }

    pub fn remove(&mut self, key: &str) -> Option<PdfObject> {
        self.entries.remove(key)
    }

    pub fn get(&self, key: &str) -> Option<&PdfObject> {
        self.entries.get(key)
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &PdfObject)> {
        self.entries.iter()
    }

    pub fn entries(&self) -> impl Iterator<Item = (&String, &PdfObject)> + '_ {
        self.entries.iter()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get_integer(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(PdfObject::as_integer)
    }

    pub fn get_name(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(PdfObject::as_name)
    }

    pub fn get_dict(&self, key: &str) -> Option<&PdfDictionary> {
        self.get(key).and_then(PdfObject::as_dict)
    }

    pub fn get_array(&self, key: &str) -> Option<&[PdfObject]> {
        self.get(key).and_then(PdfObject::as_array)
    }

    pub fn get_reference(&self, key: &str) -> Option<(u32, u16)> {
        self.get(key).and_then(PdfObject::as_reference)
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(PdfObject::as_boolean)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PdfObject {
    Boolean(bool),
    Integer(i64),
    Real(f64),
    String(Vec<u8>),
    Name(String),
    Array(Vec<PdfObject>),
    Dictionary(PdfDictionary),
    Stream { dict: PdfDictionary, raw: Vec<u8> },
    Null,
    Reference { number: u32, generation: u16 },
}

impl PdfObject {
    pub fn as_boolean(&self) -> Option<bool> {
        match self {
            Self::Boolean(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        self.as_boolean()
    }

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

    pub fn as_string(&self) -> Option<&[u8]> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_name(&self) -> Option<&str> {
        match self {
            Self::Name(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[PdfObject]> {
        match self {
            Self::Array(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&PdfDictionary> {
        match self {
            Self::Dictionary(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_stream(&self) -> Option<(&PdfDictionary, &[u8])> {
        match self {
            Self::Stream { dict, raw } => Some((dict, raw)),
            _ => None,
        }
    }

    pub fn as_reference(&self) -> Option<(u32, u16)> {
        match self {
            Self::Reference { number, generation } => Some((*number, *generation)),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    pub fn variant_name(&self) -> &'static str {
        match self {
            Self::Boolean(_) => "Boolean",
            Self::Integer(_) => "Integer",
            Self::Real(_) => "Real",
            Self::String(_) => "String",
            Self::Name(_) => "Name",
            Self::Array(_) => "Array",
            Self::Dictionary(_) => "Dictionary",
            Self::Stream { .. } => "Stream",
            Self::Null => "Null",
            Self::Reference { .. } => "Reference",
        }
    }
}
