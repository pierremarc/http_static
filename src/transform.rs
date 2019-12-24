use crate::markdown::MarkdownStream;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures::Async::*;
use futures::{future::Either, Future, Poll};
use http;
use http::header::{HeaderValue, CONTENT_TYPE};
use std::io;
use std::path::PathBuf;
use tower_service::Service;
use tower_web::error;
use tower_web::middleware::Middleware;
use tower_web::util::buf_stream::{BufStream, SizeHint};

/// Markdowns the inner service's response bodies.
#[derive(Debug)]
pub struct MarkdownService<S> {
    inner: S,
    root_path: PathBuf,
}

/// Markdown the response body.
#[derive(Debug)]
pub struct ResponseFuture<T> {
    inner: T,
    process: bool,
}

#[derive(Debug)]
pub struct ServiceError {}

impl ::std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        write!(f, "(service error)")
    }
}

impl ::std::error::Error for ServiceError {}

impl<S> MarkdownService<S> {
    pub(super) fn new(inner: S, root_path: PathBuf) -> MarkdownService<S> {
        MarkdownService { inner, root_path }
    }
}

fn update_request<RequestBody>(
    req: http::Request<RequestBody>,
    path: &str,
) -> http::Request<RequestBody> {
    let new_path = path.parse::<http::uri::PathAndQuery>();
    match new_path {
        Err(_) => req,
        Ok(paq) => {
            let (mut req_parts, body) = req.into_parts();
            let mut uri_parts = req_parts.uri.clone().into_parts();
            uri_parts.path_and_query = Some(paq);
            req_parts.uri = http::Uri::from_parts(uri_parts).unwrap_or(req_parts.uri);
            http::Request::from_parts(req_parts, body)
        }
    }
}

impl<InnerService, RequestBody, ResponseBody> Service for MarkdownService<InnerService>
where
    ResponseBody: BufStream,
    InnerService:
        Service<Request = http::Request<RequestBody>, Response = http::Response<ResponseBody>>,
    InnerService::Future: Future<Item = http::Response<ResponseBody>>,
    InnerService::Error: ::std::error::Error,
{
    type Request = http::Request<RequestBody>;
    type Response = http::Response<EitherStream<ResponseBody>>;
    type Error = ServiceError;
    type Future = ResponseFuture<InnerService::Future>;

    fn poll_ready(&mut self) -> Poll<(), Self::Error> {
        self.inner.poll_ready().map_err(|_| ServiceError {})
    }

    fn call(&mut self, request: Self::Request) -> Self::Future {
        let req_path_str = dbg!(String::from(request.uri().path()));
        let req_path = dbg!(PathBuf::from(req_path_str.get(1..).unwrap_or("index.md")));
        let maybe_full_path = dbg!(self.root_path.clone().join(req_path.clone()));
        let full_path = if maybe_full_path.is_dir() {
            maybe_full_path.clone().join("index.md")
        } else {
            maybe_full_path.clone()
        };

        match dbg!(full_path.extension()) {
            Some(ext) => ResponseFuture {
                inner: self.inner.call(request),
                process: ext == "md",
            },
            None => {
                let full_path_ext = dbg!(full_path.with_extension("md"));
                if dbg!(full_path_ext.exists()) {
                    let req_path_ext = req_path.clone().with_extension("md");
                    let new_path = dbg!(format!("/{}", req_path_ext.to_string_lossy()));
                    let new_request = (update_request(request, &new_path.clone()));
                    ResponseFuture {
                        inner: self.inner.call(new_request),
                        process: true,
                    }
                } else {
                    ResponseFuture {
                        inner: self.inner.call(request),
                        process: false,
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct EitherError {}

impl ::std::fmt::Display for EitherError {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        write!(f, "(either error)")
    }
}

impl ::std::error::Error for EitherError {}

pub struct EitherStream<B>(Either<MarkdownStream<B>, B>);

impl<B> EitherStream<B>
where
    B: BufStream,
{
    fn Left(l: MarkdownStream<B>) -> EitherStream<B> {
        EitherStream(Either::A(l))
    }

    fn Right(r: B) -> EitherStream<B> {
        EitherStream(Either::B(r))
    }
}

impl<T> BufStream for EitherStream<T>
where
    T: BufStream,
    T::Error: ::std::error::Error,
    // T::Item: ::std::io::BufRead,
{
    type Item = io::Cursor<Bytes>;
    type Error = EitherError;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match &mut self.0 {
            Either::A(ms) => match BufStream::poll(ms) {
                Err(err) => Err(EitherError {}),
                Ok(NotReady) => Ok(NotReady),
                Ok(Ready(body)) => match body {
                    None => Ok(Ready(None)),
                    Some(body) => {
                        let bytes: Bytes = body.into_inner().into();
                        let cursor: io::Cursor<Bytes> = io::Cursor::new(bytes);
                        Ok(Ready(Some(cursor)))
                    }
                },
            },
            Either::B(s) => match BufStream::poll(s) {
                Err(err) => Err(EitherError {}),
                Ok(NotReady) => Ok(NotReady),
                Ok(Ready(body)) => match body {
                    None => Ok(Ready(None)),
                    Some(body) => {
                        let bytes: Bytes = body.bytes().into();
                        let cursor: io::Cursor<Bytes> = io::Cursor::new(bytes);
                        Ok(Ready(Some(cursor)))
                    }
                },
            },
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.0 {
            Either::A(ms) => BufStream::size_hint(ms),
            Either::B(s) => BufStream::size_hint(s),
        }
    }
}

impl<T, B> Future for ResponseFuture<T>
where
    B: BufStream,
    T: Future<Item = http::Response<B>>,
    T::Error: ::std::error::Error,
{
    type Item = http::response::Response<EitherStream<B>>;
    type Error = ServiceError;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        match self.inner.poll() {
            Err(_) => Err(ServiceError {}),
            Ok(NotReady) => Ok(NotReady),
            Ok(Ready(response)) => {
                if self.process {
                    let mut mapped =
                        response.map(|body| EitherStream::Left(MarkdownStream::new(body)));
                    mapped.headers_mut().insert(
                        CONTENT_TYPE,
                        HeaderValue::from_static("text/html; charset=UTF-8"),
                    );
                    Ok(Ready(mapped))
                } else {
                    Ok(Ready(response.map(|body| EitherStream::Right(body))))
                }
            }
        }
    }
}

/// Markdown all response bodies
#[derive(Debug)]
pub struct MarkdownMiddleware {
    root: String,
}

impl MarkdownMiddleware {
    /// Create a new `MarkdownMiddleware` instance
    pub fn new(root: &str) -> MarkdownMiddleware {
        MarkdownMiddleware {
            root: String::from(root),
        }
    }
}

impl<S, RequestBody, ResponseBody> Middleware<S> for MarkdownMiddleware
where
    S: Service<Request = http::Request<RequestBody>, Response = http::Response<ResponseBody>>,
    RequestBody: BufStream,
    ResponseBody: BufStream,
    S::Error: ::std::error::Error,
{
    type Request = http::Request<RequestBody>;
    type Response = http::Response<EitherStream<ResponseBody>>;
    type Error = ServiceError;
    type Service = MarkdownService<S>;

    fn wrap(&self, service: S) -> Self::Service {
        MarkdownService::new(service, PathBuf::from(&self.root))
    }
}
