use bytes::{Buf, BufMut, Bytes, BytesMut};
use futures::Async::*;
use futures::Poll;
use pulldown_cmark::{html, CowStr, Options, Parser, URLManip};
use std::io;
use std::io::Cursor;
use std::path::PathBuf;
use tower_web::util::buf_stream::{BufStream, SizeHint};

fn make_url_manip(root_path: PathBuf) -> Box<URLManip> {
    Box::new(move |fp| {
        println!("\n> url_manip {}", fp);
        let rpath = root_path.clone();
        let fpath = PathBuf::from(fp);
        match dbg!(rpath.join(fpath.clone()).extension()) {
            Some(_) => CowStr::Borrowed(fp),
            None => {
                let fpe = dbg!(rpath.join(fpath.clone()).with_extension("md"));
                if dbg!(fpe.exists()) {
                    match fpath.with_extension("md").to_str() {
                        None => CowStr::Borrowed(fp),
                        Some(s) => dbg!(CowStr::from(String::from(s))),
                    }
                } else {
                    CowStr::Borrowed(fp)
                }
            }
        }
    })
}

const html_head_str: &'static str = include_str!("html/head.html");
const html_tail_str: &'static str = include_str!("html/tail.html");

/// Compress a buf stream using zlib deflate.
#[derive(Debug)]
pub struct MarkdownStream<T> {
    // The inner BufStream
    inner: T,

    eof: bool,
    // `true` when the inner buffer returned `None`
    inner_eof: bool,

    // Buffers input
    src_buf: BytesMut,
    // root_path: PathBuf,
}

/// Errors returned by `MarkdownStream`.
#[derive(Debug)]
pub struct Error<T>
where
    T: ::std::error::Error,
{
    /// `None` represents a deflate error
    inner: Option<T>,
}

impl<T> ::std::fmt::Display for Error<T>
where
    T: ::std::error::Error,
{
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        write!(f, "(md error)")
    }
}

impl<T> ::std::error::Error for Error<T> where T: ::std::error::Error {}

impl<T> MarkdownStream<T>
where
    T: BufStream,
{
    pub fn new(inner: T) -> MarkdownStream<T> {
        MarkdownStream {
            inner,
            eof: false,
            inner_eof: false,
            src_buf: BytesMut::new(),
        }
    }
}

impl<T> BufStream for MarkdownStream<T>
where
    T: BufStream,
    T::Error: ::std::error::Error,
{
    type Item = io::Cursor<BytesMut>;
    type Error = Error<T::Error>;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        let mut i = 0;
        if self.eof {
            return Ok(Ready(None));
        }
        loop {
            println!("{} {}", i, self.inner_eof);
            i += 1;
            if !self.inner_eof {
                let res = self.inner.poll().map_err(|e| Error { inner: Some(e) });

                match try_ready!(res) {
                    Some(buf) => {
                        self.src_buf.reserve(buf.remaining());
                        self.src_buf.put(buf);
                    }
                    None => {
                        self.inner_eof = true;
                        break;
                    }
                }
            } else {
                break;
            }
        }
        self.eof = true;
        match ::std::str::from_utf8(&self.src_buf.to_vec()) {
            Ok(input) => {
                let mut head = BytesMut::from(html_head_str);
                let tail = BytesMut::from(html_tail_str);
                let mut bytes = Vec::new();
                let parser = Parser::new(input);
                let cursor = Cursor::new(&mut bytes);
                match html::write_html(cursor, parser) {
                    Ok(_) => {
                        head.extend_from_slice(bytes.as_slice());
                        head.extend_from_slice(&tail);
                        Ok(Ready(Some(Cursor::new(head))))
                    }
                    Err(_) => Err(Error { inner: None }),
                }
            }
            Err(err) => Err(Error { inner: None }),
        }
    }

    fn size_hint(&self) -> SizeHint {
        // TODO: How should this work?
        self.inner.size_hint()
    }
}

// fn to_html(input: &str) -> String {
//     let mut options = Options::empty();
//     options.insert(Options::ENABLE_STRIKETHROUGH);
//     let parser = Parser::new_ext(input, options);

//     // Write to String buffer.
//     let mut html_output = String::new();
//     html::push_html(&mut html_output, parser);
//     html_output
// }
