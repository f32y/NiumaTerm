use std::{error, fmt, result};

use libghostty_vt_sys::Result as VtResult;

pub type Result<T> = result::Result<T, Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    OutOfMemory,
    InvalidValue,
    OutOfSpace,
    NoValue,
    Unknown(i32),
}

impl Error {
    pub(super) fn from_code(code: VtResult::Type) -> Result<()> {
        match code {
            VtResult::SUCCESS => Ok(()),
            VtResult::OUT_OF_MEMORY => Err(Self::OutOfMemory),
            VtResult::INVALID_VALUE => Err(Self::InvalidValue),
            VtResult::OUT_OF_SPACE => Err(Self::OutOfSpace),
            VtResult::NO_VALUE => Err(Self::NoValue),
            other => Err(Self::Unknown(other)),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfMemory => f.write_str("libghostty-vt allocation failed"),
            Self::InvalidValue => {
                f.write_str("libghostty-vt received or returned an invalid value")
            }
            Self::OutOfSpace => f.write_str("buffer is too small for libghostty-vt output"),
            Self::NoValue => f.write_str("libghostty-vt value is absent"),
            Self::Unknown(code) => write!(f, "unknown libghostty-vt result code {code}"),
        }
    }
}

impl error::Error for Error {}
