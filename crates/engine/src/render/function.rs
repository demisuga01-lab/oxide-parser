//! PDF function evaluation (spec §7.10) for shadings, color spaces, and
//! soft-mask transfer functions.
//!
//! Supported function types:
//! - Type 0 (sampled): multi-dimensional sample array with multilinear
//!   interpolation. 1D and 2D inputs are exercised by the corpus; higher input
//!   dimensions are handled by the same generic multilinear code path.
//! - Type 2 (exponential interpolation) and Type 3 (stitching): delegated to the
//!   existing implementations in [`crate::render::shading`].
//! - Type 4 (PostScript calculator): a small stack-based interpreter for the
//!   restricted PostScript subset defined in spec Tables 42–44.
//!
//! The public entry point is `eval_function_n`, which takes a slice of inputs
//! (so 2-input functions used by ShadingType 1 and mesh shadings work), returning
//! the output components. A single-input convenience wrapper lives in
//! `crate::render::shading::eval_function`.

use crate::object::{PdfDictionary, PdfObject};
use crate::reader::PdfReader;
use crate::render::shading::{eval_type2, eval_type3, get_float_array};

/// Evaluate a PDF function with one or more inputs, returning its output
/// components. Returns an empty `Vec` for unsupported types or malformed input
/// (never panics).
pub(crate) fn eval_function_n(func_obj: &PdfObject, inputs: &[f64], reader: &PdfReader) -> Vec<f64> {
    let dict = match resolve_to_dict(func_obj, reader) {
        Some(d) => d,
        None => return Vec::new(),
    };
    match dict.get_integer("FunctionType").unwrap_or(-1) {
        0 => eval_type0(func_obj, &dict, inputs, reader),
        2 => eval_type2(&dict, inputs.first().copied().unwrap_or(0.0)),
        3 => eval_type3(&dict, inputs.first().copied().unwrap_or(0.0), reader),
        4 => eval_type4(func_obj, &dict, inputs, reader),
        other => {
            log::debug!("PDF Function Type {other} not supported");
            Vec::new()
        }
    }
}

