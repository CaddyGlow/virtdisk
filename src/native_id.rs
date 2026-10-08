//! Version-4 identities in the little-endian UUID/GUID layout used by VDI and VHDX.
use crate::io;

pub(crate) fn identity() -> io::Result<[u8; 16]> {
    let mut id = [0; 16];
    getrandom::fill(&mut id).map_err(|error| io::Error::other(error.to_string()))?;
    id[7] = (id[7] & 15) | 64;
    id[8] = (id[8] & 63) | 128;
    Ok(id)
}
