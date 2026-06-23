//! Typed, normalized field values — the layer that makes extracted text
//! *machine-readable*.
//!
//! A raw value like `"$1,234.50"` or `"Jan 15, 2024"` is normalized to a typed
//! [`FieldValue`] (`Amount { value: 1234.50, currency: "USD" }`,
//! `Date("2024-01-15")`). All detection is pure-Rust hand-written scanning (no
//! regex crate, no ML), deterministic, and conservative: when a string does not
//! confidently match a known type it stays [`FieldValue::Text`] rather than
//! being mis-typed.

use serde::{Deserialize, Serialize};

/// A normalized field value. Serializes with an internal `"type"` tag, e.g.
/// `{"type":"amount","value":42.0,"currency":"USD"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldValue {
    /// Unrecognized / free text (the honest fallback).
    Text { text: String },
    /// An ISO-8601 date (`YYYY-MM-DD`).
    Date { iso: String },
    /// A monetary amount: decimal value + ISO-4217-ish currency code when one
    /// could be inferred (else `None`).
    Amount {
        value: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        currency: Option<String>,
    },
    /// A plain number (integer or decimal), thousands separators stripped.
    Number { value: f64 },
    /// A percentage; `value` is the numeric part (e.g. `7.5` for `"7.5%"`).
    Percent { value: f64 },
    /// An email address.
    Email { address: String },
    /// A phone number (digits + a leading `+` preserved; formatting stripped).
    Phone { number: String },
    /// A boolean (checkbox/radio on-off).
    Bool { value: bool },
}

impl FieldValue {
    /// The best-effort plain-text form of any value (for display / fallback).
    pub fn as_text(&self) -> String {
        match self {
            FieldValue::Text { text } => text.clone(),
            FieldValue::Date { iso } => iso.clone(),
            FieldValue::Amount { value, currency } => match currency {
                Some(c) => format!("{value:.2} {c}"),
                None => format!("{value:.2}"),
            },
            FieldValue::Number { value } => trim_num(*value),
            FieldValue::Percent { value } => format!("{}%", trim_num(*value)),
            FieldValue::Email { address } => address.clone(),
            FieldValue::Phone { number } => number.clone(),
            FieldValue::Bool { value } => value.to_string(),
        }
    }

    /// A short type tag (matches the serialized `"type"`), for reports.
    pub fn type_tag(&self) -> &'static str {
        match self {
            FieldValue::Text { .. } => "text",
            FieldValue::Date { .. } => "date",
            FieldValue::Amount { .. } => "amount",
            FieldValue::Number { .. } => "number",
            FieldValue::Percent { .. } => "percent",
            FieldValue::Email { .. } => "email",
            FieldValue::Phone { .. } => "phone",
            FieldValue::Bool { .. } => "bool",
        }
    }
}

fn trim_num(v: f64) -> String {
    // Render without a trailing `.0` for whole numbers.
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v}");
        s
    }
}

/// What kind of value a field is *expected* to be (a profile hint), used to bias
/// normalization toward the right type when a string is ambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueHint {
    Any,
    Date,
    Amount,
    Number,
    Percent,
    Email,
    Phone,
}

