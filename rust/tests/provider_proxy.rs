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

#[test]
fn proxy_request_process() {
    let Ok(url) = std::env::var("REAGENT_PROXY_TEST_URL") else {
        return;
    };
    let stalled = std::env::var_os("REAGENT_PROXY_TEST_STALL").is_some();
    let header = if stalled {
        Duration::from_millis(100)
    } else {
        Duration::from_secs(2)
    };
    let client = Client::new(header, Duration::from_secs(2), None).unwrap();
    let result = client
        .post(&url)
        .header("authorization", "Bearer origin-secret")
        .header("content-type", "application/json")
        .body(b"{\"n\":1.00}".to_vec())
        .send();
    if std::env::var_os("REAGENT_PROXY_TEST_REJECT").is_some() {
        assert!(result.is_err());
        if stalled {
            assert_eq!(result.err().unwrap().kind(), std::io::ErrorKind::TimedOut);
        }
        return;
    }
    let mut body = Vec::new();
    result.unwrap().read_to_end(&mut body).unwrap();
    assert_eq!(body, b"ok");
}

#[test]
fn environment_proxy_preserves_wire_and_connect_uses_native_trust() {
    // Child processes isolate environment variables from parallel transport tests.
    for mode in ["http", "file", "directories", "untrusted", "wrong-host"] {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(second.join("root.pem"), CERT).unwrap();
        let certificate = directory.path().join("root.pem");
        std::fs::write(&certificate, CERT).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let proxy = format!("http://user:p%40ss@{}", listener.local_addr().unwrap());
        let tls = mode != "http";
        let reject = matches!(mode, "untrusted" | "wrong-host");
        let host = if mode == "wrong-host" {
            "different.invalid"
        } else {
            "provider.invalid"
        };
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                if let Ok((stream, _)) = listener.accept() {
                    break stream;
                }
                assert!(Instant::now() < deadline, "proxy not reached");
                std::thread::sleep(Duration::from_millis(5));
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            if !tls {
                let request = request(&mut stream);
                assert!(
                    request.starts_with("POST http://provider.invalid:8443/path?q=1 HTTP/1.1\r\n")
                );
                assert!(request.contains("proxy-authorization: Basic dXNlcjpwQHNz\r\n"));
                assert_origin_request(&request);
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .unwrap();
                return;
            }
            let connect = request(&mut stream);
            assert!(connect.starts_with(&format!("CONNECT {host}:8443 HTTP/1.1\r\n")));
            assert!(connect.contains("Proxy-Authorization: Basic dXNlcjpwQHNz\r\n"));
            assert!(!connect.contains("origin-secret"));
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .unwrap();
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from_pem_slice(CERT).unwrap()],
                    PrivateKeyDer::from_pem_slice(KEY).unwrap(),
                )
                .unwrap();
            let mut tls = rustls::StreamOwned::new(
                rustls::ServerConnection::new(Arc::new(config)).unwrap(),
                stream,
            );
            if reject {
                assert!(tls.read(&mut [0]).is_err());
                return;
            }
            let request = request(&mut tls);
            assert!(request.starts_with("POST /path?q=1 HTTP/1.1\r\n"));
            assert!(!request.to_lowercase().contains("proxy-authorization"));
            assert_origin_request(&request);
            tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
            tls.flush().unwrap();
        });
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .env_clear()
            .args(["--exact", "proxy_request_process", "--nocapture"])
            .env(
                "REAGENT_PROXY_TEST_URL",
                format!(
                    "{}://{host}:8443/path?q=1",
                    if tls { "https" } else { "http" }
                ),
            )
            .env(if tls { "HTTPS_PROXY" } else { "HTTP_PROXY" }, proxy)
            .env("SSL_CERT_FILE", directory.path().join("missing"))
            .env("SSL_CERT_DIR", &first);
        if matches!(mode, "file" | "wrong-host") {
            child.env("SSL_CERT_FILE", &certificate);
        }
        if mode == "directories" {
            child.env(
                "SSL_CERT_DIR",
                std::env::join_paths([&first, &second]).unwrap(),
            );
        }
        if reject {
            child.env("REAGENT_PROXY_TEST_REJECT", "1");
        }
        let output = child.output().unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn request(stream: &mut impl Read) -> String {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
    }
    let headers = String::from_utf8(headers).unwrap();
    if let Some(length) = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
    {
        let mut body = vec![0; length.parse::<usize>().unwrap()];
        stream.read_exact(&mut body).unwrap();
        assert_eq!(body, b"{\"n\":1.00}");
    }
    headers
}

fn assert_origin_request(request: &str) {
    assert!(request.contains("authorization: Bearer origin-secret\r\n"));
    assert!(request.contains("content-type: application/json\r\n"));
    assert!(request.contains("accept-encoding: gzip\r\n"));
    assert!(!request.contains("\r\naccept:"));
}

