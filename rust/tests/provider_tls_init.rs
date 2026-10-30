use reagent::transport::http::Client;
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

const CERT: &[u8] = include_bytes!("provider_proxy/cert.pem");
const KEY: &[u8] = include_bytes!("provider_proxy/key.pem");

#[test]
fn tls_init_request_process() {
    let Ok(url) = std::env::var("REAGENT_TLS_INIT_TEST_URL") else {
        return;
    };
    let cached = std::env::var_os("REAGENT_TLS_INIT_TEST_CACHE").is_some();
    for attempt in 0..if cached { 2 } else { 1 } {
        if attempt == 1 {
            std::fs::remove_file(std::env::var_os("SSL_CERT_FILE").unwrap()).unwrap();
        }
        let client = Client::new(Duration::from_secs(2), Duration::from_secs(2), None).unwrap();
        let mut response = client.get(&url).send().unwrap();
        let mut body = Vec::new();
        response.read_to_end(&mut body).unwrap();
        assert_eq!(body, b"ok");
    }
}

#[test]
fn plain_http_does_not_open_native_roots_even_with_an_unused_https_proxy() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    for proxied in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let roots = directory.path().join("roots.fifo");
        let path = CString::new(roots.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let empty = directory.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let Some(mut stream) = accept(&listener) else {
                return false;
            };
            let headers = request(&mut stream);
            assert!(headers.starts_with(if proxied {
                "GET http://provider.invalid/path HTTP/1.1\r\n"
            } else {
                "GET /path HTTP/1.1\r\n"
            }));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
            true
        });
        let mut child = Command::new(std::env::current_exe().unwrap());
        child
            .env_clear()
            .args(["--exact", "tls_init_request_process", "--nocapture"])
            .env("SSL_CERT_FILE", roots)
            .env("SSL_CERT_DIR", empty)
            .env("HTTPS_PROXY", "https://127.0.0.1:1")
            .env(
                "REAGENT_TLS_INIT_TEST_URL",
                if proxied {
                    "http://provider.invalid/path".into()
                } else {
                    format!("{endpoint}/path")
                },
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if proxied {
            child.env("HTTP_PROXY", endpoint);
        }
        let mut child = child.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let output = child.wait_with_output().unwrap();
        let reached = server.join().unwrap();
        assert!(
            output.status.success(),
            "HTTP opened blocking native roots: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(reached);
    }
}

#[test]
fn native_roots_are_cached_across_clients_after_first_tls_use() {
    let directory = tempfile::tempdir().unwrap();
    let roots = directory.path().join("root.pem");
    let empty = directory.path().join("empty");
    std::fs::write(&roots, CERT).unwrap();
    std::fs::create_dir(&empty).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("https://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let config = Arc::new(
            rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from_pem_slice(CERT).unwrap()],
                    PrivateKeyDer::from_pem_slice(KEY).unwrap(),
                )
                .unwrap(),
        );
        for _ in 0..2 {
            let stream = accept(&listener).expect("TLS client did not connect");
            let mut stream = rustls::StreamOwned::new(
                rustls::ServerConnection::new(config.clone()).unwrap(),
                stream,
            );
            assert!(request(&mut stream).starts_with("GET /path HTTP/1.1\r\n"));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .unwrap();
            stream.flush().unwrap();
        }
    });
    let output = Command::new(std::env::current_exe().unwrap())
        .env_clear()
        .args(["--exact", "tls_init_request_process", "--nocapture"])
        .env("SSL_CERT_FILE", &roots)
        .env("SSL_CERT_DIR", empty)
        .env("REAGENT_TLS_INIT_TEST_URL", format!("{endpoint}/path"))
        .env("REAGENT_TLS_INIT_TEST_CACHE", "1")
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!roots.exists());
}

fn accept(listener: &TcpListener) -> Option<TcpStream> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok((stream, _)) = listener.accept() {
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            return Some(stream);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn request(stream: &mut impl Read) -> String {
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        assert!(headers.len() < 16384);
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
    }
    String::from_utf8(headers).unwrap()
}