/// Normalize a raw string into the best-matching [`FieldValue`], biased by an
/// expected-type `hint`. Pure, deterministic, conservative.
pub fn normalize(raw: &str, hint: ValueHint) -> FieldValue {
    let s = raw.trim();
    if s.is_empty() {
        return FieldValue::Text {
            text: String::new(),
        };
    }

    // When a hint is given, try it first so ambiguous strings resolve the way
    // the profile expects (e.g. a bare "1500" under an Amount field).
    match hint {
        ValueHint::Date => {
            if let Some(iso) = parse_date(s) {
                return FieldValue::Date { iso };
            }
        }
        ValueHint::Amount => {
            if let Some((value, currency)) = parse_amount(s) {
                return FieldValue::Amount { value, currency };
            }
        }
        ValueHint::Percent => {
            if let Some(value) = parse_percent(s) {
                return FieldValue::Percent { value };
            }
        }
        ValueHint::Number => {
            if let Some(value) = parse_number(s) {
                return FieldValue::Number { value };
            }
        }
        ValueHint::Email => {
            if let Some(a) = parse_email(s) {
                return FieldValue::Email { address: a };
            }
        }
        ValueHint::Phone => {
            if let Some(n) = parse_phone(s) {
                return FieldValue::Phone { number: n };
            }
        }
        ValueHint::Any => {}
    }

    // Unhinted (or hint failed): try the discriminating types in an order that
    // avoids false positives — email/percent/amount before bare number.
    if let Some(a) = parse_email(s) {
        return FieldValue::Email { address: a };
    }
    if let Some(value) = parse_percent(s) {
        return FieldValue::Percent { value };
    }
    if let Some(iso) = parse_date(s) {
        return FieldValue::Date { iso };
    }
    // Amount requires a currency cue OR an explicit decimal-with-grouping shape,
    // so a bare integer like "2024" is not mistaken for money.
    if let Some((value, currency)) = parse_amount_strict(s) {
        return FieldValue::Amount { value, currency };
    }
    if let Some(n) = parse_phone_strict(s) {
        return FieldValue::Phone { number: n };
    }
    if let Some(value) = parse_number(s) {
        return FieldValue::Number { value };
    }

    FieldValue::Text {
        text: s.to_string(),
    }
}

// ── dates ────────────────────────────────────────────────────────────────────

/// Parse a date in many common formats to ISO-8601 `YYYY-MM-DD`. Supports:
/// `YYYY-MM-DD`, `YYYY/MM/DD`, `DD/MM/YYYY` & `MM/DD/YYYY` (disambiguated by
/// value where possible), `DD.MM.YYYY`, `D Mon YYYY`, `Mon D, YYYY`,
/// `Month D, YYYY`. Two-digit years map to 2000–2099. Returns `None` if it is
/// not confidently a date.
pub fn parse_date(s: &str) -> Option<String> {
    let s = s.trim();
    // Strip a leading label remnant if any slipped through (rare).
    // Numeric separators: -, /, .
    if let Some(iso) = parse_numeric_date(s) {
        return Some(iso);
    }
    parse_textual_date(s)
}

fn parse_numeric_date(s: &str) -> Option<String> {
    let sep = ['-', '/', '.'];
    let parts: Vec<&str> = s.split(|c| sep.contains(&c)).collect();
    if parts.len() != 3 {
        return None;
    }
    let nums: Vec<i64> = parts
        .iter()
        .map(|p| p.trim().parse::<i64>().ok())
        .collect::<Option<_>>()?;
    let (a, b, c) = (nums[0], nums[1], nums[2]);

    // Decide which field is the year.
    let (year, month, day) = if parts[0].trim().len() == 4 || a > 31 {
        // YYYY M D
        (a, b, c)
    } else if parts[2].trim().len() == 4 || c > 31 {
        // a b YYYY — ambiguous D/M vs M/D. Disambiguate: if a>12 it's the day
        // (DD/MM); if b>12 it's the day (MM/DD); else default to MM/DD (US),
        // the most common in invoices we target.
        let year = c;
        if a > 12 && b <= 12 {
            (year, b, a) // DD/MM/YYYY
        } else {
            (year, a, b) // MM/DD/YYYY (default)
        }
    } else {
        return None;
    };

    let year = normalize_year(year);
    valid_ymd(year, month, day).then(|| format!("{year:04}-{month:02}-{day:02}"))
}

fn parse_textual_date(s: &str) -> Option<String> {
    // Tokenize on spaces and commas.
    let toks: Vec<String> = s
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect();
    if toks.len() < 3 {
        return None;
    }
    let mut day = None;
    let mut month = None;
    let mut year = None;
    for t in &toks {
        let tl = t.to_ascii_lowercase();
        if let Some(m) = month_from_name(&tl) {
            month = Some(m);
        } else if let Ok(n) = t
            .trim_end_matches(|c: char| !c.is_ascii_digit())
            .parse::<i64>()
        {
            if t.len() == 4 && (1900..=2200).contains(&n) {
                year = Some(n);
            } else if (1..=31).contains(&n) && day.is_none() {
                day = Some(n);
            } else if (1900..=2200).contains(&n) {
                year = Some(n);
            }
        }
    }
    let (y, m, d) = (year?, month?, day?);
    let y = normalize_year(y);
    valid_ymd(y, m, d).then(|| format!("{y:04}-{m:02}-{d:02}"))
}