/// Resolve a function reference / dict / stream to its dictionary.
fn resolve_to_dict(obj: &PdfObject, reader: &PdfReader) -> Option<PdfDictionary> {
    match obj {
        PdfObject::Dictionary(d) => Some(d.clone()),
        PdfObject::Stream { dict, .. } => Some(dict.clone()),
        PdfObject::Reference { number, generation } => {
            match reader.get_object(*number, *generation).ok()? {
                PdfObject::Dictionary(d) => Some(d),
                PdfObject::Stream { dict, .. } => Some(dict),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Resolve a function object to its decoded stream bytes (Type 0 samples or
/// Type 4 program text), applying any stream filters.
fn resolve_stream_bytes(obj: &PdfObject, reader: &PdfReader) -> Option<Vec<u8>> {
    let stream = match obj {
        PdfObject::Stream { .. } => obj.clone(),
        PdfObject::Reference { number, generation } => {
            match reader.get_object(*number, *generation).ok()? {
                s @ PdfObject::Stream { .. } => s,
                _ => return None,
            }
        }
        _ => return None,
    };
    crate::filters::decode_stream(&stream, reader).ok()
}

// ---------------------------------------------------------------------------
// MSB-first bit reader supporting up to 32-bit fields
// ---------------------------------------------------------------------------

/// Reads big-endian bit fields of width 1..=32 from a byte slice. Shared by
/// Function Type 0 sample unpacking and mesh-shading vertex unpacking.
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Absolute bit position from the start of `data`.
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    /// Read `bits` (1..=32) as an unsigned value. Returns `None` past EOF.
    pub(crate) fn read(&mut self, bits: usize) -> Option<u32> {
        if bits == 0 || bits > 32 {
            return None;
        }
        let mut value: u64 = 0;
        for _ in 0..bits {
            let byte_idx = self.bit_pos / 8;
            let bit_in_byte = 7 - (self.bit_pos % 8);
            let byte = *self.data.get(byte_idx)?;
            let bit = (byte >> bit_in_byte) & 1;
            value = (value << 1) | bit as u64;
            self.bit_pos += 1;
        }
        Some(value as u32)
    }

    /// Discard bits up to the next byte boundary (mesh shadings byte-align each
    /// vertex/flag group per the spec). Used by the mesh-shading vertex reader.
    #[allow(dead_code)]
    pub(crate) fn align_to_byte(&mut self) {
        if !self.bit_pos.is_multiple_of(8) {
            self.bit_pos = (self.bit_pos / 8 + 1) * 8;
        }
    }

    /// Number of unread bits remaining. Used by the mesh-shading vertex reader
    /// to detect the end of the vertex/patch stream.
    #[allow(dead_code)]
    pub(crate) fn bits_remaining(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.bit_pos)
    }
}

/// Maximum unsigned value representable in `bits` bits, as f64.
pub(crate) fn max_value(bits: usize) -> f64 {
    if bits >= 32 {
        u32::MAX as f64
    } else {
        ((1u64 << bits) - 1) as f64
    }
}

// ---------------------------------------------------------------------------
// Type 0: sampled functions
// ---------------------------------------------------------------------------

fn eval_type0(
    func_obj: &PdfObject,
    dict: &PdfDictionary,
    inputs: &[f64],
    reader: &PdfReader,
) -> Vec<f64> {
    let domain = get_float_array(dict, "Domain").unwrap_or_default();
    let range = match get_float_array(dict, "Range") {
        Some(r) if r.len() >= 2 => r,
        _ => {
            log::debug!("Type 0 function: missing /Range");
            return Vec::new();
        }
    };
    let size: Vec<usize> = match dict.get("Size").and_then(PdfObject::as_array) {
        Some(arr) => arr
            .iter()
            .filter_map(|o| o.as_integer())
            .map(|n| n.max(1) as usize)
            .collect(),
        None => {
            log::debug!("Type 0 function: missing /Size");
            return Vec::new();
        }
    };
    let m = size.len(); // number of input dimensions
    let n = range.len() / 2; // number of output components
    if m == 0 || n == 0 || domain.len() < 2 * m {
        return Vec::new();
    }
    let bps = dict.get_integer("BitsPerSample").unwrap_or(8) as usize;
    if !matches!(bps, 1 | 2 | 4 | 8 | 12 | 16 | 24 | 32) {
        log::debug!("Type 0 function: unsupported BitsPerSample {bps}");
        return Vec::new();
    }

    // Encode maps each input domain interval onto sample-index space
    // [0, Size_i - 1]; default is exactly that identity-to-index mapping.
    let encode = get_float_array(dict, "Encode").unwrap_or_else(|| {
        size.iter()
            .flat_map(|&s| [0.0, (s as f64 - 1.0).max(0.0)])
            .collect()
    });
    // Decode maps sample values [0, 2^bps - 1] onto the output range; default
    // equals Range.
    let decode = get_float_array(dict, "Decode").unwrap_or_else(|| range.clone());

    let samples = match resolve_stream_bytes(func_obj, reader) {
        Some(bytes) => bytes,
        None => {
            log::debug!("Type 0 function: could not read sample stream");
            return Vec::new();
        }
    };

    // Encode each input into continuous sample-index coordinates `e_i`.
    let mut e = Vec::with_capacity(m);
    for i in 0..m {
        let x = inputs.get(i).copied().unwrap_or(0.0);
        let dmin = domain[2 * i];
        let dmax = domain[2 * i + 1];
        let emin = encode.get(2 * i).copied().unwrap_or(0.0);
        let emax = encode
            .get(2 * i + 1)
            .copied()
            .unwrap_or((size[i] as f64 - 1.0).max(0.0));
        let x = x.clamp(dmin.min(dmax), dmin.max(dmax));
        let ei = if (dmax - dmin).abs() < 1e-12 {
            emin
        } else {
            emin + (x - dmin) * (emax - emin) / (dmax - dmin)
        };
        e.push(ei.clamp(0.0, (size[i] as f64 - 1.0).max(0.0)));
    }

    let max_sample = max_value(bps);

    // Multilinear interpolation over the 2^m surrounding grid corners.
    let mut out = vec![0.0f64; n];
    let corners = 1usize << m;
    for corner in 0..corners {
        // Build the integer sample index and the interpolation weight for this
        // corner (low/high choice per dimension).
        let mut weight = 1.0f64;
        let mut idx = vec![0usize; m];
        for i in 0..m {
            let lo = e[i].floor();
            let frac = e[i] - lo;
            let take_high = (corner >> i) & 1 == 1;
            let coord = if take_high {
                (lo as usize + 1).min(size[i].saturating_sub(1))
            } else {
                lo as usize
            };
            idx[i] = coord.min(size[i].saturating_sub(1));
            weight *= if take_high { frac } else { 1.0 - frac };
        }
        if weight == 0.0 {
            continue;
        }
        // Flat sample offset: dimension 0 varies fastest (spec §7.10.2).
        let mut flat = 0usize;
        let mut stride = 1usize;
        for i in 0..m {
            flat += idx[i] * stride;
            stride *= size[i];
        }
        for (j, slot) in out.iter_mut().enumerate() {
            let sample_index = flat * n + j;
            let raw = read_sample(&samples, sample_index, bps).unwrap_or(0.0);
            // Decode raw [0, max] -> [decode_lo, decode_hi].
            let dlo = decode.get(2 * j).copied().unwrap_or(0.0);
            let dhi = decode.get(2 * j + 1).copied().unwrap_or(1.0);
            let val = dlo + (raw / max_sample) * (dhi - dlo);
            *slot += weight * val;
        }
    }

    // Clamp to Range.
    for (j, slot) in out.iter_mut().enumerate() {
        let rlo = range[2 * j];
        let rhi = range[2 * j + 1];
        *slot = slot.clamp(rlo.min(rhi), rlo.max(rhi));
    }
    out
}

/// Read the `index`-th sample (each `bps` bits) from the packed sample stream.
fn read_sample(data: &[u8], index: usize, bps: usize) -> Option<f64> {
    let bit_start = index.checked_mul(bps)?;
    let mut reader = BitReader::new(data);
    // Fast-forward to the sample's bit offset.
    reader.bit_pos = bit_start;
    reader.read(bps).map(|v| v as f64)
}

// ---------------------------------------------------------------------------
// Type 4: PostScript calculator functions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum PsToken {
    Num(f64),
    Op(String),
    ProcStart,
    ProcEnd,
}

/// A parsed value on the PostScript operand stack: a number, a boolean, or a
/// deferred procedure (block of tokens) used by `if`/`ifelse`.
#[derive(Debug, Clone)]
enum PsValue {
    Num(f64),
    Bool(bool),
    Proc(Vec<PsToken>),
}

fn eval_type4(
    func_obj: &PdfObject,
    dict: &PdfDictionary,
    inputs: &[f64],
    reader: &PdfReader,
) -> Vec<f64> {
    let range = match get_float_array(dict, "Range") {
        Some(r) if r.len() >= 2 => r,
        _ => {
            log::debug!("Type 4 function: missing /Range");
            return Vec::new();
        }
    };
    let n = range.len() / 2;

    let program_bytes = match resolve_stream_bytes(func_obj, reader) {
        Some(bytes) => bytes,
        None => {
            log::debug!("Type 4 function: could not read program stream");
            return Vec::new();
        }
    };
    let text = String::from_utf8_lossy(&program_bytes);
    let tokens = tokenize_ps(&text);

    // The outermost `{ ... }` wraps the whole program; strip it so we execute
    // the body directly.
    let body = strip_outer_proc(&tokens);

    let mut stack: Vec<PsValue> = inputs.iter().map(|&v| PsValue::Num(v)).collect();
    if exec_ps(&body, &mut stack, 0).is_err() {
        log::debug!("Type 4 function: execution error");
        return Vec::new();
    }

    // The top `n` numbers on the stack are the outputs (bottom-to-top order).
    let nums: Vec<f64> = stack
        .iter()
        .filter_map(|v| match v {
            PsValue::Num(x) => Some(*x),
            PsValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            PsValue::Proc(_) => None,
        })
        .collect();
    if nums.len() < n {
        return Vec::new();
    }
    let start = nums.len() - n;
    nums[start..]
        .iter()
        .enumerate()
        .map(|(j, &v)| {
            let rlo = range[2 * j];
            let rhi = range[2 * j + 1];
            v.clamp(rlo.min(rhi), rlo.max(rhi))
        })
        .collect()
}

fn tokenize_ps(text: &str) -> Vec<PsToken> {
    let mut tokens = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '{' => {
                tokens.push(PsToken::ProcStart);
                chars.next();
            }
            '}' => {
                tokens.push(PsToken::ProcEnd);
                chars.next();
            }
            c if c.is_whitespace() => {
                chars.next();
            }
            '%' => {
                // Comment to end of line.
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c == '\n' || c == '\r' {
                        break;
                    }
                }
            }
            _ => {
                let mut word = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || c == '{' || c == '}' || c == '%' {
                        break;
                    }
                    word.push(c);
                    chars.next();
                }
                if let Ok(num) = word.parse::<f64>() {
                    tokens.push(PsToken::Num(num));
                } else {
                    tokens.push(PsToken::Op(word));
                }
            }
        }
    }
    tokens
}

