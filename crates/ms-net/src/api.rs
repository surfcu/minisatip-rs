//! Status/variables API: the C++ symbol tables (`src/api/variables.cpp`).
//! [`CBuf`] reproduces `snprintf`/`strlcatf` truncation so output matches byte for byte.

/// `#define DEFAULT_LOG LOG_UTILS` in `utils.cpp:63`, for its `DEBUGM` site.
const DEFAULT_LOG: u32 = ms_log::LOG_UTILS;

/// `VAR_UINT8` (variables.h:6).
pub const VAR_UINT8: i32 = 1;
/// `VAR_INT8`.
pub const VAR_INT8: i32 = 2;
/// `VAR_UINT16`.
pub const VAR_UINT16: i32 = 3;
/// `VAR_INT16`.
pub const VAR_INT16: i32 = 4;
/// `VAR_INT`.
pub const VAR_INT: i32 = 5;
/// `VAR_INT64`.
pub const VAR_INT64: i32 = 6;
/// `VAR_STRING`.
pub const VAR_STRING: i32 = 7;
/// `VAR_PSTRING`.
pub const VAR_PSTRING: i32 = 8;
/// `VAR_FLOAT`.
pub const VAR_FLOAT: i32 = 9;
/// `VAR_HEX`.
pub const VAR_HEX: i32 = 10;
/// `VAR_ARRAY` base (variables.h:16).
pub const VAR_ARRAY: i32 = 16;
/// `VAR_ARRAY_INT8`.
pub const VAR_ARRAY_INT8: i32 = VAR_ARRAY + VAR_INT8;
/// `VAR_AARRAY` base (variables.h:27).
pub const VAR_AARRAY: i32 = 32;
/// `VAR_AARRAY_UINT8`.
pub const VAR_AARRAY_UINT8: i32 = VAR_AARRAY + VAR_UINT8;
/// `VAR_AARRAY_INT8`.
pub const VAR_AARRAY_INT8: i32 = VAR_AARRAY + VAR_INT8;
/// `VAR_AARRAY_UINT16`.
pub const VAR_AARRAY_UINT16: i32 = VAR_AARRAY + VAR_UINT16;
/// `VAR_AARRAY_INT`.
pub const VAR_AARRAY_INT: i32 = VAR_AARRAY + VAR_INT;
/// `VAR_AARRAY_STRING`.
pub const VAR_AARRAY_STRING: i32 = VAR_AARRAY + VAR_STRING;
/// `VAR_AARRAY_PSTRING`.
pub const VAR_AARRAY_PSTRING: i32 = VAR_AARRAY + VAR_PSTRING;
/// `VAR_FUNCTION` base (variables.h:38).
pub const VAR_FUNCTION: i32 = 48;
/// `VAR_FUNCTION_INT`.
pub const VAR_FUNCTION_INT: i32 = VAR_FUNCTION + VAR_INT;
/// `VAR_FUNCTION_INT64`.
pub const VAR_FUNCTION_INT64: i32 = VAR_FUNCTION + VAR_INT64;
/// `VAR_FUNCTION_STRING`.
pub const VAR_FUNCTION_STRING: i32 = VAR_FUNCTION + VAR_STRING;

/// Longest `$var$` span accepted by `is_var` (variables.h:43).
pub const VAR_LENGTH: usize = 20;
/// `/state.json` buffer size (variables.h:45).
pub const JSON_STATE_MAXLEN: usize = 256 * 1024;
/// `/bandwidth.json` buffer size (variables.h:46).
pub const JSON_BANDWIDTH_MAXLEN: usize = 1024;
/// Scratch string buffer passed to `get_json_state` (variables.h:47).
pub const JSON_STRING_MAXLEN: usize = 1024;
/// `storage` in `var_eval` (variables.cpp:85).
pub const VAR_STORAGE_LEN: usize = 64 * 5;
/// `outp` in `process_file` (utils.cpp:380).
pub const PROCESS_FILE_BUF: usize = 8300;

