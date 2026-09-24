//! Closed data-expression grammar for bindings and pure per-row derivation.
//! Syntax is parsed before scope resolution; unknown syntax never becomes text.
use crate::operand_binding::ResolvedValue;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum DataExpr {
    Literal(ResolvedValue),
    Reference { root: String, path: Vec<String> },
    Object(BTreeMap<String, DataExpr>),
    Array(Vec<DataExpr>),
}

pub fn parse_data_expression(text: &str) -> Result<DataExpr, String> {
    let mut parser = DataParser { text, offset: 0 };
    let value = parser.value()?;
    parser.space();
    if parser.offset != text.len() {
        return Err(parser.error("unexpected syntax after data expression"));
    }
    Ok(value)
}

struct DataParser<'a> {
    text: &'a str,
    offset: usize,
}
impl DataParser<'_> {
    fn error(&self, reason: &str) -> String {
        format!("data expression at byte {}: {reason}; use literals or declared field/binding references. Bind catalog reads and relation traversals as separate rowset operations", self.offset)
    }
    fn peek(&self) -> Option<char> {
        self.text[self.offset..].chars().next()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.offset += c.len_utf8();
        Some(c)
    }
    fn space(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.bump();
        }
    }
    fn expect(&mut self, c: char) -> Result<(), String> {
        self.space();
        if self.bump() == Some(c) {
            Ok(())
        } else {
            Err(self.error(&format!("expected `{c}`")))
        }
    }
    fn identifier(&mut self) -> Result<String, String> {
        let start = self.offset;
        if !self
            .peek()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        {
            return Err(self.error("expected identifier"));
        }
        self.bump();
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.bump();
        }
        Ok(self.text[start..self.offset].into())
    }
    fn quoted(&mut self) -> Result<String, String> {
        super::quoted::parse(self.text, &mut self.offset).map_err(|e| e.to_string())
    }
    fn value(&mut self) -> Result<DataExpr, String> {
        self.space();
        match self.peek() {
            Some('{') => {
                self.bump();
                let mut fields = BTreeMap::new();
                self.space();
                if self.peek() == Some('}') {
                    self.bump();
                    return Ok(DataExpr::Object(fields));
                }
                loop {
                    self.space();
                    let key = if matches!(self.peek(), Some('"' | '\'')) {
                        self.quoted()?
                    } else {
                        self.identifier()?
                    };
                    self.expect(':')?;
                    let value = self.value()?;
                    if fields.insert(key.clone(), value).is_some() {
                        return Err(self.error(&format!("duplicate object key `{key}`")));
                    }
                    self.space();
                    if self.peek() == Some('}') {
                        self.bump();
                        break;
                    }
                    self.expect(',')?;
                }
                Ok(DataExpr::Object(fields))
            }
            Some('[') => {
                self.bump();
                let mut items = Vec::new();
                self.space();
                if self.peek() == Some(']') {
                    self.bump();
                    return Ok(DataExpr::Array(items));
                }
                loop {
                    items.push(self.value()?);
                    self.space();
                    if self.peek() == Some(']') {
                        self.bump();
                        break;
                    }
                    self.expect(',')?;
                }
                Ok(DataExpr::Array(items))
            }
            Some('"' | '\'') => {
                let text = self.quoted()?;
                Ok(DataExpr::Literal(
                    ResolvedValue::new(crate::Value::String(text)).map_err(str::to_owned)?,
                ))
            }
            Some(c) if c == '-' || c.is_ascii_digit() => {
                let start = self.offset;
                while self
                    .peek()
                    .is_some_and(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'))
                {
                    self.bump();
                }
                let literal = serde_json::from_str::<ResolvedValue>(&self.text[start..self.offset])
                    .map_err(|e| self.error(&format!("invalid number: {e}")))?;
                Ok(DataExpr::Literal(literal))
            }
            Some(_) => {
                let root = self.identifier()?;
                if matches!(root.as_str(), "null" | "true" | "false") {
                    return serde_json::from_str(&root)
                        .map(DataExpr::Literal)
                        .map_err(|e| self.error(&e.to_string()));
                }
                let mut path = Vec::new();
                while self.peek() == Some('.') {
                    self.bump();
                    path.push(self.identifier()?);
                }
                Ok(DataExpr::Reference { root, path })
            }
            None => Err(self.error("expected value")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn closed_grammar_rejects_executable_and_malformed_values() {
        for text in [
            "e11(_.id).r6",
            "{a:[Thing(_.id)]}",
            "_.x | f()",
            "_.x()",
            "_.",
            "{a:1,a:2}",
            "[1] garbage",
        ] {
            let parsed = parse_data_expression(text);
            assert!(parsed.is_err(), "{text}: {parsed:?}");
        }
    }
    #[test]
    fn quote_styles_share_escape_laws() {
        for text in [r#"'a\n\u0042'"#, r#""a\n\u0042""#] {
            let DataExpr::Literal(value) = parse_data_expression(text).unwrap() else {
                panic!("literal")
            };
            assert_eq!(value.as_str(), Some("a\nB"));
        }
        for text in [r#"'\q'"#, r#""\q""#] {
            assert!(parse_data_expression(text).is_err());
        }
    }
    #[test]
    fn malformed_surrogate_sequences_are_rejected() {
        for text in [
            r#""\uD800""#,
            r#""\uDC00""#,
            r#""\uD800\u0041""#,
            r#""\uD800\uD800""#,
        ] {
            assert!(parse_data_expression(text).is_err(), "{text}");
        }
    }
    proptest! {
        #[test]
        fn surrogate_pair_escape_matches_json(codepoint in 0x10000u32..=0x10FFFF) {
            let n = codepoint - 0x10000;
            let wire = format!("\"\\u{:04X}\\u{:04X}\"", 0xD800 + (n >> 10), 0xDC00 + (n & 0x3FF));
            let DataExpr::Literal(value) = parse_data_expression(&wire).unwrap() else { panic!("literal") };
            let expected: String = serde_json::from_str(&wire).unwrap();
            prop_assert_eq!(value.as_str(), Some(expected.as_str()));
            let single_quoted = format!("'{}'", &wire[1..wire.len()-1]);
            prop_assert_eq!(parse_data_expression(&single_quoted).unwrap(), DataExpr::Literal(value));
        }

        #[test]
        fn quoted_text_never_becomes_operational(text in ".{0,120}") {
            let wire = serde_json::to_string(&text).unwrap();
            let DataExpr::Literal(value) = parse_data_expression(&wire).unwrap() else { panic!("literal") };
            prop_assert_eq!(value.as_str(), Some(text.as_str()));
            let roundtrip: ResolvedValue = serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap();
            prop_assert_eq!(roundtrip, value);
        }
    }
}
