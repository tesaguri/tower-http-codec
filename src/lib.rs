//! Collection of Tower middlewares to handle HTTP `Content-Encoding`.

#![cfg_attr(docs, feature(doc_cfg))]
#![warn(missing_docs)]

pub mod decode;

pub use decode::{Decode, DecodeBody, DecodeLayer};

use std::error;
use std::fmt::{self, Display, Formatter};
use std::io;

/// Error type for the body types.
#[derive(Debug)]
pub enum Error<E> {
    /// Error from the underlying body.
    Body(E),
    /// Compression/decompression error.
    Compression(io::Error),
}

impl<E: Display> Display for Error<E> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match *self {
            Error::Body(ref e) => e.fmt(f),
            Error::Compression(ref e) => e.fmt(f),
        }
    }
}

impl<E: error::Error + 'static> error::Error for Error<E> {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            Error::Body(ref e) => Some(e),
            Error::Compression(ref e) => Some(e),
        }
    }
}