/// A symbol value as C++ would find it at the symbol address.
#[derive(Debug, Clone, PartialEq)]
pub enum Raw {
    /// Integer field; narrowed to the symbol type when printed.
    Int(i64),
    /// `float` field.
    Float(f32),
    /// C string; bytes after the first NUL are ignored.
    Str(Vec<u8>),
}

impl Raw {
    /// String value from UTF-8 text.
    pub fn text(value: &str) -> Self {
        Self::Str(value.as_bytes().to_vec())
    }

    fn int(&self) -> i64 {
        match self {
            Self::Int(value) => *value,
            Self::Float(value) => *value as i64,
            Self::Str(_) => 0,
        }
    }

    fn float(&self) -> f32 {
        match self {
            Self::Float(value) => *value,
            Self::Int(value) => *value as f32,
            Self::Str(_) => 0.0,
        }
    }
}

/// Reads one element: `(index, max_size)` like the C++ getters.
pub type Reader<'a> = Box<dyn Fn(i32, usize) -> Raw + 'a>;

/// One `_symbols` entry (symbols.h:4-11).
pub struct Symbol<'a> {
    /// Symbol name, matched as a prefix of the requested variable.
    pub name: &'static str,
    /// `VAR_*` type.
    pub ty: i32,
    /// Float multiplier applied before the integer cast.
    pub multiplier: f32,
    /// Element count for array and function symbols.
    pub len: i32,
    read: Reader<'a>,
}

impl<'a> Symbol<'a> {
    /// Creates a symbol whose elements come from `read`.
    pub fn new(
        name: &'static str,
        ty: i32,
        multiplier: f32,
        len: i32,
        read: impl Fn(i32, usize) -> Raw + 'a,
    ) -> Self {
        Self {
            name,
            ty,
            multiplier,
            len,
            read: Box::new(read),
        }
    }

    /// Creates a scalar symbol.
    pub fn scalar(name: &'static str, ty: i32, multiplier: f32, value: Raw) -> Self {
        Self::new(name, ty, multiplier, 0, move |_, _| value.clone())
    }

    /// Reads element `index` with a `max_size` string buffer.
    pub fn read(&self, index: i32, max_size: usize) -> Raw {
        (self.read)(index, max_size)
    }
}

/// A subsystem's symbol table (`adapters_sym`, `stream_sym`, ...).
pub trait SymbolProvider {
    /// Symbols in C++ table order.
    fn symbols(&self) -> Vec<Symbol<'_>>;
}

/// The `sym[]` list: tables in lookup and JSON order.
#[derive(Default)]
pub struct SymbolTable<'a> {
    tables: Vec<Vec<Symbol<'a>>>,
}

impl<'a> SymbolTable<'a> {
    /// Creates an empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one table.
    pub fn push(&mut self, table: Vec<Symbol<'a>>) {
        self.tables.push(table);
    }

