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
use http::header::{self, HeaderValue, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, RANGE};
use pin_project::pin_project;

use crate::util::BodyAsStream;

#[derive(Debug, Clone)]
pub struct DecodeService<S> {
    inner: S,
    options: Options,
}

#[derive(Debug, Default, Clone)]
pub struct DecodeLayer {
    options: Options,
}

#[pin_project]
#[derive(Debug)]
pub struct ResponseFuture<F> {
    #[pin]
    inner: F,
    options: Options,
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

#[derive(Debug, Clone)]
struct Options {
    #[cfg(feature = "gzip")]
    gzip: bool,
    #[cfg(feature = "deflate")]
    deflate: bool,
    #[cfg(feature = "br")]
    br: bool,
}

impl<S> DecodeService<S> {
    pub fn new(service: S) -> Self {
        DecodeService {
            inner: service,
            options: Options::default(),
        }
    }

    #[cfg(feature = "gzip")]
    pub fn gzip(mut self, enable: bool) -> Self {
        self.options.gzip = enable;
        self
    }

    #[cfg(feature = "deflate")]
    pub fn deflate(mut self, enable: bool) -> Self {
        self.options.deflate = enable;
        self
    }

    #[cfg(feature = "br")]
    pub fn br(mut self, enable: bool) -> Self {
        self.options.br = enable;
        self
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
        if !req.headers().contains_key(RANGE) {
            if let header::Entry::Vacant(e) = req.headers_mut().entry(ACCEPT_ENCODING) {
                if let Some(accept) = self.options.accept_encoding() {
                    e.insert(accept);
                }
            }
        }
        ResponseFuture {
            inner: self.inner.call(req),
            options: self.options.clone(),
        }
    }
}

impl DecodeLayer {
    pub fn new() -> Self {
        Default::default()
    }

    #[cfg(feature = "gzip")]
    pub fn gzip(mut self, enable: bool) -> Self {
        self.options.gzip = enable;
        self
    }

    #[cfg(feature = "deflate")]
    pub fn deflate(mut self, enable: bool) -> Self {
        self.options.deflate = enable;
        self
    }

    #[cfg(feature = "br")]
    pub fn br(mut self, enable: bool) -> Self {
        self.options.br = enable;
        self
    }
}

impl<S> tower_layer::Layer<S> for DecodeLayer {
    type Service = DecodeService<S>;

    fn layer(&self, service: S) -> Self::Service {
        DecodeService {
            inner: service,
            options: self.options.clone(),
        }
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
            .map(|result| result.map(|res| DecodeBody::wrap_response(res, &self.options)))
    }
}

impl<B> DecodeBody<B>
where
    B: http_body::Body,
    B::Error: Into<Box<dyn Error + Send + Sync>>,
{
    fn wrap_response(res: http::Response<B>, options: &Options) -> http::Response<Self> {
        let (mut parts, body) = res.into_parts();
        let inner = if let header::Entry::Occupied(e) = parts.headers.entry(CONTENT_ENCODING) {
            match e.get().as_bytes() {
                #[cfg(feature = "gzip")]
                b"gzip" if options.gzip => BodyInner::Gzip(GzipDecoder::new(BodyAsStream(body))),
                #[cfg(feature = "deflate")]
                b"deflate" if options.deflate => {
                    BodyInner::Deflate(ZlibDecoder::new(BodyAsStream(body)))
                }
                #[cfg(feature = "br")]
                b"br" if options.br => BodyInner::Brotli(BrotliDecoder::new(BodyAsStream(body))),
                _ => return http::Response::from_parts(parts, DecodeBody::identity(body)),
            }
            e.remove();
            parts.headers.remove(CONTENT_LENGTH);
        } else {
            return http::Response::from_parts(parts, DecodeBody::identity(body));
        };
        http::Response::from_parts(parts, DecodeBody { inner })
    }

    fn identity(body: B) -> Self {
        DecodeBody {
            inner: BodyInner::Identity(body),
        }
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

impl Options {
    fn accept_encoding(&self) -> Option<HeaderValue> {
        let accept = match (self.gzip(), self.deflate(), self.br()) {
            (true, true, true) => "gzip,deflate,br",
            (true, true, false) => "gzip,deflate",
            (true, false, true) => "gzip,br",
            (true, false, false) => "gzip",
            (false, true, true) => "deflate,br",
            (false, true, false) => "deflate",
            (false, false, true) => "br",
            (false, false, false) => return None,
        };
        Some(HeaderValue::from_static(accept))
    }

    fn gzip(&self) -> bool {
        #[cfg(feature = "gzip")]
        {
            self.gzip
        }
        #[cfg(not(feature = "gzip"))]
        {
            false
        }
    }

    fn deflate(&self) -> bool {
        #[cfg(feature = "deflate")]
        {
            self.deflate
        }
        #[cfg(not(feature = "deflate"))]
        {
            false
        }
    }

    fn br(&self) -> bool {
        #[cfg(feature = "br")]
        {
            self.br
        }
        #[cfg(not(feature = "br"))]
        {
            false
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Options {
            #[cfg(feature = "gzip")]
            gzip: true,
            #[cfg(feature = "deflate")]
            deflate: true,
            #[cfg(feature = "br")]
            br: true,
        }
    }
}
