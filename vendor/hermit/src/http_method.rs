// not tested, as this is a relatively trivial enum
// and tests would mostly involve just testing the Rust language itself
#[cfg_attr(test, mutants::skip)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Options,
    Head,
    Trace,
}

#[cfg_attr(test, mutants::skip)] // not tested
impl HttpMethod {
    pub const ALL: &'static [Self] = &[
        Self::Get,
        Self::Post,
        Self::Put,
        Self::Patch,
        Self::Delete,
        Self::Options,
        Self::Head,
        Self::Trace,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Post => "post",
            Self::Put => "put",
            Self::Patch => "patch",
            Self::Delete => "delete",
            Self::Options => "options",
            Self::Head => "head",
            Self::Trace => "trace",
        }
    }

    pub fn uses_request_body(self) -> bool {
        matches!(self, Self::Post | Self::Put | Self::Patch)
    }
}

#[cfg_attr(test, mutants::skip)] // not tested
impl TryFrom<&str> for HttpMethod {
    type Error = ();

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "get" => Ok(Self::Get),
            "post" => Ok(Self::Post),
            "put" => Ok(Self::Put),
            "patch" => Ok(Self::Patch),
            "delete" => Ok(Self::Delete),
            "options" => Ok(Self::Options),
            "head" => Ok(Self::Head),
            "trace" => Ok(Self::Trace),
            _ => Err(()),
        }
    }
}