    fn iter(&self) -> impl Iterator<Item = &Symbol<'a>> {
        self.tables.iter().flatten()
    }

    /// Symbol with exactly this table name (no prefix matching).
    pub fn symbol(&self, name: &str) -> Option<&Symbol<'a>> {
        self.iter().find(|symbol| symbol.name == name)
    }

    /// `get_var_address` (variables.cpp:113-169): first prefix match wins;
    /// out-of-range array indexes keep searching.
    pub fn get_var_address(&self, var: &[u8]) -> Option<(i32, Raw, f32)> {
        for symbol in self.iter() {
            let Some(rest) = var.strip_prefix(symbol.name.as_bytes()) else {
                continue;
            };
            let kind = symbol.ty & 0xF0;
            if symbol.ty < VAR_ARRAY {
                return Some((
                    symbol.ty,
                    symbol.read(0, VAR_STORAGE_LEN),
                    symbol.multiplier,
                ));
            } else if kind == VAR_ARRAY || kind == VAR_AARRAY {
                let off = parse_int(rest, 0);
                if off >= 0 && off < symbol.len {
                    let raw = symbol.read(off, VAR_STORAGE_LEN);
                    return Some((symbol.ty, raw, symbol.multiplier));
                }
            } else if symbol.ty == VAR_FUNCTION_INT || symbol.ty == VAR_FUNCTION_INT64 {
                let raw = symbol.read(parse_int(rest, 0), VAR_STORAGE_LEN);
                return Some((symbol.ty, raw, function_multiplier(symbol.multiplier)));
            } else if symbol.ty == VAR_FUNCTION_STRING {
                let raw = symbol.read(parse_int(rest, 0), VAR_STORAGE_LEN);
                return Some((symbol.ty, raw, 0.0));
            }
        }
        None
    }

    /// `var_eval` (variables.cpp:83-96) for the name between the `$` signs;
    /// returns the `nb` bytes C++ counts, including a NUL when truncated.
    pub fn var_eval(&self, var: &[u8], max_len: usize) -> Vec<u8> {
        match self.get_var_address(var) {
            Some((ty, raw, multiplier)) => {
                clamp_snprintf(snprintf_pointer(ty, &raw, multiplier), max_len)
            }
            None => Vec::new(),
        }
    }

    /// `get_json_state` with the C++ buffer sizes.
    pub fn json_state(&self) -> Vec<u8> {
        self.json_state_sized(JSON_STATE_MAXLEN, JSON_STRING_MAXLEN)
    }

    /// `get_json_state` (variables.cpp:189-296) into `len`/`slen` buffers.
    pub fn json_state_sized(&self, len: usize, slen: usize) -> Vec<u8> {
        let mut buf = CBuf::new(len);
        let mut first = true;
        buf.catf(b"{\n");
        for symbol in self.iter() {
            let key = if first {
                format!("\"{}\":", symbol.name)
            } else {
                format!(",\n\"{}\":", symbol.name)
            };
            buf.catf(key.as_bytes());
            let string = matches!(
                symbol.ty,
                VAR_STRING | VAR_PSTRING | VAR_HEX | VAR_AARRAY_STRING | VAR_AARRAY_PSTRING
            );
            let kind = symbol.ty & 0xF0;
            if symbol.ty < VAR_ARRAY {
                json_value(&mut buf, symbol, 0, string, slen, symbol.multiplier);
            } else if kind == VAR_ARRAY || kind == VAR_AARRAY {
                // VAR_ARRAY reads `addr + off + skip`; every C++ array symbol
                // has one-byte elements and skip 0, so that is element `off`.
                buf.catf(b"[");
                for off in 0..symbol.len {
                    if off > 0 {
                        buf.catf(b",");
                    }
                    json_value(&mut buf, symbol, off, string, slen, symbol.multiplier);
                }
                buf.catf(b"]");
            } else if symbol.ty == VAR_FUNCTION_INT || symbol.ty == VAR_FUNCTION_INT64 {
                let multiplier = function_multiplier(symbol.multiplier);
                buf.catf(b"[");
                for off in 0..symbol.len {
                    let raw = symbol.read(off, slen.saturating_sub(1));
                    if off > 0 {
                        buf.catf(b",");
                    }
                    buf.put(&snprintf_pointer(symbol.ty, &raw, multiplier));
                }
                buf.catf(b"]");
            } else if symbol.ty == VAR_FUNCTION_STRING {
                buf.catf(b"[");
                for off in 0..symbol.len {
                    let raw = symbol.read(off, slen.saturating_sub(1));
                    if off > 0 {
                        buf.catf(b",");
                    }
                    let value = match &raw {
                        Raw::Str(bytes) => cstr(bytes),
                        _ => &[],
                    };
                    buf.escape_json(value);
                }
                buf.catf(b"]");
            } else {
                buf.catf(b"\"\"");
            }
            first = false;
        }
        buf.catf(b"\n}\n");
        buf.into_bytes()
    }

    /// `process_file` (utils.cpp:379-421): returns the body bytes and whether
    /// C++ flushed mid-file (no Content-Length, `\r\n\r\n` appended).
    pub fn process_file(&self, data: &[u8]) -> (Vec<u8>, bool) {
        // utils.cpp:385 (`%p` of the template buffer has no Rust equivalent;
        // it is normalised to 0x0).
        ms_log::log!("processing_file 0x0 len {}:", data.len());
        let mut out = Vec::with_capacity(data.len());
        let mut io = 0usize;
        let mut flushed = false;
        // Start of the bytes written since the last flush (`outp` in C++).
        let mut tail_start = 0usize;
        let mut i = 0;
        while i < data.len() {
            let lv = if data[i] == b'$' {
                is_var(&data[i..])
            } else {
                0
            };
            if lv == 0 {
                out.push(data[i]);
                io += 1;
            } else {
                let value = self.var_eval(&data[i + 1..i + lv], PROCESS_FILE_BUF - io - 10);
                io += value.len();
                out.extend_from_slice(&value);
                i += lv;
            }
            if io > PROCESS_FILE_BUF - 100 {
                flushed = true;
                tail_start = out.len();
                io = 0;
            }
            i += 1;
        }
        if flushed {
            out.extend_from_slice(b"\r\n\r\n");
            // utils.cpp:419: DEBUGM("%s", outp) after the trailing CRLF pair.
            ms_log::debugm!("{}", String::from_utf8_lossy(cstr(&out[tail_start..])));
        } else {
            // http_response(..., outp, 0, 0) sends strlen(outp) bytes.
            let end = out.iter().position(|b| *b == 0).unwrap_or(out.len());
            out.truncate(end);
        }
        (out, flushed)
    }
}