fn month_from_name(t: &str) -> Option<i64> {
    let m = match &t[..t.len().min(3)] {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    };
    Some(m)
}

fn normalize_year(y: i64) -> i64 {
    if y < 100 {
        2000 + y
    } else {
        y
    }
}

fn valid_ymd(y: i64, m: i64, d: i64) -> bool {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(1..=9999).contains(&y) {
        return false;
    }
    let dim = [
        31,
        if leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    d <= dim[(m - 1) as usize]
}

fn leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

// ── amounts / currency ─────────────────────────────────────────────────────

/// Currency symbol/code → ISO-4217 code.
fn currency_code(s: &str) -> Option<&'static str> {
    let t = s.trim();
    let code = match t {
        "$" | "US$" | "USD" => "USD",
        "€" | "EUR" => "EUR",
        "£" | "GBP" => "GBP",
        "¥" | "JPY" => "JPY",
        "₹" | "INR" => "INR",
        "C$" | "CAD" => "CAD",
        "A$" | "AUD" => "AUD",
        "CHF" => "CHF",
        "₩" | "KRW" => "KRW",
        _ => return None,
    };
    Some(code)
}

/// Parse an amount, returning `(value, currency)`. Accepts a leading or trailing
/// currency symbol/code, thousands separators, and either `.` or `,` as the
/// decimal mark. Lenient: a bare number with no currency cue still parses (for
/// the hinted-Amount path).
pub fn parse_amount(s: &str) -> Option<(f64, Option<String>)> {
    let s = s.trim();
    let mut currency: Option<String> = None;

    // A sign may precede the currency symbol ("-$5.00", "($5.00)"). Detect and
    // re-apply it after parsing the magnitude.
    let negative = s.starts_with('-') || (s.starts_with('(') && s.ends_with(')'));
    let s = s
        .trim_start_matches('-')
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();

    // Pull a leading currency token.
    let mut rest = s;
    for sym in ["US$", "C$", "A$", "$", "€", "£", "¥", "₹", "₩"] {
        if let Some(r) = rest.strip_prefix(sym) {
            currency = currency_code(sym).map(|c| c.to_string());
            rest = r.trim_start();
            break;
        }
    }
    // A leading/trailing 3-letter ISO code.
    if currency.is_none() {
        let up = rest.to_ascii_uppercase();
        for code in [
            "USD", "EUR", "GBP", "JPY", "INR", "CAD", "AUD", "CHF", "KRW",
        ] {
            if let Some(r) = up.strip_prefix(code) {
                currency = Some(code.to_string());
                rest = &rest[code.len()..];
                rest = rest.trim_start();
                let _ = r;
                break;
            }
            if let Some(r) = up.strip_suffix(code) {
                currency = Some(code.to_string());
                rest = &rest[..r.len()];
                rest = rest.trim_end();
                break;
            }
        }
    }
    // Trailing symbol (e.g. "42,00 €").
    for sym in ["€", "£", "¥", "₹", "₩", "$"] {
        if let Some(r) = rest.strip_suffix(sym) {
            if currency.is_none() {
                currency = currency_code(sym).map(|c| c.to_string());
            }
            rest = r.trim_end();
            break;
        }
    }

    let value = parse_decimal(rest)?;
    Some((if negative { -value.abs() } else { value }, currency))
}

/// Stricter amount parse for the *unhinted* path: requires a currency cue or a
/// grouped/decimal shape, so a bare integer (e.g. a year or a count) is not
/// swallowed as money.
fn parse_amount_strict(s: &str) -> Option<(f64, Option<String>)> {
    let has_currency_symbol = s
        .chars()
        .any(|c| matches!(c, '$' | '€' | '£' | '¥' | '₹' | '₩'))
        || {
            let up = s.to_ascii_uppercase();
            [
                "USD", "EUR", "GBP", "JPY", "INR", "CAD", "AUD", "CHF", "KRW",
            ]
            .iter()
            .any(|c| up.contains(c))
        };
    let has_decimal = s.contains('.') || s.contains(',');
    if !has_currency_symbol && !has_decimal {
        return None;
    }
    let (value, currency) = parse_amount(s)?;
    // Only treat as amount when there's a currency OR a 2-decimal money shape.
    if currency.is_some() || looks_like_money(s) {
        Some((value, currency))
    } else {
        None
    }
}

