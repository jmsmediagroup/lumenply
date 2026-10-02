//! Photoshop's `EngineData`: the PostScript-like text a type layer (`TySh`)
//! keeps its characters, style runs, paragraph runs and fonts in.
//!
//! ```text
//! << /EngineDict << /Editor << /Text (þÿ…UTF-16BE…) >> >>
//!    /ResourceDict << /FontSet [ << /Name (þÿ…) >> ] >> >>
//! ```
//!
//! Dictionaries `<< /Key value … >>`, arrays `[ … ]`, integers, decimals
//! (`.5`, `-1.0`), `true`/`false`, `/Name` values and strings in
//! parentheses. A string starting with the byte-order mark `FE FF` is
//! UTF-16BE; inside a string a backslash takes the next byte literally
//! (Photoshop escapes `(`, `)` and `\` bytes, even inside UTF-16 code
//! units). The writer emits the same layout Photoshop does, with numbers
//! that never use exponents, so Photoshop and psd-tools read it back.

/// One EngineData value. Integers and decimals stay distinct because
/// Photoshop writes (and expects) `0` for some keys and `0.0` for others.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Value {
    Dict(Vec<(String, Value)>),
    Array(Vec<Value>),
    Int(i64),
    Num(f64),
    Bool(bool),
    Str(String),
    /// A bare `/Name` used as a value.
    Name(String),
}

impl Value {
    /// The value under `key` in a dictionary.
    pub(super) fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The value at a path of dictionary keys.
    pub(super) fn at(&self, path: &[&str]) -> Option<&Value> {
        path.iter().try_fold(self, |v, k| v.get(k))
    }

    pub(super) fn num(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Num(f) => Some(*f),
            _ => None,
        }
    }

    pub(super) fn int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Num(f) if f.fract() == 0.0 && f.abs() < 1e15 => Some(*f as i64),
            _ => None,
        }
    }

    pub(super) fn bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub(super) fn str(&self) -> Option<&str> {
        match self {
            Value::Str(s) | Value::Name(s) => Some(s),
            _ => None,
        }
    }

    pub(super) fn array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(v) => Some(v),
            _ => None,
        }
    }

    /// The numbers of an array (`[ 1.0 .5 0 ]`); non-numbers are skipped.
    pub(super) fn nums(&self) -> Vec<f64> {
        self.array()
            .map(|a| a.iter().filter_map(Value::num).collect())
            .unwrap_or_default()
    }
}

/// Shorthand for building dictionaries in the writer.
pub(super) fn dict(items: Vec<(&str, Value)>) -> Value {
    Value::Dict(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

// ---- reading ------------------------------------------------------------------------------

/// Nesting deeper than this is treated as corrupt (Photoshop's own data
/// goes about ten levels deep).
const MAX_DEPTH: usize = 64;

struct Lexer<'a> {
    b: &'a [u8],
    pos: usize,
}

#[derive(Debug, PartialEq)]
enum Tok {
    DictStart,
    DictEnd,
    ArrStart,
    ArrEnd,
    Key(String),
    Str(String),
    Word(String),
}

fn is_delim(c: u8) -> bool {
    matches!(c, b'<' | b'>' | b'[' | b']' | b'(' | b')' | b'/') || is_space(c)
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | 0 | 0x0c)
}

impl Lexer<'_> {
    fn next(&mut self) -> Option<Tok> {
        while self.pos < self.b.len() && is_space(self.b[self.pos]) {
            self.pos += 1;
        }
        let rest = &self.b[self.pos..];
        let c = *rest.first()?;
        if rest.starts_with(b"<<") {
            self.pos += 2;
            return Some(Tok::DictStart);
        }
        if rest.starts_with(b">>") {
            self.pos += 2;
            return Some(Tok::DictEnd);
        }
        match c {
            b'[' => {
                self.pos += 1;
                Some(Tok::ArrStart)
            }
            b']' => {
                self.pos += 1;
                Some(Tok::ArrEnd)
            }
            b'(' => {
                self.pos += 1;
                let mut raw = Vec::new();
                while self.pos < self.b.len() {
                    let c = self.b[self.pos];
                    self.pos += 1;
                    match c {
                        b'\\' if self.pos < self.b.len() => {
                            raw.push(self.b[self.pos]);
                            self.pos += 1;
                        }
                        b')' => break,
                        _ => raw.push(c),
                    }
                }
                Some(Tok::Str(decode_string(&raw)))
            }
            b'/' => {
                self.pos += 1;
                Some(Tok::Key(self.word()))
            }
            _ => {
                let w = self.word();
                if w.is_empty() {
                    // A stray delimiter (`>` or `)`): step over it.
                    self.pos += 1;
                    return self.next();
                }
                Some(Tok::Word(w))
            }
        }
    }

    fn word(&mut self) -> String {
        let start = self.pos;
        while self.pos < self.b.len() && !is_delim(self.b[self.pos]) {
            self.pos += 1;
        }
        String::from_utf8_lossy(&self.b[start..self.pos]).into_owned()
    }
}