impl<'a> FromIterator<Vec<Symbol<'a>>> for SymbolTable<'a> {
    fn from_iter<T: IntoIterator<Item = Vec<Symbol<'a>>>>(iter: T) -> Self {
        Self {
            tables: iter.into_iter().collect(),
        }
    }
}

fn function_multiplier(multiplier: f32) -> f32 {
    if multiplier == 0.0 { 1.0 } else { multiplier }
}

fn json_value(
    buf: &mut CBuf,
    symbol: &Symbol<'_>,
    off: i32,
    string: bool,
    slen: usize,
    multiplier: f32,
) {
    let max_len = slen.saturating_sub(1);
    let raw = symbol.read(off, max_len);
    let full = snprintf_pointer(symbol.ty, &raw, multiplier);
    if string {
        buf.escape_json(&clamp_snprintf(full, max_len));
    } else {
        buf.put(&full);
    }
}

/// `is_var` (variables.cpp:98-109): offset of the closing `$`, or 0.
pub fn is_var(s: &[u8]) -> usize {
    if s.first() != Some(&b'$') {
        return 0;
    }
    let mut i = 1;
    while let Some(&c) = s.get(i) {
        if c == 0 {
            return 0;
        }
        if c == b'$' {
            return i;
        }
        if i >= VAR_LENGTH {
            return 0;
        }
        i += 1;
    }
    0
}