/// If the token stream is a single `{ ... }` block, return its inner tokens;
/// otherwise return the tokens unchanged.
fn strip_outer_proc(tokens: &[PsToken]) -> Vec<PsToken> {
    if matches!(tokens.first(), Some(PsToken::ProcStart))
        && matches!(tokens.last(), Some(PsToken::ProcEnd))
    {
        // Confirm the first ProcStart matches the last ProcEnd (balanced).
        tokens[1..tokens.len() - 1].to_vec()
    } else {
        tokens.to_vec()
    }
}

/// Collect a balanced procedure body starting just after a `ProcStart`. Returns
/// (body tokens, index just past the matching ProcEnd).
fn collect_proc(tokens: &[PsToken], start: usize) -> Option<(Vec<PsToken>, usize)> {
    let mut depth = 1;
    let mut body = Vec::new();
    let mut i = start;
    while i < tokens.len() {
        match &tokens[i] {
            PsToken::ProcStart => {
                depth += 1;
                body.push(tokens[i].clone());
            }
            PsToken::ProcEnd => {
                depth -= 1;
                if depth == 0 {
                    return Some((body, i + 1));
                }
                body.push(tokens[i].clone());
            }
            t => body.push(t.clone()),
        }
        i += 1;
    }
    None
}

const PS_MAX_DEPTH: usize = 64;

