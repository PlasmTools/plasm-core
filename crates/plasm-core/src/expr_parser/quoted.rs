//! One quoted-string lexer for scalar and data-expression syntax.
use super::{ParseError, ParseErrorKind};

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
                        for _ in 0..4 {
                            let digit = next(input, offset).and_then(|c| c.to_digit(16));
                            let Some(digit) = digit else {
                                return Err(error(
                                    ParseErrorKind::Other {
                                        message: "invalid unicode escape: need four hex digits"
                                            .into(),
                                    },
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
                                ParseErrorKind::Other {
                                    message:
                                        "high surrogate requires a low-surrogate unicode escape"
                                            .into(),
                                },
                                *offset,
                            ));
                        }
                        let second = read_unit(offset)?;
                        if !(0xDC00..=0xDFFF).contains(&second) {
                            return Err(error(
                                ParseErrorKind::Other {
                                    message: "invalid low surrogate".into(),
                                },
                                *offset,
                            ));
                        }
                        0x10000 + ((u32::from(first) - 0xD800) << 10) + (u32::from(second) - 0xDC00)
                    } else {
                        u32::from(first)
                    };
                    let c = char::from_u32(codepoint).ok_or_else(|| {
                        error(
                            ParseErrorKind::Other {
                                message: "invalid unicode code point".into(),
                            },
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
