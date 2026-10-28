use base64::Engine;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, HeaderMap, Method, Request, StatusCode};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client as HyperClient},
    rt::TokioExecutor,
};
use std::{
    collections::BTreeMap,
    future::Future,
    io::{self, Read},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::runtime::Runtime;

/// Client sends explicit HTTP requests without implicit Accept headers or redirects.
pub struct Client {
    runtime: Arc<Runtime>,
    inner: HyperClient<HttpsConnector<HttpConnector>, Full<Bytes>>,
    header: Duration,
    idle: Duration,
    total: Option<Duration>,
    pub cancellation: Option<Arc<AtomicBool>>,
}
impl Client {
    pub fn new(header: Duration, idle: Duration, total: Option<Duration>) -> io::Result<Self> {
        let runtime = Arc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?,
        );
        let connector = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .build();
        let inner = HyperClient::builder(TokioExecutor::new())
            .retry_canceled_requests(false)
            .build(connector);
        Ok(Self {
            runtime,
            inner,
            header,
            idle,
            total,
            cancellation: None,
        })
    }
    pub fn post(&self, url: impl AsRef<str>) -> RequestBuilder<'_> {
        self.request(Method::POST, url.as_ref())
    }
    pub fn get(&self, url: impl AsRef<str>) -> RequestBuilder<'_> {
        self.request(Method::GET, url.as_ref())
    }
    fn request(&self, method: Method, url: &str) -> RequestBuilder<'_> {
        RequestBuilder {
            client: self,
            method,
            url: url.into(),
            headers: HeaderMap::new(),
            body: Vec::new(),
            invalid: false,
        }
    }
}