fn exec_ps(tokens: &[PsToken], stack: &mut Vec<PsValue>, depth: usize) -> Result<(), ()> {
    if depth > PS_MAX_DEPTH {
        return Err(());
    }
    let mut i = 0;
    while i < tokens.len() {
        match &tokens[i] {
            PsToken::Num(x) => stack.push(PsValue::Num(*x)),
            PsToken::ProcStart => {
                let (body, next) = collect_proc(tokens, i + 1).ok_or(())?;
                stack.push(PsValue::Proc(body));
                i = next;
                continue;
            }
            PsToken::ProcEnd => return Err(()),
            PsToken::Op(name) => exec_ps_op(name, stack, depth)?,
        }
        i += 1;
    }
    Ok(())
}

fn pop_num(stack: &mut Vec<PsValue>) -> Result<f64, ()> {
    match stack.pop() {
        Some(PsValue::Num(x)) => Ok(x),
        Some(PsValue::Bool(b)) => Ok(if b { 1.0 } else { 0.0 }),
        _ => Err(()),
    }
}

fn pop_bool(stack: &mut Vec<PsValue>) -> Result<bool, ()> {
    match stack.pop() {
        Some(PsValue::Bool(b)) => Ok(b),
        Some(PsValue::Num(x)) => Ok(x != 0.0),
        _ => Err(()),
    }
}