/// A string's bytes: UTF-16BE after a byte-order mark, else Latin-1.
fn decode_string(raw: &[u8]) -> String {
    match raw.strip_prefix(&[0xfe, 0xff]) {
        Some(body) => {
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|p| u16::from_be_bytes([p[0], p[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        None => raw.iter().map(|&b| b as char).collect(),
    }
}

fn word_value(w: &str) -> Value {
    match w {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => {
            if !w.contains('.') {
                if let Ok(i) = w.parse::<i64>() {
                    return Value::Int(i);
                }
            }
            match w.parse::<f64>() {
                Ok(f) if f.is_finite() => Value::Num(f),
                _ => Value::Name(w.to_string()),
            }
        }
    }
}

fn read_value(lx: &mut Lexer, tok: Tok, depth: usize) -> Option<Value> {
    if depth > MAX_DEPTH {
        return None;
    }
    Some(match tok {
        Tok::DictStart => read_dict(lx, depth + 1)?,
        Tok::ArrStart => {
            let mut items = Vec::new();
            loop {
                match lx.next()? {
                    Tok::ArrEnd => break,
                    Tok::DictEnd => return None,
                    t => items.push(read_value(lx, t, depth + 1)?),
                }
            }
            Value::Array(items)
        }
        Tok::Str(s) => Value::Str(s),
        Tok::Key(k) => Value::Name(k),
        Tok::Word(w) => word_value(&w),
        Tok::DictEnd | Tok::ArrEnd => return None,
    })
}

fn read_dict(lx: &mut Lexer, depth: usize) -> Option<Value> {
    let mut items = Vec::new();
    loop {
        match lx.next() {
            // An unterminated top-level dictionary ends with the data.
            None => break,
            Some(Tok::DictEnd) => break,
            Some(Tok::Key(k)) => {
                let t = lx.next()?;
                items.push((k, read_value(lx, t, depth)?));
            }
            // Anything else where a key belongs is skipped.
            Some(_) => {}
        }
    }
    Some(Value::Dict(items))
}

/// Parse EngineData; `None` when it is not a well-formed dictionary.
pub(super) fn parse(data: &[u8]) -> Option<Value> {
    let mut lx = Lexer { b: data, pos: 0 };
    loop {
        match lx.next()? {
            Tok::DictStart => return read_dict(&mut lx, 0),
            _ => continue,
        }
    }
}

// ---- writing ------------------------------------------------------------------------------

/// A decimal the way Photoshop writes it: `0.0`, `1.0`, `.5`, `-1.25`,
/// at most five decimals and never an exponent.
fn fmt_num(f: f64) -> String {
    let f = if f.is_finite() { f } else { 0.0 };
    let mut s = format!("{f:.5}");
    while s.ends_with('0') && !s.ends_with(".0") {
        s.pop();
    }
    if s == "-0.0" {
        s = "0.0".into();
    }
    if let Some(rest) = s.strip_prefix("0.").filter(|r| *r != "0") {
        s = format!(".{rest}");
    } else if let Some(rest) = s.strip_prefix("-0.") {
        s = format!("-.{rest}");
    }
    s
}

fn put_string(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(b"(\xfe\xff");
    for u in s.encode_utf16() {
        for b in u.to_be_bytes() {
            if matches!(b, b'(' | b')' | b'\\') {
                out.push(b'\\');
            }
            out.push(b);
        }
    }
    out.push(b')');
}

fn tabs(out: &mut Vec<u8>, n: usize) {
    out.extend(std::iter::repeat_n(b'\t', n));
}

fn is_container(v: &Value) -> bool {
    matches!(v, Value::Dict(_)) || matches!(v, Value::Array(a) if a.iter().any(is_container))
}

fn put_scalar(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Value::Num(f) => out.extend_from_slice(fmt_num(*f).as_bytes()),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Str(s) => put_string(out, s),
        Value::Name(n) => {
            out.push(b'/');
            out.extend_from_slice(n.as_bytes());
        }
        Value::Dict(_) | Value::Array(_) => {}
    }
}

/// Write `v` (a dictionary or array) whose opening line is already out.
fn put_block(out: &mut Vec<u8>, v: &Value, depth: usize) {
    match v {
        Value::Dict(items) => {
            tabs(out, depth);
            out.extend_from_slice(b"<<\n");
            for (k, item) in items {
                tabs(out, depth + 1);
                out.push(b'/');
                out.extend_from_slice(k.as_bytes());
                match item {
                    Value::Dict(_) => {
                        out.push(b'\n');
                        put_block(out, item, depth + 1);
                    }
                    Value::Array(a) if is_container(item) => {
                        out.extend_from_slice(b" [\n");
                        for e in a {
                            put_block(out, e, depth + 1);
                        }
                        tabs(out, depth + 1);
                        out.extend_from_slice(b"]\n");
                    }
                    _ => {
                        out.push(b' ');
                        put_inline(out, item);
                        out.push(b'\n');
                    }
                }
            }
            tabs(out, depth);
            out.extend_from_slice(b">>\n");
        }
        _ => {
            tabs(out, depth);
            put_inline(out, v);
            out.push(b'\n');
        }
    }
}

/// A scalar or an array of scalars on one line: `[ 1.0 0.0 ]`.
fn put_inline(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Array(a) => {
            out.push(b'[');
            for e in a {
                out.push(b' ');
                put_inline(out, e);
            }
            out.extend_from_slice(b" ]");
        }
        _ => put_scalar(out, v),
    }
}

/// Serialise a top-level dictionary as Photoshop does.
pub(super) fn write(v: &Value) -> Vec<u8> {
    let mut out = b"\n\n".to_vec();
    put_block(&mut out, v, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An excerpt of what Photoshop CS6 wrote for a one-word layer (the
    /// ag-psd "text-simple" test file), bytes as in the file.
    const SNIPPET: &[u8] = b"\n\n<<\n\t/EngineDict\n\t<<\n\t\t/Editor\n\t\t<<\n\t\t\t/Text (\xfe\xff\x00H\x00e\x00l\x00l\x00o\x00\r)\n\t\t>>\n\t\t/StyleRun\n\t\t<<\n\t\t\t/RunArray [\n\t\t\t<<\n\t\t\t\t/StyleSheet\n\t\t\t\t<<\n\t\t\t\t\t/StyleSheetData\n\t\t\t\t\t<<\n\t\t\t\t\t\t/FontSize 60.0\n\t\t\t\t\t\t/AutoKerning true\n\t\t\t\t\t\t/Kerning 0\n\t\t\t\t\t\t/FillColor\n\t\t\t\t\t\t<<\n\t\t\t\t\t\t\t/Type 1\n\t\t\t\t\t\t\t/Values [ 1.0 .84886 .43897 .87451 ]\n\t\t\t\t\t\t>>\n\t\t\t\t\t>>\n\t\t\t\t>>\n\t\t\t>>\n\t\t\t]\n\t\t\t/RunLengthArray [ 1 1 4 ]\n\t\t\t/IsJoinable 2\n\t\t>>\n\t>>\n\t/ResourceDict\n\t<<\n\t\t/KinsokuSet [\n\t\t<<\n\t\t\t/Hanging (\xfe\xff0\x010\x02\x00.\x00,)\n\t\t\t/NoStart (\xfe\xff\x00?\x00!\x00\\)\x00]\x00})\n\t\t>>\n\t\t]\n\t\t/FontSet [\n\t\t<<\n\t\t\t/Name (\xfe\xff\x00M\x00y\x00r\x00i\x00a\x00d\x00P\x00r\x00o\x00-\x00R\x00e\x00g\x00u\x00l\x00a\x00r)\n\t\t\t/Synthetic 0\n\t\t>>\n\t\t]\n\t\t/SmallCapSize .7\n\t\t/BoxBounds [ 0.0 -1.6203 150.0 17.61632 ]\n\t>>\n>>";

    #[test]
    fn photoshop_engine_data_parses_into_values() {
        let v = parse(SNIPPET).expect("parses");
        assert_eq!(
            v.at(&["EngineDict", "Editor", "Text"]).and_then(Value::str),
            Some("Hello\r")
        );
        let style = v.at(&["EngineDict", "StyleRun"]).unwrap();
        assert_eq!(style.get("RunLengthArray").unwrap().nums(), vec![1.0, 1.0, 4.0]);
        assert_eq!(style.get("IsJoinable"), Some(&Value::Int(2)));
        let run = &style.get("RunArray").unwrap().array().unwrap()[0];
        let data = run.at(&["StyleSheet", "StyleSheetData"]).unwrap();
        assert_eq!(data.get("FontSize"), Some(&Value::Num(60.0)));
        assert_eq!(data.get("AutoKerning"), Some(&Value::Bool(true)));
        assert_eq!(data.get("Kerning"), Some(&Value::Int(0)));
        assert_eq!(
            data.at(&["FillColor", "Values"]).unwrap().nums(),
            vec![1.0, 0.84886, 0.43897, 0.87451]
        );
        let res = v.get("ResourceDict").unwrap();
        // The escaped `)` inside a UTF-16 unit and Japanese punctuation.
        let kinsoku = &res.get("KinsokuSet").unwrap().array().unwrap()[0];
        assert_eq!(kinsoku.get("NoStart").and_then(Value::str), Some("?!)]}"));
        assert_eq!(
            kinsoku.get("Hanging").and_then(Value::str),
            Some("\u{3001}\u{3002}.,")
        );
        let font = &res.get("FontSet").unwrap().array().unwrap()[0];
        assert_eq!(font.get("Name").and_then(Value::str), Some("MyriadPro-Regular"));
        assert_eq!(res.get("SmallCapSize"), Some(&Value::Num(0.7)));
        assert_eq!(
            res.get("BoxBounds").unwrap().nums(),
            vec![0.0, -1.6203, 150.0, 17.61632]
        );
    }

    #[test]
    fn written_engine_data_reads_back_identically() {
        let v = parse(SNIPPET).unwrap();
        let bytes = write(&v);
        assert!(bytes
            .starts_with(b"\n\n<<\n\t/EngineDict\n\t<<\n\t\t/Editor\n\t\t<<\n\t\t\t/Text (\xfe\xff\x00H"));
        assert_eq!(parse(&bytes), Some(v));
        // Numbers in Photoshop's spelling; brackets and backslashes escaped
        // byte-wise inside UTF-16.
        let w = write(&dict(vec![
            ("A", Value::Num(0.5)),
            ("B", Value::Num(-0.25)),
            ("C", Value::Num(60.0)),
            ("D", Value::Num(1e-9)),
            ("E", Value::Array(vec![Value::Int(3), Value::Num(1.0 / 3.0)])),
            ("F", Value::Str("a(b)\\".into())),
            ("G", Value::Name("Hrzn".into())),
        ]));
        let text = String::from_utf8_lossy(&w);
        assert!(text.contains("/A .5\n"), "{text}");
        assert!(text.contains("/B -.25\n"), "{text}");
        assert!(text.contains("/C 60.0\n"), "{text}");
        assert!(text.contains("/D 0.0\n"), "{text}");
        assert!(text.contains("/E [ 3 .33333 ]\n"), "{text}");
        assert!(text.contains("/G /Hrzn\n"), "{text}");
        assert!(
            w.windows(14).any(|s| s == b"\x00a\x00\\(\x00b\x00\\)\x00\\\\)"),
            "{w:?}"
        );
        assert_eq!(parse(&w).unwrap().get("F").and_then(Value::str), Some("a(b)\\"));
    }

    #[test]
    fn malformed_engine_data_never_panics() {
        assert_eq!(parse(b""), None);
        assert_eq!(parse(b"garbage"), None);
        // Unterminated containers end with the data instead of looping.
        assert!(parse(b"<< /A [ 1 2").is_none());
        assert_eq!(parse(b"<< /A 1"), Some(dict(vec![("A", Value::Int(1))])));
        assert_eq!(
            parse(b"<< /S (\xfe\xff\x00a"),
            Some(dict(vec![("S", Value::Str("a".into()))]))
        );
        // Absurd nesting is refused, not a stack overflow.
        let deep: Vec<u8> = b"<< /A ".iter().copied().cycle().take(6 * 10_000).collect();
        assert!(parse(&deep).is_none());
    }
}
