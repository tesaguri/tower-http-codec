pub mod decode;

pub use decode::{DecodeBody, DecodeLayer, DecodeService};

use std::error;
use std::fmt::{self, Display, Formatter};
use std::io;

#[derive(Debug)]
pub enum Error<E> {
    Body(E),
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