fn pop_proc(stack: &mut Vec<PsValue>) -> Result<Vec<PsToken>, ()> {
    match stack.pop() {
        Some(PsValue::Proc(p)) => Ok(p),
        _ => Err(()),
    }
}

#[allow(clippy::too_many_lines)]
fn exec_ps_op(name: &str, stack: &mut Vec<PsValue>, depth: usize) -> Result<(), ()> {
    match name {
        // Arithmetic
        "add" => {
            let b = pop_num(stack)?;
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(a + b));
        }
        "sub" => {
            let b = pop_num(stack)?;
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(a - b));
        }
        "mul" => {
            let b = pop_num(stack)?;
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(a * b));
        }
        "div" => {
            let b = pop_num(stack)?;
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(if b == 0.0 { 0.0 } else { a / b }));
        }
        "idiv" => {
            let b = pop_num(stack)? as i64;
            let a = pop_num(stack)? as i64;
            stack.push(PsValue::Num(if b == 0 { 0.0 } else { (a / b) as f64 }));
        }
        "mod" => {
            let b = pop_num(stack)? as i64;
            let a = pop_num(stack)? as i64;
            stack.push(PsValue::Num(if b == 0 { 0.0 } else { (a % b) as f64 }));
        }
        "neg" => {
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(-a));
        }
        "abs" => {
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(a.abs()));
        }
        "sqrt" => {
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(a.max(0.0).sqrt()));
        }
        "sin" => {
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(a.to_radians().sin()));
        }
        "cos" => {
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(a.to_radians().cos()));
        }
        "atan" => {
            let den = pop_num(stack)?;
            let num = pop_num(stack)?;
            let mut deg = num.atan2(den).to_degrees();
            if deg < 0.0 {
                deg += 360.0;
            }
            stack.push(PsValue::Num(deg));
        }
        "exp" => {
            let exp = pop_num(stack)?;
            let base = pop_num(stack)?;
            stack.push(PsValue::Num(base.powf(exp)));
        }
        "ln" => {
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(if a > 0.0 { a.ln() } else { 0.0 }));
        }
        "log" => {
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(if a > 0.0 { a.log10() } else { 0.0 }));
        }
        "cvi" | "truncate" => {
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(a.trunc()));
        }
        "cvr" => { /* numbers are already real; no-op */ }
        "floor" => {
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(a.floor()));
        }
        "ceiling" => {
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(a.ceil()));
        }
        "round" => {
            let a = pop_num(stack)?;
            stack.push(PsValue::Num(a.round()));
        }
        // Stack manipulation
        "dup" => {
            let a = stack.last().cloned().ok_or(())?;
            stack.push(a);
        }
        "pop" => {
            stack.pop().ok_or(())?;
        }
        "exch" => {
            let n = stack.len();
            if n < 2 {
                return Err(());
            }
            stack.swap(n - 1, n - 2);
        }
        "copy" => {
            let count = pop_num(stack)? as i64;
            if count < 0 || count as usize > stack.len() {
                return Err(());
            }
            let count = count as usize;
            let start = stack.len() - count;
            for k in 0..count {
                stack.push(stack[start + k].clone());
            }
        }
        "index" => {
            let n = pop_num(stack)? as i64;
            if n < 0 || n as usize >= stack.len() {
                return Err(());
            }
            let v = stack[stack.len() - 1 - n as usize].clone();
            stack.push(v);
        }
        "roll" => {
            let j = pop_num(stack)? as i64;
            let n = pop_num(stack)? as i64;
            if n < 0 || n as usize > stack.len() {
                return Err(());
            }
            let n = n as usize;
            if n == 0 {
                return Ok(());
            }
            let start = stack.len() - n;
            let slice = &mut stack[start..];
            let shift = ((j % n as i64) + n as i64) as usize % n;
            slice.rotate_right(shift);
        }
        // Comparison / boolean
        "eq" => bin_bool(stack, |a, b| a == b)?,
        "ne" => bin_bool(stack, |a, b| a != b)?,
        "gt" => bin_bool(stack, |a, b| a > b)?,
        "ge" => bin_bool(stack, |a, b| a >= b)?,
        "lt" => bin_bool(stack, |a, b| a < b)?,
        "le" => bin_bool(stack, |a, b| a <= b)?,
        "and" => {
            let b = pop_num(stack)? as i64;
            let a = pop_num(stack)? as i64;
            stack.push(PsValue::Num((a & b) as f64));
        }
        "or" => {
            let b = pop_num(stack)? as i64;
            let a = pop_num(stack)? as i64;
            stack.push(PsValue::Num((a | b) as f64));
        }
        "xor" => {
            let b = pop_num(stack)? as i64;
            let a = pop_num(stack)? as i64;
            stack.push(PsValue::Num((a ^ b) as f64));
        }
        "not" => match stack.pop() {
            Some(PsValue::Bool(b)) => stack.push(PsValue::Bool(!b)),
            Some(PsValue::Num(x)) => stack.push(PsValue::Num(!(x as i64) as f64)),
            _ => return Err(()),
        },
        "bitshift" => {
            let shift = pop_num(stack)? as i64;
            let a = pop_num(stack)? as i64;
            let v = if shift >= 0 {
                a << (shift.min(63))
            } else {
                a >> ((-shift).min(63))
            };
            stack.push(PsValue::Num(v as f64));
        }
        "true" => stack.push(PsValue::Bool(true)),
        "false" => stack.push(PsValue::Bool(false)),
        // Conditionals
        "if" => {
            let proc = pop_proc(stack)?;
            let cond = pop_bool(stack)?;
            if cond {
                exec_ps(&proc, stack, depth + 1)?;
            }
        }
        "ifelse" => {
            let proc2 = pop_proc(stack)?;
            let proc1 = pop_proc(stack)?;
            let cond = pop_bool(stack)?;
            if cond {
                exec_ps(&proc1, stack, depth + 1)?;
            } else {
                exec_ps(&proc2, stack, depth + 1)?;
            }
        }
        _ => {
            log::debug!("Type 4 function: unknown operator '{name}'");
            return Err(());
        }
    }
    Ok(())
}

