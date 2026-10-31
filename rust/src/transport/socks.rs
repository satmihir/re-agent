use hyper::Uri;
use hyper_util::{client::legacy::connect::HttpConnector, rt::TokioIo};
use std::{io, net::IpAddr};
use tokio::net::TcpStream;
use tower_service::Service;

// Hyper's SOCKS utility offers only one method; Go offers no-auth with credentials too.
pub(super) async fn connect(
    proxy: &reqwest::Url,
    destination: &Uri,
    connector: &mut HttpConnector,
) -> Result<TokioIo<TcpStream>, Box<dyn std::error::Error + Send + Sync>> {
    let address: Uri = format!(
        "http://{}:{}",
        proxy.host_str().unwrap_or(""),
        proxy.port().unwrap_or(1080)
    )
    .parse()?;
    let stream = connector.call(address).await?;
    let tcp = stream.inner();
    let credentials = !proxy.username().is_empty() || proxy.password().is_some();
    write(
        tcp,
        if credentials {
            &[5, 2, 0, 2]
        } else {
            &[5, 1, 0]
        },
    )
    .await?;
    let mut response = [0; 2];
    read(tcp, &mut response).await?;
    if response[0] != 5 || !matches!(response[1], 0 | 2) || (response[1] == 2 && !credentials) {
        return Err(io::Error::other("unsupported SOCKS5 authentication method").into());
    }
    if response[1] == 2 {
        let user = super::unescape(proxy.username());
        let password = super::unescape(proxy.password().unwrap_or(""));
        if user.is_empty() || user.len() > 255 || password.len() > 255 {
            return Err(io::Error::other("invalid SOCKS5 username/password").into());
        }
        let mut authentication = vec![1, user.len() as u8];
        authentication.extend(user);
        authentication.push(password.len() as u8);
        authentication.extend(password);
        write(tcp, &authentication).await?;
        read(tcp, &mut response).await?;
        if response != [1, 0] {
            return Err(io::Error::other("SOCKS5 username/password authentication failed").into());
        }
    }
    let mut request = vec![5, 1, 0];
    let host = destination
        .host()
        .unwrap_or("")
        .trim_start_matches('[')
        .trim_end_matches(']');
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            request.push(1);
            request.extend(ip.octets());
        }
        Ok(IpAddr::V6(ip)) => {
            request.push(4);
            request.extend(ip.octets());
        }
        Err(_) if !host.is_empty() && host.len() <= 255 => {
            // Both socks5 and socks5h leave DNS to the proxy in Go's HTTP transport.
            request.extend([3, host.len() as u8]);
            request.extend(host.as_bytes());
        }
        Err(_) => return Err(io::Error::other("invalid SOCKS5 destination").into()),
    }
    let port = destination
        .port_u16()
        .unwrap_or(if destination.scheme_str() == Some("https") {
            443
        } else {
            80
        });
    request.extend(port.to_be_bytes());
    write(tcp, &request).await?;
    let mut reply = [0; 4];
    read(tcp, &mut reply).await?;
    if reply[0] != 5 || reply[1] != 0 || reply[2] != 0 {
        return Err(io::Error::other("SOCKS5 connect failed").into());
    }
    let length = match reply[3] {
        1 => 4,
        4 => 16,
        3 => {
            read(tcp, &mut response[..1]).await?;
            response[0] as usize
        }
        _ => return Err(io::Error::other("invalid SOCKS5 bound address").into()),
    };
    read(tcp, &mut vec![0; length + 2]).await?;
    Ok(stream)
}

async fn read(stream: &TcpStream, mut bytes: &mut [u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.readable().await?;
        match stream.try_read(bytes) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(count) => bytes = &mut bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

async fn write(stream: &TcpStream, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.writable().await?;
        match stream.try_write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
