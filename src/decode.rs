//! Middleware that decompresses response bodies.

#![cfg_attr(
    not(any(feature = "br", feature = "gzip", feature = "deflate")),
    allow(unreachable_code, unused)
)]

use std::fmt::{self, Debug, Formatter};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

#[cfg(feature = "br")]
use async_compression::tokio::bufread::BrotliDecoder;
#[cfg(feature = "gzip")]
use async_compression::tokio::bufread::GzipDecoder;
#[cfg(feature = "deflate")]
use async_compression::tokio::bufread::ZlibDecoder;
use bitflags::bitflags;
use bytes::{Buf, Bytes, BytesMut};
use futures_core::{ready, Stream, TryFuture};
use http::header::{self, HeaderValue, ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, RANGE};
use pin_project_lite::pin_project;
use tokio_util::codec::{BytesCodec, FramedRead};
use tokio_util::io::StreamReader;

use crate::Error;

/// Decompresses response bodies of the underlying service.
///
/// This adds the `Accept-Encoding` header to requests and transparently decompresses response
/// bodies based on the `Content-Encoding` header.
#[derive(Debug, Clone)]
pub struct Decode<S> {
    inner: S,
    options: Options,
}

/// Decompresses response bodies of the underlying service.
///
/// This adds the `Accept-Encoding` header to requests and transparently decompresses response
/// bodies based on the `Content-Encoding` header.
#[derive(Debug, Default, Clone)]
pub struct DecodeLayer {
    options: Options,
}

pin_project! {
    /// Response future of [`Decode`].
    #[derive(Debug)]
    pub struct ResponseFuture<F> {
        #[pin]
        inner: F,
        options: Options,
    }
}

pin_project! {
    /// Response body of [`Decode`].
    #[derive(Debug)]
    pub struct DecodeBody<B: http_body::Body> {
        #[pin]
        inner: BodyInner<B>,
    }
}

