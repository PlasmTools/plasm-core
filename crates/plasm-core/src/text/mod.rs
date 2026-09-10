//! UTF-8 text carriers and brace templates for overlay / workflow holes.
//!
//! Program string expansion lives in [`crate::program_string_template`] (Minijinja).

mod brace_template;
mod utf8_text;

pub use brace_template::{parse_brace_template, BraceParseError, BraceSegment, BraceTemplate};
pub use utf8_text::{Utf8FromBytesError, Utf8Text};
