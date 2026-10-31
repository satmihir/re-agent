use reagent::transport::http::Client;
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

const CERT: &[u8] = include_bytes!("provider_proxy/cert.pem");
const KEY: &[u8] = include_bytes!("provider_proxy/key.pem");
const BODY: &[u8] = b"{\"n\":1.00,\"escaped\":\"\\u0061\"}";

#[test]
fn https_proxy_request_process() {
    let Ok(url) = std::env::var("REAGENT_HTTPS_PROXY_TEST_URL") else {
        return;
    };
    let client = Client::new(Duration::from_secs(2), Duration::from_secs(2), None).unwrap();
    let result = client
        .post(&url)
        .header("authorization", "Bearer origin-secret")
        .header("content-type", "application/json")
        .body(BODY.to_vec())
        .send();
    if std::env::var_os("REAGENT_HTTPS_PROXY_TEST_REJECT").is_some() {
        assert!(
            result.is_err(),
            "untrusted proxy must not return a response"
        );
        return;
    }
    let mut body = Vec::new();
    result.unwrap().read_to_end(&mut body).unwrap();
    assert_eq!(body, b"ok");
}

#[test]
fn https_proxy_forwards_http_absolute_form_over_tls() {
    proxy_case(false, false);
}

#[test]
fn https_proxy_connect_tunnels_nested_origin_tls() {
    proxy_case(true, false);
}

#[test]
fn untrusted_https_proxy_is_rejected_without_direct_fallback() {
    for https_origin in [false, true] {
        proxy_case(https_origin, true);
    }
}

fn proxy_case(https_origin: bool, reject: bool) {
    let directory = tempfile::tempdir().unwrap();
    let roots = directory.path().join("root.pem");
    let empty = directory.path().join("empty");
    std::fs::write(&roots, CERT).unwrap();
    std::fs::create_dir(&empty).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let proxy = format!("https://user:p%40ss@{}", listener.local_addr().unwrap());
    // The unspecified address reaches this local listener without DNS, but is not
    // subject to the proxy policy's explicit loopback-address bypass.
    let direct = TcpListener::bind("127.0.0.1:0").unwrap();
    direct.set_nonblocking(true).unwrap();
    let authority = if reject {
        format!("0.0.0.0:{}", direct.local_addr().unwrap().port())
    } else {
        "provider.invalid:8443".to_owned()
    };
    let url = format!(
        "{}://{authority}/path?q=1",
        if https_origin { "https" } else { "http" }
    );
    let server_url = url.clone();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "HTTPS proxy not reached");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept HTTPS proxy: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from_pem_slice(CERT).unwrap()],
                    PrivateKeyDer::from_pem_slice(KEY).unwrap(),
                )
                .unwrap(),
        );
        let mut proxy_tls = rustls::StreamOwned::new(
            rustls::ServerConnection::new(config.clone()).unwrap(),
            stream,
        );
        if reject {
            assert!(proxy_tls.read(&mut [0]).is_err());
            return listener;
        }
        if !https_origin {
            let headers = request(&mut proxy_tls);
            assert!(headers.starts_with(&format!("POST {server_url} HTTP/1.1\r\n")));
            assert!(headers.contains("proxy-authorization: Basic dXNlcjpwQHNz\r\n"));
            assert_origin_request(&headers);
            proxy_tls
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
            proxy_tls.flush().unwrap();
            return listener;
        }
        let connect = request(&mut proxy_tls);
        assert!(connect.starts_with(&format!("CONNECT {authority} HTTP/1.1\r\n")));
        assert!(connect.contains("Proxy-Authorization: Basic dXNlcjpwQHNz\r\n"));
        assert!(!connect.contains("origin-secret"));
        assert!(!connect.contains("content-type:"));
        proxy_tls
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .unwrap();
        proxy_tls.flush().unwrap();
        let mut origin_tls =
            rustls::StreamOwned::new(rustls::ServerConnection::new(config).unwrap(), proxy_tls);
        let headers = request(&mut origin_tls);
        assert!(headers.starts_with("POST /path?q=1 HTTP/1.1\r\n"));
        assert!(!headers.contains("proxy-authorization:"));
        assert_origin_request(&headers);
        origin_tls
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .unwrap();
        origin_tls.flush().unwrap();
        listener
    });
    // The child keeps proxy and trust variables isolated from other test threads.
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .env_clear()
        .args(["--exact", "https_proxy_request_process", "--nocapture"])
        .env("REAGENT_HTTPS_PROXY_TEST_URL", &url)
        .env(
            if https_origin {
                "HTTPS_PROXY"
            } else {
                "HTTP_PROXY"
            },
            proxy,
        )
        .env("SSL_CERT_FILE", &roots)
        .env("SSL_CERT_DIR", &empty);
    if reject {
        child
            .env("SSL_CERT_FILE", directory.path().join("missing"))
            .env("REAGENT_HTTPS_PROXY_TEST_REJECT", "1");
    }
    let output = child.output().unwrap();
    let listener = server.join().unwrap();
    assert!(
        output.status.success(),
        "https_origin={https_origin}, reject={reject}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        direct.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "must not connect directly to the origin"
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "must not retry the proxy connection"
    );
}

fn request(stream: &mut impl Read) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        assert!(bytes.len() < 16384, "request headers exceed fixture limit");
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    let headers = String::from_utf8(bytes).unwrap();
    if let Some(length) = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
    {
        assert_eq!(length.parse::<usize>().unwrap(), BODY.len());
        let mut body = vec![0; BODY.len()];
        stream.read_exact(&mut body).unwrap();
        assert_eq!(body, BODY);
    }
    headers
}

fn assert_origin_request(headers: &str) {
    assert!(headers.contains("authorization: Bearer origin-secret\r\n"));
    assert!(headers.contains("content-type: application/json\r\n"));
    assert!(headers.contains(&format!("content-length: {}\r\n", BODY.len())));
    assert!(headers.contains("accept-encoding: gzip\r\n"));
    assert!(!headers.contains("\r\naccept:"));
}
