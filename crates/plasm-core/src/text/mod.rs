//! UTF-8 text carriers and template IR for program literals and wire payloads.

mod brace_template;
mod dollar_template;
mod utf8_text;

pub use brace_template::{parse_brace_template, BraceParseError, BraceSegment, BraceTemplate};
pub use dollar_template::{
    interpolate_dollar_template, parse_dollar_template, DollarParseError, DollarSegment,
    DollarTemplate, InterpolateError, DEFAULT_MAX_INTERPOLATED_LEN,
};
pub use utf8_text::{Utf8FromBytesError, Utf8Text};
