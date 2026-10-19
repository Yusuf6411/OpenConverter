use crate::Context;
use crate::entry::Archive;
use oc_core::{Error, ErrorCode, Result};
pub fn read(_data: &[u8], _cx: &Context) -> Result<Archive> {
    Err(Error::new(ErrorCode::UnsupportedVariant, "not implemented yet"))
}