pub struct RequestBuilder<'a> {
    client: &'a Client,
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Vec<u8>,
    invalid: bool,
}
impl RequestBuilder<'_> {
    pub fn header(mut self, name: &str, value: impl AsRef<str>) -> Self {
        match (
            hyper::header::HeaderName::from_bytes(name.as_bytes()),
            hyper::header::HeaderValue::from_str(value.as_ref()),
        ) {
            (Ok(name), Ok(value)) => {
                self.headers.insert(name, value);
            }
            _ => self.invalid = true,
        }
        self
    }
    pub fn bearer_auth(self, token: impl AsRef<str>) -> Self {
        self.header("authorization", format!("Bearer {}", token.as_ref()))
    }
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }
    pub fn form(self, fields: &BTreeMap<&str, &str>) -> Self {
        let mut url = reqwest::Url::parse("http://localhost/").unwrap();
        url.query_pairs_mut().extend_pairs(fields.iter());
        self.header("content-type", "application/x-www-form-urlencoded")
            .body(url.query().unwrap_or("").as_bytes().to_vec())
    }
    pub fn send(mut self) -> io::Result<Response> {
        if self.invalid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid HTTP header",
            ));
        }
        let mut url = reqwest::Url::parse(&self.url)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid HTTP endpoint"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid HTTP endpoint",
            ));
        }
        if (!url.username().is_empty() || url.password().is_some())
            && !self.headers.contains_key("authorization")
        {
            let credentials = [
                unescape(url.username()),
                b":".to_vec(),
                unescape(url.password().unwrap_or("")),
            ]
            .concat();
            self = self.header(
                "authorization",
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode(credentials)
                ),
            );
        }
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_fragment(None);
        if !self.headers.contains_key("user-agent") {
            self = self.header("user-agent", "Go-http-client/1.1");
        }
        let gzip = !self.headers.contains_key("accept-encoding")
            && !self.headers.contains_key("range")
            && self.method != Method::HEAD;
        if gzip {
            self = self.header("accept-encoding", "gzip");
        }
        if self.method != Method::GET
            && self.method != Method::HEAD
            && !self.headers.contains_key("content-length")
        {
            let length = self.body.len();
            self = self.header("content-length", length.to_string());
        }
        let mut request = Request::builder()
            .method(self.method)
            .uri(url.as_str())
            .body(Full::new(Bytes::from(self.body)))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid HTTP request"))?;
        *request.headers_mut() = self.headers;
        let total = self.client.total.map(|duration| Instant::now() + duration);
        let header_end = total.map_or(Instant::now() + self.client.header, |end| {
            end.min(Instant::now() + self.client.header)
        });
        let response = self
            .client
            .runtime
            .block_on(wait(
                self.client.inner.request(request),
                header_end,
                self.client.cancellation.as_ref(),
            ))?
            .map_err(|error| {
                io::Error::new(
                    if error.is_connect() {
                        io::ErrorKind::ConnectionRefused
                    } else {
                        io::ErrorKind::Other
                    },
                    error,
                )
            })?;
        let status = response.status();
        let mut headers = response.headers().clone();
        let decompress = gzip && headers.get("content-encoding").is_some_and(|v| v == "gzip");
        if decompress {
            headers.remove("content-encoding");
            headers.remove("content-length");
        }
        let idle_end = Arc::new(Mutex::new(Instant::now() + self.client.idle));
        let reader = NetworkReader {
            runtime: self.client.runtime.clone(),
            incoming: response.into_body(),
            buffer: Bytes::new(),
            idle_end: idle_end.clone(),
            total,
            cancellation: self.client.cancellation.clone(),
        };
        Ok(Response {
            status,
            headers,
            body: Body {
                plain: Some(reader),
                gzip: None,
                decompress,
                idle: self.client.idle,
                idle_end,
            },
        })
    }
}
fn unescape(value: &str) -> Vec<u8> {
    let mut output = Vec::new();
    let mut bytes = value.as_bytes().iter().copied();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            if let (Some(a), Some(b)) = (bytes.next(), bytes.next()) {
                if let (Some(a), Some(b)) = ((a as char).to_digit(16), (b as char).to_digit(16)) {
                    output.push((a * 16 + b) as u8);
                    continue;
                }
            }
        }
        output.push(byte);
    }
    output
}
async fn wait<F: Future>(
    future: F,
    end: Instant,
    cancellation: Option<&Arc<AtomicBool>>,
) -> io::Result<F::Output> {
    let mut future = std::pin::pin!(future);
    loop {
        if cancellation.is_some_and(|token| token.load(Ordering::SeqCst)) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "context canceled",
            ));
        }
        let remaining = end
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "operation timed out"))?;
        match tokio::time::timeout(remaining.min(Duration::from_millis(20)), future.as_mut()).await
        {
            Ok(result) => return Ok(result),
            Err(_) => {}
        }
    }
}
/// Response retains headers even if a later body read fails.
pub struct Response {
    status: StatusCode,
    headers: HeaderMap,
    body: Body,
}
impl Response {
    pub fn status(&self) -> StatusCode {
        self.status
    }
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }
}
impl Read for Response {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.body.read(bytes)
    }
}
struct Body {
    plain: Option<NetworkReader>,
    gzip: Option<flate2::read::GzDecoder<NetworkReader>>,
    decompress: bool,
    idle: Duration,
    idle_end: Arc<Mutex<Instant>>,
}
impl Read for Body {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let read = if self.decompress {
            if self.gzip.is_none() {
                self.gzip = Some(flate2::read::GzDecoder::new(self.plain.take().unwrap()));
            }
            self.gzip.as_mut().unwrap().read(output)
        } else {
            self.plain.as_mut().unwrap().read(output)
        };
        if matches!(read,Ok(n) if n>0) {
            *self.idle_end.lock().unwrap() = Instant::now() + self.idle;
        }
        read
    }
}
struct NetworkReader {
    runtime: Arc<Runtime>,
    incoming: Incoming,
    buffer: Bytes,
    idle_end: Arc<Mutex<Instant>>,
    total: Option<Instant>,
    cancellation: Option<Arc<AtomicBool>>,
}
impl Read for NetworkReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            if !self.buffer.is_empty() {
                let count = output.len().min(self.buffer.len());
                output[..count].copy_from_slice(&self.buffer.split_to(count));
                return Ok(count);
            }
            let end = *self.idle_end.lock().unwrap();
            let end = self.total.map_or(end, |total| total.min(end));
            let frame = self.runtime.block_on(wait(
                self.incoming.frame(),
                end,
                self.cancellation.as_ref(),
            ))?;
            match frame {
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        self.buffer = data;
                    }
                }
                Some(Err(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "unexpected end of file",
                    ))
                }
                None => return Ok(0),
            }
        }
    }
}

#[cfg(test)]
#[path = "http_tests.rs"]
pub(crate) mod tests;