fn looks_like_money(s: &str) -> bool {
    // Ends with exactly two fractional digits after the last . or , — the money
    // shape (1,234.50 / 1.234,50). Avoids treating "3.14159" as a price.
    let trimmed: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let last_dot = trimmed.rfind('.');
    let last_comma = trimmed.rfind(',');
    let dec_pos = match (last_dot, last_comma) {
        (Some(d), Some(c)) => Some(d.max(c)),
        (Some(d), None) => Some(d),
        (None, Some(c)) => Some(c),
        (None, None) => None,
    };
    match dec_pos {
        Some(p) => {
            trimmed[p + 1..]
                .chars()
                .filter(|c| c.is_ascii_digit())
                .count()
                == 2
        }
        None => false,
    }
}

/// Parse a numeric string handling both `1,234.50` (US) and `1.234,50` (EU)
/// grouping/decimal conventions into an `f64`.
fn parse_decimal(s: &str) -> Option<f64> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if s.is_empty() {
        return None;
    }
    let neg = s.starts_with('-') || (s.starts_with('(') && s.ends_with(')'));
    let core: String = s
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();
    if core.chars().all(|c| !c.is_ascii_digit()) {
        return None;
    }

    let last_dot = core.rfind('.');
    let last_comma = core.rfind(',');
    // The decimal separator is whichever appears LAST (closest to the end).
    let normalized = match (last_dot, last_comma) {
        (Some(d), Some(c)) => {
            if d > c {
                // '.' is decimal, ',' is grouping → drop commas.
                core.replace(',', "")
            } else {
                // ',' is decimal, '.' is grouping → drop dots, comma→dot.
                core.replace('.', "").replace(',', ".")
            }
        }
        (Some(_), None) => {
            // Only dots. If exactly one dot with 1-2 trailing digits it's decimal;
            // multiple dots ⇒ grouping.
            if core.matches('.').count() > 1 {
                core.replace('.', "")
            } else {
                core
            }
        }
        (None, Some(_)) => {
            if core.matches(',').count() > 1 {
                core.replace(',', "")
            } else {
                // Single comma: treat as decimal mark.
                core.replace(',', ".")
            }
        }
        (None, None) => core,
    };
    let v: f64 = normalized.parse().ok()?;
    Some(if neg { -v } else { v })
}

// ── numbers / percent ──────────────────────────────────────────────────────

