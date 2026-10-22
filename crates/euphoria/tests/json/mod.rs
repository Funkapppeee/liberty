//! A tiny JSON reader for the test vectors (no external crates).
#![allow(dead_code)]

#[derive(Debug, Clone)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> &Json {
        match self {
            Json::Obj(kv) => kv
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v)
                .unwrap_or_else(|| panic!("missing key {key}")),
            _ => panic!("not an object looking up {key}"),
        }
    }
    pub fn arr(&self) -> &[Json] {
        match self {
            Json::Arr(a) => a,
            _ => panic!("not an array"),
        }
    }
    pub fn num(&self) -> f64 {
        match self {
            Json::Num(n) => *n,
            _ => panic!("not a number"),
        }
    }
    pub fn f32(&self) -> f32 {
        self.num() as f32
    }
    pub fn bool(&self) -> bool {
        match self {
            Json::Bool(b) => *b,
            _ => panic!("not a bool"),
        }
    }
    pub fn str(&self) -> &str {
        match self {
            Json::Str(s) => s,
            _ => panic!("not a string"),
        }
    }
}

pub fn parse(text: &str) -> Result<Json, String> {
    let mut p = Parser { b: text.as_bytes(), i: 0 };
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("trailing data at {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&self) -> Result<u8, String> {
        self.b.get(self.i).copied().ok_or_else(|| "unexpected end".to_string())
    }

    fn lit(&mut self, s: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(s.as_bytes()) {
            self.i += s.len();
            Ok(v)
        } else {
            Err(format!("bad literal at {}", self.i))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.peek()? {
            b'{' => {
                self.i += 1;
                let mut kv = Vec::new();
                loop {
                    self.ws();
                    if self.peek()? == b'}' {
                        self.i += 1;
                        break;
                    }
                    let Json::Str(k) = self.value()? else { return Err("object key".into()) };
                    self.ws();
                    if self.peek()? != b':' {
                        return Err(format!("expected : at {}", self.i));
                    }
                    self.i += 1;
                    kv.push((k, self.value()?));
                    self.ws();
                    match self.peek()? {
                        b',' => self.i += 1,
                        b'}' => {}
                        _ => return Err(format!("expected , or }} at {}", self.i)),
                    }
                }
                Ok(Json::Obj(kv))
            }
            b'[' => {
                self.i += 1;
                let mut a = Vec::new();
                loop {
                    self.ws();
                    if self.peek()? == b']' {
                        self.i += 1;
                        break;
                    }
                    a.push(self.value()?);
                    self.ws();
                    match self.peek()? {
                        b',' => self.i += 1,
                        b']' => {}
                        _ => return Err(format!("expected , or ] at {}", self.i)),
                    }
                }
                Ok(Json::Arr(a))
            }
            b'"' => {
                self.i += 1;
                let mut bytes = Vec::new();
                loop {
                    let ch = self.peek()?;
                    self.i += 1;
                    match ch {
                        b'"' => break,
                        b'\\' => {
                            let e = self.peek()?;
                            self.i += 1;
                            match e {
                                b'n' => bytes.push(b'\n'),
                                b't' => bytes.push(b'\t'),
                                b'u' => {
                                    let hex = std::str::from_utf8(&self.b[self.i..self.i + 4])
                                        .map_err(|e| e.to_string())?;
                                    self.i += 4;
                                    let cp = u32::from_str_radix(hex, 16).map_err(|e| e.to_string())?;
                                    let mut buf = [0u8; 4];
                                    let ch = char::from_u32(cp).unwrap_or('?');
                                    bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                                }
                                other => bytes.push(other),
                            }
                        }
                        other => bytes.push(other),
                    }
                }
                String::from_utf8(bytes).map(Json::Str).map_err(|e| e.to_string())
            }
            b't' => self.lit("true", Json::Bool(true)),
            b'f' => self.lit("false", Json::Bool(false)),
            b'n' => self.lit("null", Json::Null),
            _ => {
                let start = self.i;
                while self.i < self.b.len() && b"+-0123456789.eE".contains(&self.b[self.i]) {
                    self.i += 1;
                }
                let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
                s.parse::<f64>().map(Json::Num).map_err(|e| format!("number {s:?}: {e}"))
            }
        }
    }
}
