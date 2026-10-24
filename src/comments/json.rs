use super::{CommentError, CommentSide, ReviewComment};

const MAX_JSON_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum JsonVal {
    Null,
    Bool(bool),
    Number(i64),
    Str(String),
    Array(Vec<JsonVal>),
    Object(Vec<(String, JsonVal)>),
}

impl JsonVal {
    pub(super) fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    pub(super) fn as_usize(&self) -> Option<usize> {
        match self {
            Self::Number(n) if *n > 0 => usize::try_from(*n).ok(),
            _ => None,
        }
    }

    pub(super) fn as_array(&self) -> Option<&[JsonVal]> {
        match self {
            Self::Array(arr) => Some(arr),
            _ => None,
        }
    }

    pub(super) fn as_object(&self) -> Option<&[(String, JsonVal)]> {
        match self {
            Self::Object(entries) => Some(entries),
            _ => None,
        }
    }

    pub(super) fn get(&self, key: &str) -> Option<&JsonVal> {
        self.as_object()?.iter().find_map(|(k, v)| if k == key { Some(v) } else { None })
    }
}

pub(super) fn parse_json_stream(input: &str) -> Result<Vec<JsonVal>, CommentError> {
    JsonParser::new(input).parse_stream()
}

struct JsonParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn new(input: &'a str) -> Self {
        Self { bytes: input.as_bytes(), pos: 0 }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn parse_stream(&mut self) -> Result<Vec<JsonVal>, CommentError> {
        let mut values = Vec::new();
        loop {
            self.skip_ws();
            if self.pos >= self.bytes.len() {
                break;
            }
            values.push(self.parse_value(0)?);
        }
        Ok(values)
    }

    fn parse_value(&mut self, depth: usize) -> Result<JsonVal, CommentError> {
        if depth > MAX_JSON_DEPTH {
            return Err(CommentError::Parse("JSON nesting exceeds maximum depth".into()));
        }
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.parse_object(depth + 1),
            Some(b'[') => self.parse_array(depth + 1),
            Some(b'"') => self.parse_string().map(JsonVal::Str),
            Some(b't') => self.parse_literal(b"true", JsonVal::Bool(true)),
            Some(b'f') => self.parse_literal(b"false", JsonVal::Bool(false)),
            Some(b'n') => self.parse_literal(b"null", JsonVal::Null),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            Some(ch) => Err(CommentError::Parse(format!(
                "unexpected byte '{}' at offset {}",
                ch as char, self.pos
            ))),
            None => Err(CommentError::Parse("unexpected end of JSON input".into())),
        }
    }

    fn parse_literal(&mut self, lit: &[u8], val: JsonVal) -> Result<JsonVal, CommentError> {
        if self.bytes.get(self.pos..self.pos + lit.len()) == Some(lit) {
            self.pos += lit.len();
            Ok(val)
        } else {
            Err(CommentError::Parse(format!("invalid JSON literal at offset {}", self.pos)))
        }
    }

    fn parse_number(&mut self) -> Result<JsonVal, CommentError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let digits_start = self.pos;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        if self.pos == digits_start {
            return Err(CommentError::Parse(format!("invalid number at offset {start}")));
        }
        let int_end = self.pos;
        if self.peek() == Some(b'.') {
            self.pos += 1;
            let frac_start = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            if self.pos == frac_start {
                return Err(CommentError::Parse(format!("invalid decimal at offset {start}")));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exp_start = self.pos;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            if self.pos == exp_start {
                return Err(CommentError::Parse(format!("invalid exponent at offset {start}")));
            }
        }
        let int_str = std::str::from_utf8(&self.bytes[start..int_end])
            .map_err(|_| CommentError::Parse("invalid UTF-8 in number".into()))?;
        Ok(JsonVal::Number(int_str.parse::<i64>().unwrap_or(0)))
    }

    fn parse_string(&mut self) -> Result<String, CommentError> {
        if self.peek() != Some(b'"') {
            return Err(CommentError::Parse(format!("expected '\"' at offset {}", self.pos)));
        }
        self.pos += 1;
        let mut out = String::new();
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            self.pos += 1;
            match b {
                b'"' => return Ok(out),
                b'\\' => self.parse_escape(&mut out)?,
                0..=0x1f => {
                    return Err(CommentError::Parse(
                        "unescaped control char in JSON string".into(),
                    ));
                }
                _ => {
                    let char_start = self.pos - 1;
                    while self.pos < self.bytes.len() {
                        let next = self.bytes[self.pos];
                        if next == b'"' || next == b'\\' || next <= 0x1f {
                            break;
                        }
                        self.pos += 1;
                    }
                    let slice = std::str::from_utf8(&self.bytes[char_start..self.pos])
                        .map_err(|_| CommentError::Parse("invalid UTF-8 in JSON string".into()))?;
                    out.push_str(slice);
                }
            }
        }
        Err(CommentError::Parse("unterminated JSON string".into()))
    }

    fn parse_escape(&mut self, out: &mut String) -> Result<(), CommentError> {
        let Some(esc) = self.peek() else {
            return Err(CommentError::Parse("unterminated escape in JSON string".into()));
        };
        self.pos += 1;
        match esc {
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'/' => out.push('/'),
            b'b' => out.push('\u{0008}'),
            b'f' => out.push('\u{000c}'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'u' => {
                let cp = self.parse_hex4()?;
                if (0xD800..=0xDBFF).contains(&cp) {
                    if self.bytes.get(self.pos..self.pos + 2) != Some(b"\\u") {
                        return Err(CommentError::Parse("unpaired UTF-16 high surrogate".into()));
                    }
                    self.pos += 2;
                    let low = self.parse_hex4()?;
                    if !(0xDC00..=0xDFFF).contains(&low) {
                        return Err(CommentError::Parse("invalid UTF-16 low surrogate".into()));
                    }
                    let combined =
                        0x10000 + ((u32::from(cp) - 0xD800) << 10) + (u32::from(low) - 0xDC00);
                    out.push(
                        char::from_u32(combined)
                            .ok_or_else(|| CommentError::Parse("invalid surrogate".into()))?,
                    );
                } else {
                    out.push(
                        char::from_u32(u32::from(cp))
                            .ok_or_else(|| CommentError::Parse("invalid unicode escape".into()))?,
                    );
                }
            }
            _ => return Err(CommentError::Parse(format!("invalid escape '\\{}'", esc as char))),
        }
        Ok(())
    }

    fn parse_hex4(&mut self) -> Result<u16, CommentError> {
        let Some(slice) = self.bytes.get(self.pos..self.pos + 4) else {
            return Err(CommentError::Parse("short \\u escape".into()));
        };
        let hex_str = std::str::from_utf8(slice)
            .map_err(|_| CommentError::Parse("invalid hex in \\u escape".into()))?;
        let val = u16::from_str_radix(hex_str, 16)
            .map_err(|_| CommentError::Parse("invalid hex digits in \\u escape".into()))?;
        self.pos += 4;
        Ok(val)
    }

    fn parse_array(&mut self, depth: usize) -> Result<JsonVal, CommentError> {
        self.pos += 1;
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(JsonVal::Array(items));
        }
        loop {
            items.push(self.parse_value(depth)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(JsonVal::Array(items));
                }
                _ => return Err(CommentError::Parse("expected ',' or ']' in array".into())),
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<JsonVal, CommentError> {
        self.pos += 1;
        self.skip_ws();
        let mut entries = Vec::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(JsonVal::Object(entries));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(CommentError::Parse("expected ':' after object key".into()));
            }
            self.pos += 1;
            entries.push((key, self.parse_value(depth)?));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(JsonVal::Object(entries));
                }
                _ => return Err(CommentError::Parse("expected ',' or '}' in object".into())),
            }
        }
    }
}

