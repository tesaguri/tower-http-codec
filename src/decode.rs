#![cfg_attr(
    not(any(feature = "br", feature = "gzip", feature = "deflate")),
    allow(unreachable_code, unused)
)]

use std::error::Error;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

#[cfg(feature = "br")]
use async_compression::stream::BrotliDecoder;
#[cfg(feature = "gzip")]
use async_compression::stream::GzipDecoder;
#[cfg(feature = "deflate")]
use async_compression::stream::ZlibDecoder;
use bytes::{Buf, Bytes};
use futures_core::stream::Stream;
use http::header::{self, HeaderValue, ACCEPT_ENCODING, CONTENT_ENCODING};
use pin_project::pin_project;

use crate::util::BodyAsStream;

#[derive(Debug, Clone)]
pub struct DecodeService<S> {
    inner: S,
}

#[derive(Debug, Default, Clone)]
pub struct DecodeLayer {
    _priv: (),
}

#[pin_project]
#[derive(Debug)]
pub struct ResponseFuture<F> {
    #[pin]
    inner: F,
}

#[pin_project]
#[derive(Debug)]
pub struct DecodeBody<B> {
    #[pin]
    inner: BodyInner<B>,
}

#[pin_project(project = BodyInnerProj)]
#[derive(Debug)]
enum BodyInner<B> {
    Identity(#[pin] B),
    #[cfg(feature = "gzip")]
    Gzip(#[pin] GzipDecoder<BodyAsStream<B>>),
    #[cfg(feature = "deflate")]
    Deflate(#[pin] ZlibDecoder<BodyAsStream<B>>),
    #[cfg(feature = "br")]
    Brotli(#[pin] BrotliDecoder<BodyAsStream<B>>),
}

impl<S> DecodeService<S> {
    pub fn new(service: S) -> Self {
        DecodeService { inner: service }
    }
}

impl<S, T, B> tower_service::Service<http::Request<T>> for DecodeService<S>
where
    S: tower_service::Service<http::Request<T>, Response = http::Response<B>>,
    S::Error: Into<Box<dyn Error + Send + Sync>>,
    B: http_body::Body,
    B::Error: Into<Box<dyn Error + Send + Sync>>,
{
    type Response = http::Response<DecodeBody<B>>;
    type Error = Box<dyn Error + Send + Sync>;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, mut req: http::Request<T>) -> Self::Future {
        if cfg!(any(feature = "br", feature = "deflate", feature = "gzip")) {
            let accept = match (
                cfg!(feature = "gzip"),
                cfg!(feature = "deflate"),
                cfg!(feature = "br"),
            ) {
                (true, true, true) => "gzip,deflate,br",
                (true, true, false) => "gzip,deflate",
                (true, false, true) => "gzip,br",
                (true, false, false) => "gzip",
                (false, true, true) => "deflate,br",
                (false, true, false) => "deflate",
                (false, false, true) => "br",
                (false, false, false) => unreachable!(),
            };
            req.headers_mut()
                .insert(ACCEPT_ENCODING, HeaderValue::from_static(accept));
        }
        ResponseFuture {
            inner: self.inner.call(req),
        }
    }
}

impl DecodeLayer {
    pub fn new() -> Self {
        Default::default()
    }
}

impl<S> tower_layer::Layer<S> for DecodeLayer {
    type Service = DecodeService<S>;

    fn layer(&self, service: S) -> Self::Service {
        DecodeService::new(service)
    }
}

impl<F, B, E> Future for ResponseFuture<F>
where
    F: Future<Output = Result<http::Response<B>, E>>,
    B: http_body::Body,
    B::Error: Into<Box<dyn Error + Send + Sync>>,
    E: Into<Box<dyn Error + Send + Sync>>,
{
    type Output = Result<http::Response<DecodeBody<B>>, Box<dyn Error + Send + Sync>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().project();
        this.inner
            .poll(cx)
            .map_err(Into::into)
            .map(|result| result.map(DecodeBody::wrap_response))
    }
}

impl<B> DecodeBody<B>
where
    B: http_body::Body,
    B::Error: Into<Box<dyn Error + Send + Sync>>,
{
    fn wrap_response(res: http::Response<B>) -> http::Response<Self> {
        let (mut parts, body) = res.into_parts();
        let inner = if let header::Entry::Occupied(e) = parts.headers.entry(CONTENT_ENCODING) {
            match e.get().as_bytes() {
                #[cfg(feature = "gzip")]
                b"gzip" => {
                    e.remove();
                    BodyInner::Gzip(GzipDecoder::new(BodyAsStream(body)))
                }
                #[cfg(feature = "deflate")]
                b"deflate" => {
                    e.remove();
                    BodyInner::Deflate(ZlibDecoder::new(BodyAsStream(body)))
                }
                #[cfg(feature = "br")]
                b"br" => {
                    e.remove();
                    BodyInner::Brotli(BrotliDecoder::new(BodyAsStream(body)))
                }
                _ => BodyInner::Identity(body),
            }
        } else {
            BodyInner::Identity(body)
        };
        http::Response::from_parts(parts, DecodeBody { inner })
    }
}

impl<B> http_body::Body for DecodeBody<B>
where
    B: http_body::Body,
    B::Error: Into<Box<dyn Error + Send + Sync>>,
    B::Error: Into<Box<dyn Error + Send + Sync>>,
{
    type Data = Bytes;
    type Error = Box<dyn Error + Send + Sync>;

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        let poll: Poll<Option<Result<_, _>>> = match self.project().inner.project() {
            BodyInnerProj::Identity(b) => {
                return b.poll_data(cx).map(|opt| {
                    opt.map(|result| result.map(|mut data| data.to_bytes()).map_err(Into::into))
                })
            }
            #[cfg(feature = "gzip")]
            BodyInnerProj::Gzip(s) => s.poll_next(cx),
            #[cfg(feature = "deflate")]
            BodyInnerProj::Deflate(s) => s.poll_next(cx),
            #[cfg(feature = "br")]
            BodyInnerProj::Brotli(s) => s.poll_next(cx),
        };
        poll.map(|opt| opt.map(|result| result.map_err(io::Error::into)))
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        match self.project().inner.project() {
            BodyInnerProj::Identity(b) => b.poll_trailers(cx),
            #[cfg(feature = "gzip")]
            BodyInnerProj::Gzip(s) => s.get_pin_mut().project().0.poll_trailers(cx),
            #[cfg(feature = "deflate")]
            BodyInnerProj::Deflate(s) => s.get_pin_mut().project().0.poll_trailers(cx),
            #[cfg(feature = "br")]
            BodyInnerProj::Brotli(s) => s.get_pin_mut().project().0.poll_trailers(cx),
        }
        .map_err(Into::into)
    }
}