pub fn parse_number(s: &str) -> Option<f64> {
    let s = s.trim();
    // Reject anything with letters (so "abc" or "P.O. 12" isn't a number).
    if s.chars().any(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    parse_decimal(s)
}

pub fn parse_percent(s: &str) -> Option<f64> {
    let s = s.trim();
    let stripped = s.strip_suffix('%')?;
    parse_decimal(stripped.trim())
}

// ── email / phone ────────────────────────────────────────────────────────────

pub fn parse_email(s: &str) -> Option<String> {
    let s = s.trim();
    let at = s.find('@')?;
    let (local, domain) = (&s[..at], &s[at + 1..]);
    if local.is_empty() || domain.is_empty() {
        return None;
    }
    // domain must contain a dot with non-empty labels, no spaces anywhere.
    if s.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    let dot = domain.rfind('.')?;
    if dot == 0 || dot == domain.len() - 1 {
        return None;
    }
    Some(s.to_string())
}

/// Lenient phone parse (for the hinted path).
pub fn parse_phone(s: &str) -> Option<String> {
    let s = s.trim();
    let plus = s.starts_with('+');
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 7 || digits.len() > 15 {
        return None;
    }
    // Reject if there are letters (other than common separators).
    if s.chars().any(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some(if plus { format!("+{digits}") } else { digits })
}

/// Stricter phone parse for the unhinted path: requires phone-ish punctuation
/// (parens/dashes/leading +) so a plain 10-digit id isn't read as a phone.
fn parse_phone_strict(s: &str) -> Option<String> {
    let s = s.trim();
    let phoneish = s.starts_with('+')
        || s.contains('(')
        || s.contains(')')
        || s.matches('-').count() >= 2
        || (s.contains('-') && s.contains(' '));
    if !phoneish {
        return None;
    }
    parse_phone(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_normalize_to_iso() {
        assert_eq!(parse_date("2024-01-15").as_deref(), Some("2024-01-15"));
        assert_eq!(parse_date("2024/01/15").as_deref(), Some("2024-01-15"));
        assert_eq!(parse_date("01/15/2024").as_deref(), Some("2024-01-15")); // US MM/DD
        assert_eq!(parse_date("15/01/2024").as_deref(), Some("2024-01-15")); // DD/MM (day>12)
        assert_eq!(parse_date("15.01.2024").as_deref(), Some("2024-01-15"));
        assert_eq!(parse_date("Jan 15, 2024").as_deref(), Some("2024-01-15"));
        assert_eq!(parse_date("15 January 2024").as_deref(), Some("2024-01-15"));
        assert_eq!(parse_date("January 5, 2024").as_deref(), Some("2024-01-05"));
        // Not dates:
        assert_eq!(parse_date("hello"), None);
        assert_eq!(parse_date("13/13/2024"), None); // invalid month
    }

    #[test]
    fn amounts_normalize_with_currency() {
        assert_eq!(parse_amount("$42.00"), Some((42.00, Some("USD".into()))));
        assert_eq!(
            parse_amount("$1,234.50"),
            Some((1234.50, Some("USD".into())))
        );
        assert_eq!(
            parse_amount("€1.234,50"),
            Some((1234.50, Some("EUR".into())))
        );
        assert_eq!(parse_amount("USD 99.99"), Some((99.99, Some("USD".into()))));
        assert_eq!(parse_amount("1500"), Some((1500.0, None)));
        // Negative and parenthesized amounts keep the currency and the sign.
        assert_eq!(parse_amount("-$5.00"), Some((-5.0, Some("USD".into()))));
        assert_eq!(parse_amount("($5.00)"), Some((-5.0, Some("USD".into()))));
    }

    #[test]
    fn normalize_picks_sensible_types() {
        assert!(
            matches!(normalize("$42.00", ValueHint::Any), FieldValue::Amount { value, .. } if value == 42.0)
        );
        assert!(
            matches!(normalize("7.5%", ValueHint::Any), FieldValue::Percent { value } if value == 7.5)
        );
        assert!(matches!(
            normalize("a@b.com", ValueHint::Any),
            FieldValue::Email { .. }
        ));
        assert!(matches!(
            normalize("Jan 15, 2024", ValueHint::Any),
            FieldValue::Date { .. }
        ));
        // A bare integer is a Number, NOT an amount (no currency/decimal cue).
        assert!(
            matches!(normalize("2024", ValueHint::Any), FieldValue::Number { value } if value == 2024.0)
        );
        // Free text stays text.
        assert!(matches!(
            normalize("Acme Corporation", ValueHint::Any),
            FieldValue::Text { .. }
        ));
    }

    #[test]
    fn hint_biases_ambiguous_strings() {
        // "1500" under an Amount field becomes money; under no hint stays Number.
        assert!(
            matches!(normalize("1500", ValueHint::Amount), FieldValue::Amount { value, .. } if value == 1500.0)
        );
        assert!(matches!(
            normalize("1500", ValueHint::Any),
            FieldValue::Number { .. }
        ));
    }

    #[test]
    fn phone_requires_phone_shape_unhinted() {
        assert!(matches!(
            normalize("+1 (555) 123-4567", ValueHint::Any),
            FieldValue::Phone { .. }
        ));
        // A bare 10-digit string unhinted is a Number, not a phone.
        assert!(matches!(
            normalize("5551234567", ValueHint::Any),
            FieldValue::Number { .. }
        ));
        // But with the Phone hint it parses as a phone.
        assert!(matches!(
            normalize("5551234567", ValueHint::Phone),
            FieldValue::Phone { .. }
        ));
    }

    #[test]
    fn decimal_grouping_conventions() {
        assert_eq!(parse_decimal("1,234.50"), Some(1234.50));
        assert_eq!(parse_decimal("1.234,50"), Some(1234.50));
        assert_eq!(parse_decimal("1234"), Some(1234.0));
        assert_eq!(parse_decimal("12,50"), Some(12.50)); // single comma = decimal
    }
}