#[test]
fn socks5_keeps_dns_remote_and_authentication_out_of_origin_headers() {
    for (scheme, tls, credentials, method) in [
        ("socks5", false, false, 0),
        ("socks5h", false, true, 0),
        ("socks5", false, true, 2),
        ("socks5h", true, true, 2),
        ("socks5", false, true, 255),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let proxy = format!(
            "{scheme}://{}{}",
            if credentials { "user:p%40ss@" } else { "" },
            listener.local_addr().unwrap()
        );
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                if let Ok((stream, _)) = listener.accept() {
                    break stream;
                }
                assert!(Instant::now() < deadline, "SOCKS proxy not reached");
                std::thread::sleep(Duration::from_millis(5));
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut greeting = [0; 2];
            stream.read_exact(&mut greeting).unwrap();
            assert_eq!(greeting, [5, if credentials { 2 } else { 1 }]);
            let mut methods = vec![0; greeting[1] as usize];
            stream.read_exact(&mut methods).unwrap();
            assert_eq!(methods, if credentials { vec![0, 2] } else { vec![0] });
            stream.write_all(&[5, method]).unwrap();
            if method == 255 {
                return;
            }
            if method == 2 {
                let mut authentication = [0; 2];
                stream.read_exact(&mut authentication).unwrap();
                assert_eq!(authentication, [1, 4]);
                let mut user = [0; 4];
                stream.read_exact(&mut user).unwrap();
                assert_eq!(&user, b"user");
                let mut length = [0];
                stream.read_exact(&mut length).unwrap();
                let mut password = vec![0; length[0] as usize];
                stream.read_exact(&mut password).unwrap();
                assert_eq!(password, b"p@ss");
                stream.write_all(&[1, 0]).unwrap();
            }
            let mut connect = [0; 5];
            stream.read_exact(&mut connect).unwrap();
            assert_eq!(connect, [5, 1, 0, 3, 16]);
            let mut host = [0; 16];
            stream.read_exact(&mut host).unwrap();
            assert_eq!(&host, b"provider.invalid");
            let mut port = [0; 2];
            stream.read_exact(&mut port).unwrap();
            assert_eq!(u16::from_be_bytes(port), 8443);
            stream.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).unwrap();
            if !tls {
                let request = request(&mut stream);
                assert!(request.starts_with("POST /path HTTP/1.1\r\n"));
                assert!(!request.to_lowercase().contains("proxy-authorization"));
                assert_origin_request(&request);
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .unwrap();
                return;
            }
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from_pem_slice(CERT).unwrap()],
                    PrivateKeyDer::from_pem_slice(KEY).unwrap(),
                )
                .unwrap();
            let mut stream = rustls::StreamOwned::new(
                rustls::ServerConnection::new(Arc::new(config)).unwrap(),
                stream,
            );
            let request = request(&mut stream);
            assert!(request.starts_with("POST /path HTTP/1.1\r\n"));
            assert!(!request.to_lowercase().contains("proxy-authorization"));
            assert_origin_request(&request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .unwrap();
            stream.flush().unwrap();
        });
        let directory = tempfile::tempdir().unwrap();
        let certificate = directory.path().join("root.pem");
        std::fs::write(&certificate, CERT).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .env_clear()
            .args(["--exact", "proxy_request_process", "--nocapture"])
            .env(
                "REAGENT_PROXY_TEST_URL",
                format!(
                    "{}://provider.invalid:8443/path",
                    if tls { "https" } else { "http" }
                ),
            )
            .env(if tls { "HTTPS_PROXY" } else { "HTTP_PROXY" }, proxy)
            .env("SSL_CERT_FILE", certificate)
            .env("SSL_CERT_DIR", directory.path());
        if method == 255 {
            child.env("REAGENT_PROXY_TEST_REJECT", "1");
        }
        let output = child.output().unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{scheme} TLS={tls} method={method}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn socks5_stalled_handshake_uses_the_existing_header_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let proxy = format!("socks5://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            if let Ok((stream, _)) = listener.accept() {
                break stream;
            }
            assert!(Instant::now() < deadline, "SOCKS proxy not reached");
            std::thread::sleep(Duration::from_millis(5));
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut greeting = [0; 3];
        stream.read_exact(&mut greeting).unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        assert_eq!(stream.read(&mut [0]).unwrap(), 0);
    });
    let output = Command::new(std::env::current_exe().unwrap())
        .env_clear()
        .args(["--exact", "proxy_request_process", "--nocapture"])
        .env("HTTP_PROXY", proxy)
        .env("REAGENT_PROXY_TEST_URL", "http://provider.invalid/path")
        .env("REAGENT_PROXY_TEST_REJECT", "1")
        .env("REAGENT_PROXY_TEST_STALL", "1")
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