/// A wrapper around `pin_project!` to handle `cfg` attributes on enum variants.
macro_rules! pin_project_cfg {
    (
        $(#[$($attr:tt)*])*
        enum $name:ident $(<$($typaram:ident $(: $bound:path)?),*$(,)?>)? {
            $($body:tt)*
        }
    ) => {
        pin_project_cfg! {
            @accum
            #[cfg(all())]
            [$(#[$($attr)*])* enum $name <$($($typaram $(: $bound)?),*)?>]
            {}
            $($body)*
        }
    };
    (
        @accum
        #[cfg(all($($pred_accum:tt)*))]
        $outer:tt
        {$($accum:tt)*}

        #[cfg($($pred:tt)*)]
        $(#[$($variant_attr:tt)*])* $variant:ident $variant_body:tt,
        $($rest:tt)*
    ) => {
        pin_project_cfg! {
            @accum
            #[cfg(all($($pred_accum)* $($pred)*,))]
            $outer
            { $($accum)* $(#[$($variant_attr)*])* $variant $variant_body, }
            $($rest)*
        }
        pin_project_cfg! {
            @accum
            #[cfg(all($($pred_accum)* not($($pred)*),))]
            $outer
            {$($accum)*}
            $($rest)*
        }
    };
    (
        @accum
        #[cfg(all($($pred_accum:tt)*))]
        $outer:tt
        {$($accum:tt)*}

        $(#[$($variant_attr:tt)*])* $variant:ident $variant_body:tt,
        $($rest:tt)*
    ) => {
        pin_project_cfg! {
            @accum
            #[cfg(all($($pred_accum)*))]
            $outer
            {
                $($accum)*
                $(#[$($variant_attr)*])* $variant $variant_body,
            }
            $($rest)*
        }
    };
    (
        @accum
        #[$cfg:meta]
        [$($outer:tt)*]
        $body:tt
    ) => {
        #[$cfg]
        pin_project! {
            $($outer)* $body
        }
    };
}

type BodyReader<B> = StreamReader<Adapter<B>, Bytes>;

pin_project_cfg! {
    #[project = BodyInnerProj]
    #[derive(Debug)]
    enum BodyInner<B: http_body::Body> {
        Identity {
            #[pin]
            inner: B,
        },
        #[cfg(feature = "gzip")]
        Gzip {
            #[pin]
            inner: FramedRead<GzipDecoder<BodyReader<B>>, BytesCodec>,
        },
        #[cfg(feature = "deflate")]
        Deflate {
            #[pin]
            inner: FramedRead<ZlibDecoder<BodyReader<B>>, BytesCodec>,
        },
        #[cfg(feature = "br")]
        Brotli {
            #[pin]
            inner: FramedRead<BrotliDecoder<BodyReader<B>>, BytesCodec>,
        },
    }
}

pin_project! {
    /// A `TryStream<Error>` that captures the errors from the `Body` for later inspection.
    ///
    /// This is needed since the `io::Read` wrappers do not provide direct access to
    /// the inner `Body::Error` values.
    struct Adapter<B: http_body::Body> {
        #[pin]
        body: B,
        error: Option<B::Error>,
    }
}

bitflags! {
    struct Options: u8 {
        #[cfg(feature = "gzip")]
        const GZIP = 0b001;
        #[cfg(feature = "deflate")]
        const DEFLATE = 0b010;
        #[cfg(feature = "br")]
        const BR = 0b100;
    }
}

impl<S> Decode<S> {
    /// Creates a new `Decode` wrapping the `service`.
    pub fn new(service: S) -> Self {
        Decode {
            inner: service,
            options: Options::default(),
        }
    }

    /// Gets a reference to the underlying service.
    pub fn get_ref(&self) -> &S {
        &self.inner
    }

    /// Gets a mutable reference to the underlying service.
    pub fn get_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    /// Consumes `self`, returning the underlying service.
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// Sets whether to request the gzip encoding.
    #[cfg(feature = "gzip")]
    #[cfg_attr(docs, doc(cfg(feature = "gzip")))]
    pub fn gzip(self, enable: bool) -> Self {
        self.options.set_gzip(enable);
        self
    }

    /// Sets whether to request the Deflate encoding.
    #[cfg(feature = "deflate")]
    #[cfg_attr(docs, doc(cfg(feature = "deflate")))]
    pub fn deflate(self, enable: bool) -> Self {
        self.options.set_deflate(enable);
        self
    }

    /// Sets whether to request the Brotli encoding.
    #[cfg(feature = "br")]
    #[cfg_attr(docs, doc(cfg(feature = "br")))]
    pub fn br(self, enable: bool) -> Self {
        self.options.set_br(enable);
        self
    }

    /// Disables the gzip encoding.
    ///
    /// This method is available even if the `gzip` crate feature is disabled.
    pub fn no_gzip(self) -> Self {
        self.options.set_gzip(false);
        self
    }

    /// Disables the Deflate encoding.
    ///
    /// This method is available even if the `deflate` crate feature is disabled.
    pub fn no_deflate(self) -> Self {
        self.options.set_deflate(false);
        self
    }

    /// Disables the Brotli encoding.
    ///
    /// This method is available even if the `br` crate feature is disabled.
    pub fn no_br(self) -> Self {
        self.options.set_br(false);
        self
    }
}

impl<S, T, B> tower_service::Service<http::Request<T>> for Decode<S>
where
    S: tower_service::Service<http::Request<T>, Response = http::Response<B>>,
    B: http_body::Body,
{
    type Response = http::Response<DecodeBody<B>>;
    type Error = S::Error;
    type Future = ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
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
            options: self.options,
        }
    }
}

impl DecodeLayer {
    /// Creates a new `DecodeLayer`.
    pub fn new() -> Self {
        Default::default()
    }

    /// Sets whether to request the gzip encoding.
    #[cfg(feature = "gzip")]
    #[cfg_attr(docs, doc(cfg(feature = "gzip")))]
    pub fn gzip(self, enable: bool) -> Self {
        self.options.set_gzip(enable);
        self
    }

    /// Sets whether to request the Deflate encoding.
    #[cfg(feature = "deflate")]
    #[cfg_attr(docs, doc(cfg(feature = "deflate")))]
    pub fn deflate(self, enable: bool) -> Self {
        self.options.set_deflate(enable);
        self
    }

    /// Sets whether to request the Brotli encoding.
    #[cfg(feature = "br")]
    #[cfg_attr(docs, doc(cfg(feature = "br")))]
    pub fn br(self, enable: bool) -> Self {
        self.options.set_br(enable);
        self
    }

    /// Disables the gzip encoding.
    ///
    /// This method is available even if the `gzip` crate feature is disabled.
    pub fn no_gzip(self) -> Self {
        self.options.set_gzip(false);
        self
    }

    /// Disables the Deflate encoding.
    ///
    /// This method is available even if the `deflate` crate feature is disabled.
    pub fn no_deflate(self) -> Self {
        self.options.set_deflate(false);
        self
    }

    /// Disables the Brotli encoding.
    ///
    /// This method is available even if the `br` crate feature is disabled.
    pub fn no_br(self) -> Self {
        self.options.set_br(false);
        self
    }
}

impl<S> tower_layer::Layer<S> for DecodeLayer {
    type Service = Decode<S>;

    fn layer(&self, service: S) -> Self::Service {
        Decode {
            inner: service,
            options: self.options,
        }
    }
}

impl<F, B> Future for ResponseFuture<F>
where
    F: TryFuture<Ok = http::Response<B>>,
    B: http_body::Body,
{
    type Output = Result<http::Response<DecodeBody<B>>, F::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.as_mut().project();
        this.inner
            .try_poll(cx)
            .map(|result| result.map(|res| DecodeBody::wrap_response(res, &self.options)))
    }
}

impl<B> DecodeBody<B>
where
    B: http_body::Body,
{
    /// Gets a reference to the underlying body.
    pub fn get_ref(&self) -> &B {
        match self.inner {
            BodyInner::Identity { ref inner } => inner,
            #[cfg(feature = "gzip")]
            BodyInner::Gzip { ref inner } => &inner.get_ref().get_ref().get_ref().body,
            #[cfg(feature = "deflate")]
            BodyInner::Deflate { ref inner } => &inner.get_ref().get_ref().get_ref().body,
            #[cfg(feature = "br")]
            BodyInner::Brotli { ref inner } => &inner.get_ref().get_ref().get_ref().body,
        }
    }

    /// Gets a mutable reference to the underlying body.
    ///
    /// It is inadvisable to directly read from the underlying body.
    pub fn get_mut(&mut self) -> &mut B {
        match self.inner {
            BodyInner::Identity { ref mut inner } => inner,
            #[cfg(feature = "gzip")]
            BodyInner::Gzip { ref mut inner } => &mut inner.get_mut().get_mut().get_mut().body,
            #[cfg(feature = "deflate")]
            BodyInner::Deflate { ref mut inner } => &mut inner.get_mut().get_mut().get_mut().body,
            #[cfg(feature = "br")]
            BodyInner::Brotli { ref mut inner } => &mut inner.get_mut().get_mut().get_mut().body,
        }
    }

    /// Gets a pinned mutable reference to the underlying body.
    ///
    /// It is inadvisable to directly read from the underlying body.
    pub fn get_pin_mut(self: Pin<&mut Self>) -> Pin<&mut B> {
        match self.project().inner.project() {
            BodyInnerProj::Identity { inner } => inner,
            #[cfg(feature = "gzip")]
            BodyInnerProj::Gzip { inner } => {
                inner
                    .get_pin_mut()
                    .get_pin_mut()
                    .get_pin_mut()
                    .project()
                    .body
            }
            #[cfg(feature = "deflate")]
            BodyInnerProj::Deflate { inner } => {
                inner
                    .get_pin_mut()
                    .get_pin_mut()
                    .get_pin_mut()
                    .project()
                    .body
            }
            #[cfg(feature = "br")]
            BodyInnerProj::Brotli { inner } => {
                inner
                    .get_pin_mut()
                    .get_pin_mut()
                    .get_pin_mut()
                    .project()
                    .body
            }
        }
    }

    /// Comsumes `self`, returning the underlying body.
    ///
    /// Note that any leftover data in the internal buffer is lost.
    pub fn into_inner(self) -> B {
        match self.inner {
            BodyInner::Identity { inner } => inner,
            #[cfg(feature = "gzip")]
            BodyInner::Gzip { inner } => inner.into_inner().into_inner().into_inner().body,
            #[cfg(feature = "deflate")]
            BodyInner::Deflate { inner } => inner.into_inner().into_inner().into_inner().body,
            #[cfg(feature = "br")]
            BodyInner::Brotli { inner } => inner.into_inner().into_inner().into_inner().body,
        }
    }

    fn wrap_response(res: http::Response<B>, options: &Options) -> http::Response<Self> {
        let (mut parts, body) = res.into_parts();
        let inner = if let header::Entry::Occupied(e) = parts.headers.entry(CONTENT_ENCODING) {
            let inner = match e.get().as_bytes() {
                #[cfg(feature = "gzip")]
                b"gzip" if options.gzip() => {
                    let read = StreamReader::new(Adapter { body, error: None });
                    let inner = FramedRead::new(GzipDecoder::new(read), BytesCodec::new());
                    BodyInner::Gzip { inner }
                }
                #[cfg(feature = "deflate")]
                b"deflate" if options.deflate() => {
                    let read = StreamReader::new(Adapter { body, error: None });
                    let inner = FramedRead::new(ZlibDecoder::new(read), BytesCodec::new());
                    BodyInner::Deflate { inner }
                }
                #[cfg(feature = "br")]
                b"br" if options.br() => {
                    let read = StreamReader::new(Adapter { body, error: None });
                    let inner = FramedRead::new(BrotliDecoder::new(read), BytesCodec::new());
                    BodyInner::Brotli { inner }
                }
                _ => return http::Response::from_parts(parts, DecodeBody::identity(body)),
            };
            e.remove();
            parts.headers.remove(CONTENT_LENGTH);
            inner
        } else {
            BodyInner::Identity { inner: body }
        };
        http::Response::from_parts(parts, DecodeBody { inner })
    }

    fn identity(body: B) -> Self {
        DecodeBody {
            inner: BodyInner::Identity { inner: body },
        }
    }
}

impl<B> http_body::Body for DecodeBody<B>
where
    B: http_body::Body,
{
    type Data = Bytes;
    type Error = Error<B::Error>;

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        // The type annotation is required when compiling with no features.
        let (poll, inner): (Poll<_>, Pin<&mut Adapter<B>>) = match self.project().inner.project() {
            BodyInnerProj::Identity { inner } => {
                return inner.poll_data(cx).map(|opt| {
                    opt.map(|result| {
                        result
                            .map(|mut data| data.copy_to_bytes(data.remaining()))
                            .map_err(Error::Body)
                    })
                })
            }
            #[cfg(feature = "gzip")]
            BodyInnerProj::Gzip { mut inner } => (
                inner.as_mut().poll_next(cx),
                inner.get_pin_mut().get_pin_mut().get_pin_mut(),
            ),
            #[cfg(feature = "deflate")]
            BodyInnerProj::Deflate { mut inner } => (
                inner.as_mut().poll_next(cx),
                inner.get_pin_mut().get_pin_mut().get_pin_mut(),
            ),
            #[cfg(feature = "br")]
            BodyInnerProj::Brotli { mut inner } => (
                inner.as_mut().poll_next(cx),
                inner.get_pin_mut().get_pin_mut().get_pin_mut(),
            ),
        };
        poll.map(|opt: Option<_>| {
            opt.map(|result: Result<_, _>| {
                result.map(BytesMut::freeze).map_err(|e| {
                    inner
                        .project()
                        .error
                        .take()
                        .map(Error::Body)
                        .unwrap_or(Error::Compression(e))
                })
            })
        })
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        match self.project().inner.project() {
            BodyInnerProj::Identity { inner } => inner.poll_trailers(cx),
            #[cfg(feature = "gzip")]
            BodyInnerProj::Gzip { inner } => inner
                .get_pin_mut()
                .get_pin_mut()
                .get_pin_mut()
                .project()
                .body
                .poll_trailers(cx),
            #[cfg(feature = "deflate")]
            BodyInnerProj::Deflate { inner } => inner
                .get_pin_mut()
                .get_pin_mut()
                .get_pin_mut()
                .project()
                .body
                .poll_trailers(cx),
            #[cfg(feature = "br")]
            BodyInnerProj::Brotli { inner } => inner
                .get_pin_mut()
                .get_pin_mut()
                .get_pin_mut()
                .project()
                .body
                .poll_trailers(cx),
        }
        .map_err(Error::Body)
    }
}

impl<B: http_body::Body + Debug> Debug for Adapter<B> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        #[derive(Debug)]
        struct Adapter<B> {
            body: B,
            error: Option<Placeholder>,
        }
        struct Placeholder;
        impl Debug for Placeholder {
            fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
                f.write_str("_")
            }
        }

        Adapter {
            body: &self.body,
            error: self.error.as_ref().and(Some(Placeholder)),
        }
        .fmt(f)
    }
}

impl<B: http_body::Body> Stream for Adapter<B> {
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        match ready!(this.body.poll_data(cx)) {
            Some(Ok(mut data)) => Poll::Ready(Some(Ok(data.copy_to_bytes(data.remaining())))),
            Some(Err(e)) => {
                *this.error = Some(e);
                // Return a placeholder, which should be discarded by the outer `DecodeBody`.
                Poll::Ready(Some(Err(io::Error::from_raw_os_error(0))))
            }
            None => Poll::Ready(None),
        }
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
            self.contains(Options::GZIP)
        }
        #[cfg(not(feature = "gzip"))]
        {
            false
        }
    }

    fn deflate(&self) -> bool {
        #[cfg(feature = "deflate")]
        {
            self.contains(Options::DEFLATE)
        }
        #[cfg(not(feature = "deflate"))]
        {
            false
        }
    }

    fn br(&self) -> bool {
        #[cfg(feature = "br")]
        {
            self.contains(Options::BR)
        }
        #[cfg(not(feature = "br"))]
        {
            false
        }
    }

    pub fn set_gzip(mut self, enable: bool) -> Self {
        #[cfg(feature = "gzip")]
        {
            self.set(Options::GZIP, enable);
            self
        }
        #[cfg(not(feature = "gzip"))]
        {
            self
        }
    }

    pub fn set_deflate(mut self, enable: bool) -> Self {
        #[cfg(feature = "deflate")]
        {
            self.set(Options::DEFLATE, enable);
            self
        }
        #[cfg(not(feature = "deflate"))]
        {
            self
        }
    }

    pub fn set_br(mut self, enable: bool) -> Self {
        #[cfg(feature = "br")]
        {
            self.set(Options::BR, enable);
            self
        }
        #[cfg(not(feature = "br"))]
        {
            self
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Options::all()
    }
}
