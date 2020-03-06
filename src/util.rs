use std::error;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes};
use futures_core::Stream;

#[pin_project::pin_project]
#[derive(Debug)]
pub struct BodyAsStream<B>(#[pin] pub B);

impl<B> Stream for BodyAsStream<B>
where
    B: http_body::Body,
    B::Error: Into<Box<dyn error::Error + Send + Sync>>,
{
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().0.poll_data(cx).map(|opt| {
            opt.map(|result| {
                result
                    .map(|mut data| data.to_bytes())
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))
            })
        })
    }
}
