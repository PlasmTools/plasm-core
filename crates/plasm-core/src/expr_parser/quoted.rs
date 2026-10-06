//! One quoted-string lexer for scalar and data-expression syntax.
use super::{ParseError, ParseErrorKind};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_failures_preserve_semantic_metadata_and_offset() {
        let mut offset = 0;
        let error = parse(r#""\u12z4""#, &mut offset).unwrap_err();
        assert!(matches!(
            error.kind,
            ParseErrorKind::InvalidUnicodeEscape {
                digit_index: 2,
                got: Some('z')
            }
        ));
        assert_eq!(error.offset, 6);

        let mut offset = 0;
        let error = parse(r#""\uD800\u0041""#, &mut offset).unwrap_err();
        assert!(matches!(
            error.kind,
            ParseErrorKind::InvalidLowSurrogate { unit: 0x41 }
        ));
        assert_eq!(error.offset, 13);
    }
}

pub(super) fn parse(input: &str, offset: &mut usize) -> Result<String, ParseError> {
    fn next(input: &str, offset: &mut usize) -> Option<char> {
        let c = input[*offset..].chars().next()?;
        *offset += c.len_utf8();
        Some(c)
    }
    let error = |kind, offset| ParseError { kind, offset };
    let quote =
        next(input, offset).ok_or_else(|| error(ParseErrorKind::UnterminatedString, *offset))?;
    debug_assert!(matches!(quote, '"' | '\''));
    let mut out = String::new();
    loop {
        match next(input, offset) {
            None => return Err(error(ParseErrorKind::UnterminatedString, *offset)),
            Some(c) if c == quote => return Ok(out),
            Some('\\') => {
                let esc = next(input, offset)
                    .ok_or_else(|| error(ParseErrorKind::UnterminatedEscape, *offset))?;
                if let Some(c) = crate::string_unescape::json_escape_simple(esc) {
                    out.push(c);
                } else if esc == 'u' {
                    let read_unit = |offset: &mut usize| -> Result<u16, ParseError> {
                        let mut value = 0u16;
                        for digit_index in 0..4 {
                            let got = next(input, offset);
                            let digit = got.and_then(|c| c.to_digit(16));
                            let Some(digit) = digit else {
                                return Err(error(
                                    ParseErrorKind::InvalidUnicodeEscape { digit_index, got },
                                    *offset,
                                ));
                            };
                            value = (value << 4) | digit as u16;
                        }
                        Ok(value)
                    };
                    let first = read_unit(offset)?;
                    let codepoint = if (0xD800..=0xDBFF).contains(&first) {
                        if next(input, offset) != Some('\\') || next(input, offset) != Some('u') {
                            return Err(error(
                                ParseErrorKind::MissingLowSurrogate { high: first },
                                *offset,
                            ));
                        }
                        let second = read_unit(offset)?;
                        if !(0xDC00..=0xDFFF).contains(&second) {
                            return Err(error(
                                ParseErrorKind::InvalidLowSurrogate { unit: second },
                                *offset,
                            ));
                        }
                        0x10000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(second) - 0xDC00)
                    } else {
                        u32::from(first)
                    };
                    let c = char::from_u32(codepoint).ok_or_else(|| {
                        error(
                            ParseErrorKind::InvalidUnicodeCodepoint { codepoint },
                            *offset,
                        )
                    })?;
                    out.push(c);
                } else {
                    return Err(error(
                        ParseErrorKind::UnknownEscape { escape: esc },
                        *offset,
                    ));
                }
            }
            Some(c) => out.push(c),
        }
    }
}
