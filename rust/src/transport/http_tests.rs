use super::*;
use std::{
    io::Write,
    net::{TcpListener, TcpStream},
    thread,
};

pub(crate) fn server(
    action: impl FnOnce(TcpStream) + Send + 'static,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        action(stream);
    });
    (url, handle)
}
pub(crate) fn request(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut byte = [0];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
    }
    let headers = String::from_utf8(bytes).unwrap().to_lowercase();
    assert!(
        !headers.lines().any(|line| line.starts_with("accept:")),
        "{headers}"
    );
    assert!(headers.contains("user-agent: go-http-client/1.1\r\n"));
    let length = headers
        .lines()
        .find_map(|line| line.strip_prefix("content-length: "))
        .unwrap_or("0")
        .parse()
        .unwrap();
    stream.read_exact(&mut vec![0; length]).unwrap();
    headers
}
fn client(header: u64, idle: u64) -> Client {
    Client::new(
        Duration::from_millis(header),
        Duration::from_millis(idle),
        None,
    )
    .unwrap()
}

#[test]
fn explicit_headers_basic_auth_and_no_redirect_following() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://u:p%61ss@{}/start", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let headers = request(&mut stream);
        assert!(headers.contains("authorization: basic dtpwyxnz\r\n"));
        assert!(headers.contains("accept-encoding: gzip\r\n"));
        stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: /leak\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        thread::sleep(Duration::from_millis(60));
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    });
    let response = client(500, 500).get(url).send().unwrap();
    assert_eq!(response.status(), 302);
    server.join().unwrap();
}
#[test]
fn gzip_is_decompressed_only_when_automatically_requested() {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(b"decoded response").unwrap();
    let compressed = encoder.finish().unwrap();
    for automatic in [true, false] {
        let payload = compressed.clone();
        let (url, server) = server(move |mut stream| {
            let headers = request(&mut stream);
            assert!(headers.contains("accept-encoding: gzip\r\n"));
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",payload.len()).unwrap();
            stream.write_all(&payload).unwrap();
        });
        let client = client(500, 500);
        let mut req = client.get(url);
        if !automatic {
            req = req.header("accept-encoding", "gzip");
        }
        let mut response = req.send().unwrap();
        assert_eq!(
            response.headers().contains_key("content-encoding"),
            !automatic
        );
        let mut body = Vec::new();
        response.read_to_end(&mut body).unwrap();
        assert_eq!(
            body,
            if automatic {
                b"decoded response".to_vec()
            } else {
                compressed.clone()
            }
        );
        server.join().unwrap();
    }
}
#[test]
fn response_header_timeout_is_separate_from_body_idle_timeout() {
    let (url, server) = server(|mut stream| {
        request(&mut stream);
        thread::sleep(Duration::from_millis(120));
        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
    });
    let error = client(40, 500).get(url).send().err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    server.join().unwrap();
}
#[test]
fn body_stall_keeps_headers_and_partial_body() {
    let (url, server) = server(|mut stream| {
        request(&mut stream);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nX-Request-Id: kept\r\nContent-Length: 2\r\n\r\na")
            .unwrap();
        thread::sleep(Duration::from_millis(120));
        let _ = stream.write_all(b"b");
    });
    let mut response = client(500, 40).get(url).send().unwrap();
    let mut body = Vec::new();
    assert_eq!(
        response.read_to_end(&mut body).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["x-request-id"], "kept");
    assert_eq!(body, b"a");
    server.join().unwrap();
}
#[test]
fn progressing_chunks_reset_idle_without_whole_response_timeout() {
    let (url, server) = server(|mut stream| {
        request(&mut stream);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\n")
            .unwrap();
        for _ in 0..4 {
            thread::sleep(Duration::from_millis(40));
            stream.write_all(b"a").unwrap();
        }
    });
    let mut response = client(70, 100).get(url).send().unwrap();
    let mut body = Vec::new();
    response.read_to_end(&mut body).unwrap();
    assert_eq!(body, b"aaaa");
    server.join().unwrap();
}
#[test]
fn cancellation_interrupts_header_and_body_waits() {
    for headers in [false, true] {
        let ready = Arc::new(AtomicBool::new(false));
        let received = ready.clone();
        let (url, server) = server(move |mut stream| {
            request(&mut stream);
            received.store(true, Ordering::SeqCst);
            if headers {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n")
                    .unwrap();
            }
            thread::sleep(Duration::from_millis(150));
        });
        let token = Arc::new(AtomicBool::new(false));
        let signal = token.clone();
        let cancel = thread::spawn(move || {
            let end = Instant::now() + Duration::from_secs(2);
            while !ready.load(Ordering::SeqCst) && Instant::now() < end {
                thread::sleep(Duration::from_millis(2));
            }
            thread::sleep(Duration::from_millis(40));
            signal.store(true, Ordering::SeqCst);
        });
        let mut client = client(500, 500);
        client.cancellation = Some(token);
        let error = if headers {
            let mut response = client.get(url).send().unwrap();
            response.read_to_end(&mut Vec::new()).unwrap_err()
        } else {
            client.get(url).send().err().unwrap()
        };
        assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
        cancel.join().unwrap();
        server.join().unwrap();
    }
}

pub(crate) fn no_accept_server(body: Vec<u8>) -> (String, thread::JoinHandle<()>) {
    server(move |mut stream| {
        let headers = request(&mut stream);
        assert!(headers.contains("accept-encoding: gzip\r\n"));
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
    })
}
