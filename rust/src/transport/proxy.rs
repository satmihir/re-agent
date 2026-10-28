use super::proxy_policy::ProxyPolicy;
use base64::Engine;
use hyper::{header::HeaderValue, rt, Uri};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder, MaybeHttpsStream};
use hyper_util::{
    client::legacy::connect::{proxy::Tunnel, Connected, Connection, HttpConnector},
    rt::TokioIo,
};
use std::{
    future::Future,
    io,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::net::TcpStream;
use tower_service::Service;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub(super) fn tls_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    } else {
        // Go overrides the file and directory searches independently, not as a pair.
        let probed = openssl_probe::probe();
        let file = std::env::var_os("SSL_CERT_FILE")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or(probed.cert_file);
        let directories = std::env::var_os("SSL_CERT_DIR")
            .filter(|value| !value.is_empty())
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or(probed.cert_dir);
        roots.add_parsable_certificates(
            rustls_native_certs::load_certs_from_paths(file.as_deref(), None).certs,
        );
        for directory in directories {
            roots.add_parsable_certificates(
                rustls_native_certs::load_certs_from_paths(None, Some(&directory)).certs,
            );
        }
    }
    // An empty trust store must not prevent plain HTTP requests.
    rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

pub(super) fn authorization(proxy: &reqwest::Url) -> Option<HeaderValue> {
    if proxy.username().is_empty() && proxy.password().is_none() {
        return None;
    }
    let credentials = [
        super::unescape(proxy.username()),
        b":".to_vec(),
        super::unescape(proxy.password().unwrap_or("")),
    ]
    .concat();
    let mut header = HeaderValue::from_str(&format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(credentials)
    ))
    .unwrap();
    header.set_sensitive(true);
    Some(header)
}

#[derive(Clone)]
pub(super) struct ProxyConnector {
    http: HttpConnector,
    proxy: HttpsConnector<HttpConnector>,
    policy: Arc<ProxyPolicy>,
}
impl ProxyConnector {
    pub(super) fn new(policy: Arc<ProxyPolicy>, tls: rustls::ClientConfig) -> Self {
        let mut http = HttpConnector::new();
        http.enforce_http(false);
        let proxy = HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .build();
        Self {
            http,
            proxy,
            policy,
        }
    }
}
impl Service<Uri> for ProxyConnector {
    type Response = ProxyStream;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<ProxyStream, BoxError>> + Send>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, destination: Uri) -> Self::Future {
        let selected = self.policy.select(&destination);
        let mut http = self.http.clone();
        let mut connector = self.proxy.clone();
        Box::pin(async move {
            let Some(mut proxy) = selected? else {
                return Ok(ProxyStream {
                    inner: MaybeHttpsStream::Http(http.call(destination).await?),
                    forward: false,
                });
            };
            let authorization = authorization(&proxy);
            let _ = proxy.set_username("");
            let _ = proxy.set_password(None);
            let proxy: Uri = proxy.as_str().parse()?;
            if destination.scheme_str() == Some("https") {
                let mut tunnel = Tunnel::new(proxy, connector);
                if let Some(authorization) = authorization {
                    tunnel = tunnel.with_auth(authorization);
                }
                return Ok(ProxyStream {
                    inner: tunnel.call(destination).await?,
                    forward: false,
                });
            }
            Ok(ProxyStream {
                inner: connector.call(proxy).await?,
                forward: true,
            })
        })
    }
}

pub(super) struct ProxyStream {
    inner: MaybeHttpsStream<TokioIo<TcpStream>>,
    forward: bool,
}
impl Connection for ProxyStream {
    fn connected(&self) -> Connected {
        self.inner.connected().proxy(self.forward)
    }
}
impl rt::Read for ProxyStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}
impl rt::Write for ProxyStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write_vectored(cx, bufs)
    }
}
