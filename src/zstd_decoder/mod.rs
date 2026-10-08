//! Private bounded Zstandard decoder, based on ms-compress-ruzstd 0.9.1.
//! Copyright and MIT license are retained in LICENSE.
//! The block decoder validates literal/sequence/output ceilings before growth.
#![allow(dead_code, missing_docs, unused_imports, clippy::all)]
// The audited upstream implementation retains its internal style and APIs.
#[cfg(feature = "std")]
pub(crate) const VERBOSE: bool = false;
macro_rules! vprintln {
    ($($x:expr),*) => {
        #[cfg(feature = "std")]
        if crate::zstd_decoder::VERBOSE { std::println!($($x),*); }
    }
}
mod bit_io;
mod blocks;
mod checksum;
mod common;
pub(crate) mod decoding;
mod fse;
mod huff0;
#[cfg(feature = "std")]
pub(crate) mod io_std;
#[cfg(feature = "std")]
pub(crate) use io_std as io;
#[cfg(not(feature = "std"))]
pub(crate) mod io_nostd;
#[cfg(not(feature = "std"))]
pub(crate) use io_nostd as io;