/// `parse_int` (utils.cpp:102-124): leading blanks, optional sign, digits
/// up to the first non-digit; `dv` when nothing parses or on overflow.
pub fn parse_int(s: &[u8], dv: i32) -> i32 {
    let start = s
        .iter()
        .position(|c| !b" \t\r\n\x0b\x0c".contains(c))
        .unwrap_or(s.len());
    let mut s = &s[start..];
    match s.first() {
        None => return dv,
        Some(b'+') => {
            s = &s[1..];
            if s.is_empty() {
                return dv;
            }
        }
        Some(c) if *c == b'-' || c.is_ascii_digit() => {}
        Some(_) => return dv,
    }
    let (negative, digits) = match s.strip_prefix(b"-") {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let count = digits.iter().take_while(|c| c.is_ascii_digit()).count();
    if count == 0 {
        return dv;
    }
    let mut value: i64 = 0;
    for c in &digits[..count] {
        value = value * 10 + i64::from(c - b'0');
        if value > i64::from(i32::MAX) + 1 {
            return dv;
        }
    }
    let value = if negative { -value } else { value };
    i32::try_from(value).unwrap_or(dv)
}

/// `snprintf_pointer` (variables.cpp:30-81) without the length limit.
pub fn snprintf_pointer(ty: i32, raw: &Raw, multiplier: f32) -> Vec<u8> {
    let scaled = |value: f32| cvt_i32(value * multiplier).to_string().into_bytes();
    match ty & 0xF {
        VAR_UINT8 => scaled(f32::from(raw.int() as u8)),
        VAR_INT8 => scaled(f32::from(raw.int() as i8)),
        VAR_UINT16 => scaled(f32::from(raw.int() as u16)),
        VAR_INT16 => scaled(f32::from(raw.int() as i16)),
        VAR_INT => scaled(raw.int() as i32 as f32),
        VAR_INT64 => cvt_i64(raw.int() as f32 * multiplier)
            .to_string()
            .into_bytes(),
        VAR_STRING | VAR_PSTRING => match raw {
            Raw::Str(bytes) => cstr(bytes).to_vec(),
            _ => Vec::new(),
        },
        VAR_FLOAT => printf_f(f64::from(raw.float() * multiplier)).into_bytes(),
        VAR_HEX => {
            let value = cvt_i32(raw.int() as i32 as f32 * multiplier);
            format!("0x{:x}", value as u32).into_bytes()
        }
        _ => Vec::new(),
    }
}

/// The bytes `snprintf` leaves for a `max_len` buffer, counted the way
/// `snprintf_pointer` returns them: truncation keeps the terminating NUL.
pub fn clamp_snprintf(mut full: Vec<u8>, max_len: usize) -> Vec<u8> {
    if full.len() < max_len {
        return full;
    }
    if max_len == 0 {
        return Vec::new();
    }
    full.truncate(max_len - 1);
    full.push(0);
    full
}

/// Bytes before the first NUL.
pub fn cstr(bytes: &[u8]) -> &[u8] {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    &bytes[..end]
}

/// `_strncpy(dest, src, n)` (utils.cpp:815-818): at most `n - 1` bytes.
pub fn strncpy_c(src: &[u8], n: usize) -> Vec<u8> {
    let src = cstr(src);
    src[..src.len().min(n.saturating_sub(1))].to_vec()
}

/// x86-64 `cvttss2si`: out-of-range and NaN give the integer minimum.
fn cvt_i32(value: f32) -> i32 {
    if value.is_nan() || !(-2_147_483_648.0..2_147_483_648.0).contains(&value) {
        i32::MIN
    } else {
        value as i32
    }
}

fn cvt_i64(value: f32) -> i64 {
    if value.is_nan() || !(-9.223_372e18..9.223_372e18).contains(&value) {
        i64::MIN
    } else {
        value as i64
    }
}

/// glibc `%f`.
fn printf_f(value: f64) -> String {
    if value.is_nan() {
        if value.is_sign_negative() {
            "-nan"
        } else {
            "nan"
        }
        .to_owned()
    } else if value.is_infinite() {
        if value < 0.0 { "-inf" } else { "inf" }.to_owned()
    } else {
        format!("{value:.6}")
    }
}

/// A fixed-size C buffer with a write position, as used with `strlcatf`.
#[derive(Debug, Clone)]
pub struct CBuf {
    buf: Vec<u8>,
    ptr: usize,
}

impl CBuf {
    /// Zero-filled buffer of `size` bytes.
    pub fn new(size: usize) -> Self {
        Self {
            buf: vec![0; size],
            ptr: 0,
        }
    }

    /// Current position (`ptr`/`len`).
    pub fn len(&self) -> usize {
        self.ptr
    }

    /// Whether nothing was appended.
    pub fn is_empty(&self) -> bool {
        self.ptr == 0
    }

    fn write_at(&mut self, at: usize, bytes: &[u8]) {
        for (offset, byte) in bytes.iter().enumerate() {
            if let Some(slot) = self.buf.get_mut(at + offset) {
                *slot = *byte;
            }
        }
    }

    fn snprintf(&mut self, at: usize, room: usize, text: &[u8]) {
        if room == 0 {
            return;
        }
        let n = text.len().min(room - 1);
        self.write_at(at, &text[..n]);
        self.write_at(at + n, &[0]);
    }

    /// `strlcatf(buf, size, ptr, "%s", text)` (utils.h:176-185).
    pub fn catf(&mut self, text: &[u8]) {
        let size = self.buf.len();
        self.snprintf(self.ptr, size.saturating_sub(self.ptr), text);
        self.ptr = self.ptr.saturating_add(text.len());
        if self.ptr >= size {
            self.ptr = size.saturating_sub(1);
        }
    }

    /// `ptr += snprintf_pointer(buf + ptr, len - ptr, ...)`.
    pub fn put(&mut self, text: &[u8]) {
        let room = self.buf.len().saturating_sub(self.ptr);
        self.snprintf(self.ptr, room, text);
        self.ptr += text.len().min(room);
    }

    /// Writes one byte (`dest[index] = byte`).
    pub fn set(&mut self, index: usize, byte: u8) {
        self.write_at(index, &[byte]);
    }

    /// `ptr += escape_json_string(buf + ptr, len - ptr, src, sl)`
    /// (variables.cpp:171-187), including its `strlcatf(dest, sl, ...)` bug.
    // The C++ `LOG_AND_RETURN` sits in a void function, so it returns `()`.
    #[allow(clippy::unused_unit)]
    pub fn escape_json(&mut self, src: &[u8]) {
        let base = self.ptr;
        let dl = self.buf.len() as i64 - base as i64;
        if dl < 2 {
            // variables.cpp:174 (`__FUNCTION__` and `%s` of `src`).
            ms_log::log_and_return!(
                (),
                "{}: dl {} < 2 for {}",
                "escape_json_string",
                dl,
                String::from_utf8_lossy(cstr(src))
            );
        }
        let sl = src.len() as i64;
        self.write_at(base, b"\"");
        let mut j: i64 = 1;
        for &c in src {
            if j >= dl - 1 {
                break;
            }
            if c >= 32 {
                self.write_at(base + j as usize, &[c]);
                j += 1;
                continue;
            }
            let text = format!("%x{c:02X}");
            let room = sl - j;
            if room < 0 {
                // A negative size converts to a huge size_t: no truncation.
                self.write_at(base + j as usize, text.as_bytes());
                self.write_at(base + j as usize + text.len(), &[0]);
            } else {
                self.snprintf(base + j as usize, room as usize, text.as_bytes());
            }
            j += text.len() as i64;
            if j >= sl {
                j = sl - 1;
            }
        }
        self.write_at(base + j as usize, b"\"");
        j += 1;
        self.write_at(base + j as usize, &[0]);
        // C++ can move `ptr` past the buffer here; never let a later write
        // underflow (DEV-140).
        self.ptr = (base + j as usize).min(self.buf.len());
    }

    /// The C string at the start of the buffer.
    pub fn as_cstr(&self) -> &[u8] {
        cstr(&self.buf)
    }

    /// The first `ptr` bytes, as sent with an explicit length.
    pub fn into_bytes(mut self) -> Vec<u8> {
        self.buf.truncate(self.ptr);
        self.buf
    }
}
