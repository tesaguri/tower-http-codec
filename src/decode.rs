use std::error::Error;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use async_compression::stream::GzipDecoder;
use bytes::{Buf, Bytes};
use futures_core::stream::Stream;
use http::header::{HeaderValue, ACCEPT_ENCODING, CONTENT_ENCODING};
use pin_project::{pin_project, project};

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

#[pin_project]
#[derive(Debug)]
enum BodyInner<B> {
    Identity(#[pin] B),
    Gzip(#[pin] GzipDecoder<BodyAsStream<B>>),
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
        req.headers_mut()
            .insert(ACCEPT_ENCODING, HeaderValue::from_static("gzip"));
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
        this.inner.poll(cx).map_err(Into::into).map(|result| {
            result.and_then(|res| {
                let (parts, body) = res.into_parts();
                let inner = match parts.headers.get(CONTENT_ENCODING).map(|v| v.as_bytes()) {
                    None => BodyInner::Identity(body),
                    Some(b"gzip") => BodyInner::Gzip(GzipDecoder::new(BodyAsStream(body))),
                    Some(_) => return Err("unsupported encoding".into()),
                };
                let body = DecodeBody { inner };
                Ok(http::Response::from_parts(parts, body))
            })
        })
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

    #[project]
    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        #[project]
        match self.project().inner.project() {
            BodyInner::Identity(b) => b.poll_data(cx).map(|opt| {
                opt.map(|result| result.map(|mut data| data.to_bytes()).map_err(Into::into))
            }),
            BodyInner::Gzip(s) => s
                .poll_next(cx)
                .map(|opt| opt.map(|result| result.map_err(Into::into))),
        }
    }

    #[project]
    fn poll_trailers(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
        #[project]
        match self.project().inner.project() {
            BodyInner::Identity(b) => b.poll_trailers(cx).map_err(Into::into),
            BodyInner::Gzip(s) => s
                .get_pin_mut()
                .project()
                .0
                .poll_trailers(cx)
                .map_err(Into::into),
        }
    }
}
