// Constants

const MAX_DEPTH: usize = 64;

// Types

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}

// Access

impl Json {
    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Self::Object(members) => members
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }
}

// Parsing

pub(crate) fn parse(text: &str) -> Option<Json> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        position: 0,
    };
    let value = parser.value(0)?;
    parser.skip_whitespace();
    (parser.position == parser.bytes.len()).then_some(value)
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.position += 1;
        }
    }

    fn expect(&mut self, literal: &str) -> Option<()> {
        let end = self.position + literal.len();
        (self.bytes.get(self.position..end) == Some(literal.as_bytes())).then(|| {
            self.position = end;
        })
    }

    fn value(&mut self, depth: usize) -> Option<Json> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_whitespace();
        match self.peek()? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Json::String),
            b't' => self.expect("true").map(|()| Json::Bool(true)),
            b'f' => self.expect("false").map(|()| Json::Bool(false)),
            b'n' => self.expect("null").map(|()| Json::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn object(&mut self, depth: usize) -> Option<Json> {
        self.position += 1;
        let mut members = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Some(Json::Object(members));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return None;
            }
            let key = self.string()?;
            self.skip_whitespace();
            if self.next()? != b':' {
                return None;
            }
            let value = self.value(depth + 1)?;
            members.push((key, value));
            self.skip_whitespace();
            match self.next()? {
                b',' => {}
                b'}' => return Some(Json::Object(members)),
                _ => return None,
            }
        }
    }

    fn array(&mut self, depth: usize) -> Option<Json> {
        self.position += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.position += 1;
            return Some(Json::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.next()? {
                b',' => {}
                b']' => return Some(Json::Array(items)),
                _ => return None,
            }
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        match self.next()? {
            b'0' => {}
            b'1'..=b'9' => self.digits(),
            _ => return None,
        }
        if self.peek() == Some(b'.') {
            self.position += 1;
            self.required_digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            self.required_digits()?;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.position]).ok()?;
        Some(Json::Number(text.to_string()))
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
    }

    fn required_digits(&mut self) -> Option<()> {
        let start = self.position;
        self.digits();
        (self.position > start).then_some(())
    }

    fn string(&mut self) -> Option<String> {
        self.position += 1;
        let mut bytes = Vec::new();
        loop {
            match self.next()? {
                b'"' => return String::from_utf8(bytes).ok(),
                b'\\' => {
                    let character = match self.next()? {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode_escape()?,
                        _ => return None,
                    };
                    let mut buffer = [0; 4];
                    bytes.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                }
                byte if byte < 0x20 => return None,
                byte => bytes.push(byte),
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let digits = self.bytes.get(self.position..self.position + 4)?;
        let text = std::str::from_utf8(digits).ok()?;
        if !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        self.position += 4;
        u32::from_str_radix(text, 16).ok()
    }

    fn unicode_escape(&mut self) -> Option<char> {
        let first = self.hex4()?;
        if (0xD800..0xDC00).contains(&first) {
            self.expect("\\u")?;
            let second = self.hex4()?;
            if !(0xDC00..0xE000).contains(&second) {
                return None;
            }
            return char::from_u32(0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00));
        }
        char::from_u32(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_parse() {
        let value = parse(
            r#" { "a": [1, -2.5e3, true, false, null], "b": { "c": "x\"y\\z\/\n\u00e4\ud83d\ude00" }, "d": {} , "e": [] } "#,
        )
        .expect("parsing failed");
        assert_eq!(
            value.get("a"),
            Some(&Json::Array(vec![
                Json::Number("1".to_string()),
                Json::Number("-2.5e3".to_string()),
                Json::Bool(true),
                Json::Bool(false),
                Json::Null,
            ]))
        );
        assert_eq!(
            value.get("b").and_then(|b| b.get("c")).and_then(Json::as_str),
            Some("x\"y\\z/\nä😀")
        );
        assert_eq!(value.get("d"), Some(&Json::Object(Vec::new())));
        assert_eq!(value.get("missing"), None);
    }

    #[test]
    fn invalid_documents_are_rejected() {
        for text in [
            "",
            "{",
            "[1,]",
            "{\"a\" 1}",
            "{\"a\":1,}",
            "\"open",
            "01",
            "1.",
            "-",
            "tru",
            "\"\\x\"",
            "\"\\ud83d\"",
            "\"a\nb\"",
            "[] []",
            "{a:1}",
        ] {
            assert_eq!(parse(text), None, "{text:?} parsed");
        }
    }

    #[test]
    fn deep_nesting_is_rejected() {
        let deep = format!("{}{}", "[".repeat(100), "]".repeat(100));
        assert_eq!(parse(&deep), None);
        let fine = format!("{}{}", "[".repeat(20), "]".repeat(20));
        assert!(parse(&fine).is_some());
    }
}
