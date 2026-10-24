//! PDF functions: sampled (0), exponential (2), stitching (3) and PostScript calculator (4).
use crate::doc::Pdf;
use crate::object::{Dict, Obj};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum Function {
    Sampled {
        domain: Vec<f64>,
        range: Vec<f64>,
        size: Vec<usize>,
        encode: Vec<f64>,
        decode: Vec<f64>,
        samples: Arc<Vec<f32>>,
        n_out: usize,
    },
    Exp {
        domain: Vec<f64>,
        range: Vec<f64>,
        c0: Vec<f64>,
        c1: Vec<f64>,
        n: f64,
    },
    Stitch {
        domain: Vec<f64>,
        range: Vec<f64>,
        funcs: Vec<Function>,
        bounds: Vec<f64>,
        encode: Vec<f64>,
    },
    Ps {
        domain: Vec<f64>,
        range: Vec<f64>,
        code: Arc<Vec<Ps>>,
    },
    /// One single-output function per component.
    Multi(Vec<Function>),
    Identity,
}

#[derive(Debug, Clone)]
pub enum Ps {
    Num(f64),
    Op(&'static str),
    If(Vec<Ps>),
    IfElse(Vec<Ps>, Vec<Ps>),
}

const OPS: &[&str] = &[
    "abs", "add", "atan", "ceiling", "cos", "cvi", "cvr", "div", "exp", "floor", "idiv", "ln",
    "log", "mod", "mul", "neg", "round", "sin", "sqrt", "sub", "truncate", "and", "bitshift", "eq",
    "false", "ge", "gt", "le", "lt", "ne", "not", "or", "true", "xor", "copy", "dup", "exch",
    "index", "pop", "roll",
];

fn parse_ps(tokens: &mut std::iter::Peekable<std::vec::IntoIter<String>>, depth: u32) -> Vec<Ps> {
    let mut out: Vec<Ps> = Vec::new();
    let mut blocks: Vec<Vec<Ps>> = Vec::new();
    while let Some(t) = tokens.next() {
        match t.as_str() {
            "{" => {
                if depth < 32 {
                    blocks.push(parse_ps(tokens, depth + 1));
                }
            }
            "}" => break,
            "if" => {
                if let Some(b) = blocks.pop() {
                    out.push(Ps::If(b));
                }
            }
            "ifelse" => {
                let b2 = blocks.pop();
                let b1 = blocks.pop();
                if let (Some(a), Some(b)) = (b1, b2) {
                    out.push(Ps::IfElse(a, b));
                }
            }
            other => {
                if let Ok(v) = other.parse::<f64>() {
                    out.push(Ps::Num(v));
                } else if let Some(op) = OPS.iter().find(|o| **o == other) {
                    out.push(Ps::Op(op));
                }
            }
        }
    }
    out
}

fn tokenize_ps(data: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(data);
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        match c {
            '{' | '}' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push(c.to_string());
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn run_ps(code: &[Ps], st: &mut Vec<f64>, depth: u32) {
    if depth > 32 {
        return;
    }
    for item in code {
        if st.len() > 1000 {
            return;
        }
        match item {
            Ps::Num(v) => st.push(*v),
            Ps::If(b) => {
                if st.pop().is_some_and(|c| c != 0.0) {
                    run_ps(b, st, depth + 1);
                }
            }
            Ps::IfElse(a, b) => {
                let c = st.pop().unwrap_or(0.0);
                run_ps(if c != 0.0 { a } else { b }, st, depth + 1);
            }
            Ps::Op(op) => ps_op(op, st),
        }
    }
}

fn ps_op(op: &str, st: &mut Vec<f64>) {
    let pop = |st: &mut Vec<f64>| st.pop().unwrap_or(0.0);
    let b2f = |b: bool| if b { 1.0 } else { 0.0 };
    match op {
        "abs" => {
            let a = pop(st);
            st.push(a.abs())
        }
        "add" => {
            let b = pop(st);
            let a = pop(st);
            st.push(a + b)
        }
        "sub" => {
            let b = pop(st);
            let a = pop(st);
            st.push(a - b)
        }
        "mul" => {
            let b = pop(st);
            let a = pop(st);
            st.push(a * b)
        }
        "div" => {
            let b = pop(st);
            let a = pop(st);
            st.push(if b == 0.0 { 0.0 } else { a / b })
        }
        "idiv" => {
            let b = pop(st) as i64;
            let a = pop(st) as i64;
            st.push(if b == 0 { 0.0 } else { (a / b) as f64 })
        }
        "mod" => {
            let b = pop(st) as i64;
            let a = pop(st) as i64;
            st.push(if b == 0 { 0.0 } else { (a % b) as f64 })
        }
        "neg" => {
            let a = pop(st);
            st.push(-a)
        }
        "atan" => {
            let den = pop(st);
            let num = pop(st);
            let mut d = num.atan2(den).to_degrees();
            if d < 0.0 {
                d += 360.0
            }
            st.push(d)
        }
        "ceiling" => {
            let a = pop(st);
            st.push(a.ceil())
        }
        "floor" => {
            let a = pop(st);
            st.push(a.floor())
        }
        "round" => {
            let a = pop(st);
            st.push((a + 0.5).floor())
        }
        "truncate" | "cvi" => {
            let a = pop(st);
            st.push(a.trunc())
        }
        "cvr" => {}
        "cos" => {
            let a = pop(st);
            st.push(a.to_radians().cos())
        }
        "sin" => {
            let a = pop(st);
            st.push(a.to_radians().sin())
        }
        "exp" => {
            let e = pop(st);
            let b = pop(st);
            st.push(b.powf(e))
        }
        "ln" => {
            let a = pop(st);
            st.push(if a > 0.0 { a.ln() } else { 0.0 })
        }
        "log" => {
            let a = pop(st);
            st.push(if a > 0.0 { a.log10() } else { 0.0 })
        }
        "sqrt" => {
            let a = pop(st);
            st.push(a.max(0.0).sqrt())
        }
        "and" => {
            let b = pop(st) as i64;
            let a = pop(st) as i64;
            st.push((a & b) as f64)
        }
        "or" => {
            let b = pop(st) as i64;
            let a = pop(st) as i64;
            st.push((a | b) as f64)
        }
        "xor" => {
            let b = pop(st) as i64;
            let a = pop(st) as i64;
            st.push((a ^ b) as f64)
        }
        "not" => {
            let a = pop(st);
            st.push(if a == 0.0 || a == 1.0 {
                1.0 - a
            } else {
                !(a as i64) as f64
            })
        }
        "bitshift" => {
            let s = pop(st) as i64;
            let a = pop(st) as i64;
            st.push(if s >= 0 {
                a.checked_shl(s.min(63) as u32).unwrap_or(0)
            } else {
                a >> (-s).min(63)
            } as f64)
        }
        "eq" => {
            let b = pop(st);
            let a = pop(st);
            st.push(b2f(a == b))
        }
        "ne" => {
            let b = pop(st);
            let a = pop(st);
            st.push(b2f(a != b))
        }
        "ge" => {
            let b = pop(st);
            let a = pop(st);
            st.push(b2f(a >= b))
        }
        "gt" => {
            let b = pop(st);
            let a = pop(st);
            st.push(b2f(a > b))
        }
        "le" => {
            let b = pop(st);
            let a = pop(st);
            st.push(b2f(a <= b))
        }
        "lt" => {
            let b = pop(st);
            let a = pop(st);
            st.push(b2f(a < b))
        }
        "true" => st.push(1.0),
        "false" => st.push(0.0),
        "dup" => {
            if let Some(a) = st.last().copied() {
                st.push(a)
            }
        }
        "pop" => {
            st.pop();
        }
        "exch" => {
            let n = st.len();
            if n >= 2 {
                st.swap(n - 1, n - 2)
            }
        }
        "copy" => {
            let n = pop(st).max(0.0) as usize;
            if n <= st.len() && n < 500 {
                let from = st.len() - n;
                let v: Vec<f64> = st[from..].to_vec();
                st.extend(v);
            }
        }
        "index" => {
            let n = pop(st).max(0.0) as usize;
            if n < st.len() {
                st.push(st[st.len() - 1 - n]);
            }
        }
        "roll" => {
            let j = pop(st) as i64;
            let n = pop(st).max(0.0) as usize;
            if n > 0 && n <= st.len() {
                let from = st.len() - n;
                let j = j.rem_euclid(n as i64) as usize;
                st[from..].rotate_right(j);
            }
        }
        _ => {}
    }
}

fn nums(pdf: &Pdf, d: &Dict, key: &str) -> Vec<f64> {
    pdf.dget_numbers(d, key).unwrap_or_default()
}

impl Function {
    /// Parse a function object (or an array of single-output functions).
    pub fn parse(pdf: &Pdf, o: &Obj) -> Option<Function> {
        Self::parse_depth(pdf, o, 0)
    }

    fn parse_depth(pdf: &Pdf, o: &Obj, depth: u32) -> Option<Function> {
        if depth > 8 {
            return None;
        }
        let o = pdf.resolve(o);
        if let Obj::Array(a) = &o {
            let fs: Vec<Function> = a
                .iter()
                .filter_map(|x| Self::parse_depth(pdf, x, depth + 1))
                .collect();
            return (!fs.is_empty()).then_some(Function::Multi(fs));
        }
        if o.as_name() == Some("Identity") {
            return Some(Function::Identity);
        }
        let d = o.as_dict()?.clone();
        let domain = nums(pdf, &d, "Domain");
        let range = nums(pdf, &d, "Range");
        match pdf.dget_i64(&d, "FunctionType")? {
            0 => {
                let Obj::Stream(s) = &o else { return None };
                let data = crate::filter::decode_stream(pdf, s, false).ok()?.0;
                let size: Vec<usize> = nums(pdf, &d, "Size")
                    .iter()
                    .map(|v| (*v as usize).clamp(1, 1 << 16))
                    .collect();
                let bps = pdf.dget_i64(&d, "BitsPerSample")?.clamp(1, 32) as usize;
                let m = domain.len() / 2;
                let n_out = range.len() / 2;
                if m == 0 || n_out == 0 || size.len() != m {
                    return None;
                }
                let total = size
                    .iter()
                    .try_fold(1usize, |a, b| a.checked_mul(*b))?
                    .checked_mul(n_out)?;
                if total > 1 << 26 {
                    return None;
                }
                let maxv = ((1u64 << bps) - 1) as f32;
                let mut samples = Vec::with_capacity(total);
                let mut bitpos = 0usize;
                for _ in 0..total {
                    let mut v = 0u64;
                    for _ in 0..bps {
                        let byte = data.get(bitpos / 8).copied().unwrap_or(0);
                        v = (v << 1) | u64::from((byte >> (7 - bitpos % 8)) & 1);
                        bitpos += 1;
                    }
                    samples.push(v as f32 / maxv);
                }
                let encode = {
                    let e = nums(pdf, &d, "Encode");
                    if e.len() >= 2 * m {
                        e
                    } else {
                        size.iter().flat_map(|s| [0.0, (*s as f64) - 1.0]).collect()
                    }
                };
                let decode = {
                    let e = nums(pdf, &d, "Decode");
                    if e.len() >= 2 * n_out {
                        e
                    } else {
                        range.clone()
                    }
                };
                Some(Function::Sampled {
                    domain,
                    range,
                    size,
                    encode,
                    decode,
                    samples: Arc::new(samples),
                    n_out,
                })
            }
            2 => {
                let c0 = {
                    let v = nums(pdf, &d, "C0");
                    if v.is_empty() {
                        vec![0.0]
                    } else {
                        v
                    }
                };
                let c1 = {
                    let v = nums(pdf, &d, "C1");
                    if v.is_empty() {
                        vec![1.0]
                    } else {
                        v
                    }
                };
                Some(Function::Exp {
                    domain,
                    range,
                    c0,
                    c1,
                    n: pdf.dget_f64(&d, "N").unwrap_or(1.0),
                })
            }
            3 => {
                let funcs: Vec<Function> = pdf
                    .dget_array(&d, "Functions")?
                    .iter()
                    .filter_map(|f| Self::parse_depth(pdf, f, depth + 1))
                    .collect();
                if funcs.is_empty() {
                    return None;
                }
                Some(Function::Stitch {
                    domain,
                    range,
                    funcs,
                    bounds: nums(pdf, &d, "Bounds"),
                    encode: nums(pdf, &d, "Encode"),
                })
            }
            4 => {
                let Obj::Stream(s) = &o else { return None };
                let data = crate::filter::decode_stream(pdf, s, false).ok()?.0;
                let mut it = tokenize_ps(&data).into_iter().peekable();
                // skip the outer "{"
                if it.peek().map(String::as_str) == Some("{") {
                    it.next();
                }
                Some(Function::Ps {
                    domain,
                    range,
                    code: Arc::new(parse_ps(&mut it, 0)),
                })
            }
            _ => None,
        }
    }

    pub fn n_outputs(&self) -> usize {
        match self {
            Function::Sampled { n_out, .. } => *n_out,
            Function::Exp { c0, .. } => c0.len(),
            Function::Stitch { funcs, .. } => funcs.first().map_or(1, Function::n_outputs),
            Function::Ps { range, .. } => range.len() / 2,
            Function::Multi(v) => v.len(),
            Function::Identity => 1,
        }
    }

    pub fn eval(&self, input: &[f64]) -> Vec<f64> {
        let clip = |v: Vec<f64>, range: &[f64]| -> Vec<f64> {
            if range.len() >= v.len() * 2 {
                v.iter()
                    .enumerate()
                    .map(|(i, x)| x.clamp(range[2 * i], range[2 * i + 1]))
                    .collect()
            } else {
                v
            }
        };
        match self {
            Function::Identity => input.to_vec(),
            Function::Multi(fs) => fs
                .iter()
                .map(|f| f.eval(input).first().copied().unwrap_or(0.0))
                .collect(),
            Function::Exp {
                domain,
                range,
                c0,
                c1,
                n,
            } => {
                let mut x = input.first().copied().unwrap_or(0.0);
                if domain.len() >= 2 {
                    x = x.clamp(domain[0], domain[1]);
                }
                let p = if *n == 1.0 { x } else { x.powf(*n) };
                let out: Vec<f64> = c0
                    .iter()
                    .zip(c1.iter().chain(std::iter::repeat(&1.0)))
                    .map(|(a, b)| a + p * (b - a))
                    .collect();
                clip(out, range)
            }
            Function::Stitch {
                domain,
                range,
                funcs,
                bounds,
                encode,
            } => {
                let (d0, d1) = (
                    domain.first().copied().unwrap_or(0.0),
                    domain.get(1).copied().unwrap_or(1.0),
                );
                let x = input
                    .first()
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(d0.min(d1), d1.max(d0));
                let k = funcs.len();
                let mut i = 0;
                while i + 1 < k && i < bounds.len() && x >= bounds[i] {
                    i += 1;
                }
                let lo = if i == 0 { d0 } else { bounds[i - 1] };
                let hi = if i < bounds.len() { bounds[i] } else { d1 };
                let (e0, e1) = (
                    encode.get(2 * i).copied().unwrap_or(0.0),
                    encode.get(2 * i + 1).copied().unwrap_or(1.0),
                );
                let t = if hi == lo {
                    e0
                } else {
                    e0 + (x - lo) * (e1 - e0) / (hi - lo)
                };
                clip(funcs[i].eval(&[t]), range)
            }
            Function::Ps {
                domain,
                range,
                code,
            } => {
                let mut st: Vec<f64> = input
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        if domain.len() > 2 * i + 1 {
                            v.clamp(domain[2 * i], domain[2 * i + 1])
                        } else {
                            *v
                        }
                    })
                    .collect();
                run_ps(code, &mut st, 0);
                let n = range.len() / 2;
                let from = st.len().saturating_sub(n);
                let mut out = st[from..].to_vec();
                out.resize(n, 0.0);
                clip(out, range)
            }
            Function::Sampled {
                domain,
                range,
                size,
                encode,
                decode,
                samples,
                n_out,
            } => {
                let m = size.len();
                // encoded position per input
                let mut pos = Vec::with_capacity(m);
                for i in 0..m {
                    let (d0, d1) = (domain[2 * i], domain[2 * i + 1]);
                    let x = input
                        .get(i)
                        .copied()
                        .unwrap_or(0.0)
                        .clamp(d0.min(d1), d1.max(d0));
                    let (e0, e1) = (encode[2 * i], encode[2 * i + 1]);
                    let e = if d1 == d0 {
                        e0
                    } else {
                        e0 + (x - d0) * (e1 - e0) / (d1 - d0)
                    };
                    pos.push(e.clamp(0.0, (size[i] - 1) as f64));
                }
                let mut out = vec![0.0f64; *n_out];
                // multilinear over the 2^m corners
                let corners = 1usize << m.min(8);
                for c in 0..corners {
                    let mut w = 1.0;
                    let mut idx = 0usize;
                    let mut stride = 1usize;
                    for i in 0..m {
                        let lo = pos[i].floor();
                        let frac = pos[i] - lo;
                        let hi_bit = (c >> i) & 1 == 1;
                        let (ii, wi) = if hi_bit {
                            (((lo as usize) + 1).min(size[i] - 1), frac)
                        } else {
                            (lo as usize, 1.0 - frac)
                        };
                        w *= wi;
                        idx += ii * stride;
                        stride *= size[i];
                    }
                    if w == 0.0 {
                        continue;
                    }
                    for (j, o) in out.iter_mut().enumerate() {
                        *o += w * f64::from(samples.get(idx * n_out + j).copied().unwrap_or(0.0));
                    }
                }
                let out: Vec<f64> = out
                    .iter()
                    .enumerate()
                    .map(|(j, v)| decode[2 * j] + v * (decode[2 * j + 1] - decode[2 * j]))
                    .collect();
                clip(out, range)
            }
        }
    }
}