pub fn comment_to_json_line(comment: &ReviewComment) -> String {
    let mut out = String::from("{\"path\":");
    push_json_str(&mut out, &comment.path);
    match (comment.start_line, comment.end_line) {
        (Some(s), Some(e)) if s == e => out.push_str(&format!(",\"line\":{s}")),
        (Some(s), Some(e)) => {
            out.push_str(&format!(",\"start_line\":{},\"end_line\":{}", s.min(e), s.max(e)));
        }
        (Some(s), None) | (None, Some(s)) => out.push_str(&format!(",\"line\":{s}")),
        (None, None) => {}
    }
    let side_str = match comment.side {
        CommentSide::New => "new",
        CommentSide::Old => "old",
    };
    out.push_str(",\"side\":\"");
    out.push_str(side_str);
    out.push_str("\",\"body\":");
    push_json_str(&mut out, &comment.body);
    if let Some(ref author) = comment.author {
        out.push_str(",\"author\":");
        push_json_str(&mut out, author);
    }
    if let Some(ref suggestion) = comment.suggestion {
        out.push_str(",\"suggestion\":");
        push_json_str(&mut out, suggestion);
    }
    if comment.outdated {
        out.push_str(",\"outdated\":true");
    }
    out.push('}');
    out
}

fn push_json_str(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            c if (c as u32) <= 0x1f => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn parse_one(input: &str) -> Result<JsonVal, CommentError> {
        let mut values = parse_json_stream(input)?;
        assert_eq!(values.len(), 1, "{input:?} held {} values", values.len());
        Ok(values.remove(0))
    }

    fn str_of(input: &str) -> String {
        match parse_one(input).unwrap() {
            JsonVal::Str(s) => s,
            other => panic!("{input:?} parsed to {other:?}"),
        }
    }

    fn from_serde(value: &serde_json::Value) -> JsonVal {
        match value {
            serde_json::Value::Null => JsonVal::Null,
            serde_json::Value::Bool(b) => JsonVal::Bool(*b),
            serde_json::Value::Number(n) => JsonVal::Number(n.as_i64().unwrap()),
            serde_json::Value::String(s) => JsonVal::Str(s.clone()),
            serde_json::Value::Array(items) => {
                JsonVal::Array(items.iter().map(from_serde).collect())
            }
            serde_json::Value::Object(map) => {
                JsonVal::Object(map.iter().map(|(k, v)| (k.clone(), from_serde(v))).collect())
            }
        }
    }

    /// Writes `value` with every non-ASCII and control character as a `\u` escape,
    /// so characters outside the Basic Multilingual Plane become surrogate pairs.
    fn escape_everything(value: &serde_json::Value) -> String {
        fn string(out: &mut String, s: &str) {
            out.push('"');
            for unit in s.encode_utf16() {
                match unit {
                    0x22 => out.push_str("\\\""),
                    0x5c => out.push_str("\\\\"),
                    0x20..=0x7e => out.push(char::from_u32(u32::from(unit)).unwrap()),
                    _ => out.push_str(&format!("\\u{unit:04X}")),
                }
            }
            out.push('"');
        }
        let mut out = String::new();
        match value {
            serde_json::Value::String(s) => string(&mut out, s),
            serde_json::Value::Array(items) => {
                out.push_str("[ ");
                let parts: Vec<String> = items.iter().map(escape_everything).collect();
                out.push_str(&parts.join(" ,\n"));
                out.push_str(" ]");
            }
            serde_json::Value::Object(map) => {
                out.push_str("{\t");
                let parts: Vec<String> = map
                    .iter()
                    .map(|(k, v)| {
                        let mut key = String::new();
                        string(&mut key, k);
                        format!("{key} :\r\n{}", escape_everything(v))
                    })
                    .collect();
                out.push_str(&parts.join(","));
                out.push_str("\t}");
            }
            other => out.push_str(&other.to_string()),
        }
        out
    }

    fn arb_json() -> impl Strategy<Value = serde_json::Value> {
        let leaf = prop_oneof![
            Just(serde_json::Value::Null),
            any::<bool>().prop_map(serde_json::Value::Bool),
            any::<i64>().prop_map(serde_json::Value::from),
            any::<String>().prop_map(serde_json::Value::String),
        ];
        leaf.prop_recursive(4, 32, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(serde_json::Value::Array),
                prop::collection::btree_map(any::<String>(), inner, 0..6)
                    .prop_map(|m| serde_json::Value::Object(m.into_iter().collect())),
            ]
        })
    }

    proptest! {
        /// Differential: whatever serde_json writes, compact, pretty or with every
        /// character escaped, parses to the same value.
        #[test]
        fn parses_what_serde_json_writes(value in arb_json()) {
            let expected = from_serde(&value);
            for text in [
                serde_json::to_string(&value).unwrap(),
                serde_json::to_string_pretty(&value).unwrap(),
                escape_everything(&value),
            ] {
                prop_assert_eq!(parse_one(&text).unwrap(), expected.clone(), "{}", text);
            }
        }

        /// Roundtrip: a comment written as a JSON line parses back to its strings.
        #[test]
        fn comment_json_lines_parse_back(body in any::<String>(), path in any::<String>()) {
            let comment = ReviewComment {
                path: path.clone(),
                start_line: Some(4),
                end_line: Some(4),
                side: CommentSide::New,
                body: body.clone(),
                author: None,
                suggestion: None,
                outdated: false,
            };
            let parsed = parse_one(&comment_to_json_line(&comment)).unwrap();
            prop_assert_eq!(parsed.get("body").and_then(JsonVal::as_str), Some(body.as_str()));
            prop_assert_eq!(parsed.get("path").and_then(JsonVal::as_str), Some(path.as_str()));
        }
    }

    #[test]
    fn every_escape_decodes_to_its_character() {
        assert_eq!(str_of(r#""\"\\\/\b\f\n\r\t""#), "\"\\/\u{8}\u{c}\n\r\t");
        assert_eq!(str_of(r#""caf\u00e9 \u00E9""#), "café é");
        assert_eq!(str_of(r#""\ud83d\ude00 x""#), "😀 x");
        assert_eq!(str_of(r#""\ud800\udc00\udbff\udfff""#), "\u{10000}\u{10FFFF}");
        assert_eq!(str_of(r#""a\u0041b""#), "aAb");
    }

    #[test]
    fn broken_escapes_and_strings_are_errors() {
        for bad in [
            r#""\ud83d""#, r#""\ud83d x""#, r#""\ud83dA""#, r#""\ud83d\n""#, r#""\u12""#,
            r#""\u12"#, r#""\uzzzz""#, r#""\x""#, "\"\\", "\"abc", "\"abc\\\"", "\"a\u{1}b\"",
            "\"\u{1f}\"", "\"tab\there\"",
        ] {
            assert!(parse_json_stream(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn numbers_keep_their_integer_part() {
        assert_eq!(
            parse_one("[0, -5, 12, 1.25, -3e+2, 4E-1, 5.0e10, 7e3, 8]").unwrap(),
            JsonVal::Array(
                [0, -5, 12, 1, -3, 4, 5, 7, 8].into_iter().map(JsonVal::Number).collect()
            )
        );
        assert_eq!(parse_one("{\"a\":1.5,\"b\":2}").unwrap().get("b"), Some(&JsonVal::Number(2)));
        for bad in ["-", "1.", "1.e5", "1e", "1e+", "-x", "[1.]", "[2e-]"] {
            assert!(parse_json_stream(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn empty_containers_end_where_they_close() {
        assert_eq!(
            parse_one("[[],{},[ ],{ },\"x\"]").unwrap(),
            JsonVal::Array(vec![
                JsonVal::Array(vec![]),
                JsonVal::Object(vec![]),
                JsonVal::Array(vec![]),
                JsonVal::Object(vec![]),
                JsonVal::Str("x".into()),
            ])
        );
        assert_eq!(
            parse_json_stream("{}[]").unwrap(),
            vec![JsonVal::Object(vec![]), JsonVal::Array(vec![])]
        );
    }

    #[test]
    fn nesting_is_allowed_to_exactly_the_maximum_depth() {
        let arrays = |n: usize| format!("{}0{}", "[".repeat(n), "]".repeat(n));
        let objects = |n: usize| format!("{}0{}", "{\"a\":".repeat(n), "}".repeat(n));
        assert!(parse_json_stream(&arrays(MAX_JSON_DEPTH)).is_ok());
        assert!(parse_json_stream(&arrays(MAX_JSON_DEPTH + 1)).is_err());
        assert!(parse_json_stream(&objects(MAX_JSON_DEPTH)).is_ok());
        assert!(parse_json_stream(&objects(MAX_JSON_DEPTH + 1)).is_err());
    }

    #[test]
    fn only_positive_numbers_are_line_numbers() {
        assert_eq!(JsonVal::Number(1).as_usize(), Some(1));
        assert_eq!(JsonVal::Number(0).as_usize(), None);
        assert_eq!(JsonVal::Number(-3).as_usize(), None);
        assert_eq!(JsonVal::Str("1".into()).as_usize(), None);
    }

    #[test]
    fn a_one_line_comment_is_written_with_line() {
        let mut comment = ReviewComment {
            path: "a.rs".into(),
            start_line: Some(5),
            end_line: Some(5),
            side: CommentSide::Old,
            body: "b".into(),
            author: None,
            suggestion: None,
            outdated: false,
        };
        assert_eq!(
            comment_to_json_line(&comment),
            r#"{"path":"a.rs","line":5,"side":"old","body":"b"}"#
        );
        comment.end_line = Some(3);
        assert_eq!(
            comment_to_json_line(&comment),
            r#"{"path":"a.rs","start_line":3,"end_line":5,"side":"old","body":"b"}"#
        );
    }
}