fn bin_bool(stack: &mut Vec<PsValue>, f: impl Fn(f64, f64) -> bool) -> Result<(), ()> {
    let b = pop_num(stack)?;
    let a = pop_num(stack)?;
    stack.push(PsValue::Bool(f(a, b)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Type 4 tokenizer / interpreter --------------------------------

    fn run_ps(program: &str, inputs: &[f64], n_outputs: usize) -> Vec<f64> {
        let tokens = tokenize_ps(program);
        let body = strip_outer_proc(&tokens);
        let mut stack: Vec<PsValue> = inputs.iter().map(|&v| PsValue::Num(v)).collect();
        exec_ps(&body, &mut stack, 0).unwrap();
        let nums: Vec<f64> = stack
            .iter()
            .filter_map(|v| match v {
                PsValue::Num(x) => Some(*x),
                PsValue::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
                PsValue::Proc(_) => None,
            })
            .collect();
        let start = nums.len() - n_outputs;
        nums[start..].to_vec()
    }

    #[test]
    fn ps_tokenizes_proc_blocks() {
        let toks = tokenize_ps("{ 2 copy gt { exch } if pop }");
        // Expect: { 2 copy gt { exch } if pop }
        assert!(matches!(toks[0], PsToken::ProcStart));
        assert!(matches!(toks[1], PsToken::Num(n) if (n - 2.0).abs() < 1e-9));
        assert!(matches!(&toks[2], PsToken::Op(o) if o == "copy"));
        assert!(matches!(&toks[3], PsToken::Op(o) if o == "gt"));
        assert!(matches!(toks[4], PsToken::ProcStart));
        assert!(matches!(&toks[5], PsToken::Op(o) if o == "exch"));
        assert!(matches!(toks[6], PsToken::ProcEnd));
        assert!(matches!(&toks[7], PsToken::Op(o) if o == "if"));
    }

    #[test]
    fn ps_basic_arithmetic() {
        assert_eq!(run_ps("{ 2 3 add }", &[], 1), vec![5.0]);
        assert_eq!(run_ps("{ 10 3 sub }", &[], 1), vec![7.0]);
        assert_eq!(run_ps("{ 4 5 mul }", &[], 1), vec![20.0]);
        assert!((run_ps("{ 9 sqrt }", &[], 1)[0] - 3.0).abs() < 1e-9);
    }

    #[test]
    fn ps_stack_exch() {
        let r = run_ps("{ 1 2 exch }", &[], 2);
        assert_eq!(r, vec![2.0, 1.0]);
    }

    #[test]
    fn ps_conditional_true_and_false() {
        assert_eq!(run_ps("{ true { 1 } { 2 } ifelse }", &[], 1), vec![1.0]);
        assert_eq!(run_ps("{ false { 1 } { 2 } ifelse }", &[], 1), vec![2.0]);
    }

    #[test]
    fn ps_if_executes_only_when_true() {
        // Keep a base value on the stack, then conditionally push 99.
        // 7 5 gt is true -> { 99 } runs -> stack [10, 99] -> top 2 outputs.
        assert_eq!(run_ps("{ 10 7 5 gt { 99 } if }", &[], 2), vec![10.0, 99.0]);
        // 5 7 gt is false -> { 99 } skipped -> stack [10] -> single output.
        assert_eq!(run_ps("{ 10 5 7 gt { 99 } if }", &[], 1), vec![10.0]);
    }

    #[test]
    fn ps_separation_tint_transform_style() {
        // A realistic Separation tint transform: input t -> CMYK linear blend.
        // { dup 0.1 mul exch 0.9 mul } on input 0.5 -> [0.05, 0.45].
        let r = run_ps("{ dup 0.1 mul exch 0.9 mul }", &[0.5], 2);
        assert!((r[0] - 0.05).abs() < 1e-9, "{:?}", r);
        assert!((r[1] - 0.45).abs() < 1e-9, "{:?}", r);
    }

    #[test]
    fn ps_roll_rotates() {
        // 1 2 3  3 1 roll -> 3 1 2
        let r = run_ps("{ 1 2 3 3 1 roll }", &[], 3);
        assert_eq!(r, vec![3.0, 1.0, 2.0]);
    }

    #[test]
    fn ps_index_copies_nth() {
        // 10 20 30  2 index -> 10 20 30 10
        let r = run_ps("{ 10 20 30 2 index }", &[], 4);
        assert_eq!(r, vec![10.0, 20.0, 30.0, 10.0]);
    }

    // ---- BitReader -----------------------------------------------------

    #[test]
    fn bit_reader_reads_8bit() {
        let mut br = BitReader::new(&[0xAB, 0xCD]);
        assert_eq!(br.read(8), Some(0xAB));
        assert_eq!(br.read(8), Some(0xCD));
        assert_eq!(br.read(8), None);
    }

    #[test]
    fn bit_reader_reads_16bit() {
        let mut br = BitReader::new(&[0x12, 0x34, 0xFF, 0xFF]);
        assert_eq!(br.read(16), Some(0x1234));
        assert_eq!(br.read(16), Some(0xFFFF));
    }

    #[test]
    fn bit_reader_reads_sub_byte_fields() {
        // 0b1011_0010 -> read 2,3,3 = 0b10, 0b110, 0b010
        let mut br = BitReader::new(&[0b1011_0010]);
        assert_eq!(br.read(2), Some(0b10));
        assert_eq!(br.read(3), Some(0b110));
        assert_eq!(br.read(3), Some(0b010));
    }

    #[test]
    fn read_sample_8bit() {
        let data = [0u8, 128, 255];
        assert_eq!(read_sample(&data, 0, 8), Some(0.0));
        assert_eq!(read_sample(&data, 1, 8), Some(128.0));
        assert_eq!(read_sample(&data, 2, 8), Some(255.0));
    }

    // ---- Type 0 sampled functions (end-to-end via a stream object) -----

    fn reader_for_tests() -> crate::reader::PdfReader {
        crate::reader::PdfReader::from_bytes(crate::render::shading::tests_minimal_pdf()).unwrap()
    }

    fn type0_stream(
        size: &[i64],
        bps: i64,
        domain: &[f64],
        range: &[f64],
        samples: Vec<u8>,
    ) -> PdfObject {
        use std::collections::BTreeMap;
        let mut m: BTreeMap<String, PdfObject> = BTreeMap::new();
        m.insert("FunctionType".into(), PdfObject::Integer(0));
        m.insert(
            "Size".into(),
            PdfObject::Array(size.iter().map(|&s| PdfObject::Integer(s)).collect()),
        );
        m.insert("BitsPerSample".into(), PdfObject::Integer(bps));
        m.insert(
            "Domain".into(),
            PdfObject::Array(domain.iter().map(|&v| PdfObject::Real(v)).collect()),
        );
        m.insert(
            "Range".into(),
            PdfObject::Array(range.iter().map(|&v| PdfObject::Real(v)).collect()),
        );
        m.insert("Length".into(), PdfObject::Integer(samples.len() as i64));
        PdfObject::Stream {
            dict: PdfDictionary::new(m),
            raw: samples,
        }
    }

    #[test]
    fn type0_1d_exact_at_sample_points() {
        // 4 samples over Domain [0,1] -> Range [0,1]: 0, 85, 170, 255 (8-bit).
        let obj = type0_stream(&[4], 8, &[0.0, 1.0], &[0.0, 1.0], vec![0, 85, 170, 255]);
        let r = reader_for_tests();
        // At t=0 -> sample 0 -> 0.0; t=1 -> sample 3 -> 1.0.
        assert!((eval_function_n(&obj, &[0.0], &r)[0] - 0.0).abs() < 0.01);
        assert!((eval_function_n(&obj, &[1.0], &r)[0] - 1.0).abs() < 0.01);
        // t=1/3 -> sample index 1 -> 85/255 ≈ 0.333.
        let v = eval_function_n(&obj, &[1.0 / 3.0], &r)[0];
        assert!((v - 85.0 / 255.0).abs() < 0.02, "v={v}");
    }

    #[test]
    fn type0_1d_linear_interpolation_at_midpoint() {
        // 2 samples: 0 and 255 over [0,1]. Midpoint t=0.5 -> 0.5.
        let obj = type0_stream(&[2], 8, &[0.0, 1.0], &[0.0, 1.0], vec![0, 255]);
        let r = reader_for_tests();
        let v = eval_function_n(&obj, &[0.5], &r)[0];
        assert!((v - 0.5).abs() < 0.01, "midpoint interp v={v}");
    }

    #[test]
    fn type0_2d_bilinear_interpolation() {
        // 2x2 grid, single output. Samples (dim0 fastest):
        //   (0,0)=0  (1,0)=255  (0,1)=255  (1,1)=0
        // Center (0.5,0.5) bilinear = (0+255+255+0)/4 = 127.5 -> 0.5.
        let obj = type0_stream(
            &[2, 2],
            8,
            &[0.0, 1.0, 0.0, 1.0],
            &[0.0, 1.0],
            vec![0, 255, 255, 0],
        );
        let r = reader_for_tests();
        let v = eval_function_n(&obj, &[0.5, 0.5], &r)[0];
        assert!((v - 0.5).abs() < 0.02, "bilinear center v={v}");
        // Corner (0,0) -> exactly 0.
        assert!((eval_function_n(&obj, &[0.0, 0.0], &r)[0]).abs() < 0.01);
        // Corner (1,0) -> exactly 255 -> 1.0.
        assert!((eval_function_n(&obj, &[1.0, 0.0], &r)[0] - 1.0).abs() < 0.01);
    }

    #[test]
    fn type0_16bit_samples() {
        // 2 samples, 16-bit: 0x0000 and 0xFFFF over [0,1].
        let obj = type0_stream(&[2], 16, &[0.0, 1.0], &[0.0, 1.0], vec![0x00, 0x00, 0xFF, 0xFF]);
        let r = reader_for_tests();
        assert!((eval_function_n(&obj, &[0.0], &r)[0]).abs() < 0.01);
        assert!((eval_function_n(&obj, &[1.0], &r)[0] - 1.0).abs() < 0.01);
    }
}
