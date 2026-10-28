use reqwest::Url;
use std::{io, net::IpAddr};

/// ProxyPolicy snapshots Go-compatible HTTP proxy environment settings.
pub(super) struct ProxyPolicy {
    http: Option<Url>,
    https: Option<Url>,
    no_proxy: String,
    cgi: bool,
}

impl ProxyPolicy {
    /// from_env reads proxy settings without consulting ALL_PROXY.
    pub(super) fn from_env() -> Self {
        Self::read_env(|name| std::env::var(name).unwrap_or_default())
    }

    fn read_env(read: impl Fn(&str) -> String) -> Self {
        let first = |upper, lower| {
            let value = read(upper);
            if value.is_empty() {
                read(lower)
            } else {
                value
            }
        };
        Self {
            http: parse_proxy(&first("HTTP_PROXY", "http_proxy")),
            https: parse_proxy(&first("HTTPS_PROXY", "https_proxy")),
            no_proxy: first("NO_PROXY", "no_proxy"),
            cgi: !read("REQUEST_METHOD").is_empty(),
        }
    }

    /// select returns the proxy for a destination, retaining its URL credentials.
    pub(super) fn select(&self, destination: &hyper::Uri) -> io::Result<Option<Url>> {
        let proxy = match destination.scheme_str() {
            Some("https") => self.https.as_ref(),
            Some("http") => {
                if self.http.is_some() && self.cgi {
                    return Err(io::Error::other(
                        "refusing to use HTTP_PROXY value in CGI environment; see golang.org/s/cgihttpproxy",
                    ));
                }
                self.http.as_ref()
            }
            _ => None,
        };
        let Some(proxy) = proxy else {
            return Ok(None);
        };
        let host = destination.host().unwrap_or("");
        let host = ascii_host(host.trim_start_matches('[').trim_end_matches(']'));
        let ip = host.parse::<IpAddr>().ok().map(normalize_ip);
        if host == "localhost" || ip.is_some_and(|ip| ip.is_loopback()) {
            return Ok(None);
        }
        let explicit_port = destination.port();
        let port = explicit_port.as_ref().map_or(
            if destination.scheme_str() == Some("https") {
                "443"
            } else {
                "80"
            },
            |port| port.as_str(),
        );
        let host = host.trim().to_lowercase();
        if self
            .no_proxy
            .split(',')
            .any(|entry| bypass(entry, &host, port, ip))
        {
            return Ok(None);
        }
        Ok(Some(proxy.clone()))
    }
}

fn parse_proxy(value: &str) -> Option<Url> {
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
        return None;
    }
    let url = Url::parse(value)
        .ok()
        .filter(|url| url.has_host())
        .or_else(|| Url::parse(&format!("http://{value}")).ok())?;
    (matches!(url.scheme(), "http" | "https") && url.has_host()).then_some(url)
}

fn ascii_host(host: &str) -> String {
    // Go leaves ASCII hostnames alone, including non-DNS names and trailing dots.
    if !host.is_ascii() {
        if let Ok(url) = Url::parse(&format!("http://{host}")) {
            if let Some(host) = url.host_str() {
                return host.to_owned();
            }
        }
    }
    host.to_owned()
}

fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map_or(IpAddr::V6(ip), IpAddr::V4),
        ip => ip,
    }
}

fn bypass(entry: &str, host: &str, port: &str, ip: Option<IpAddr>) -> bool {
    let entry = entry.trim().to_lowercase();
    if entry == "*" {
        return true;
    }
    if entry.is_empty() {
        return false;
    }
    if let Some((network, prefix)) = entry.split_once('/') {
        if let (Ok(network), Ok(prefix)) = (network.parse::<IpAddr>(), prefix.parse::<u32>()) {
            if prefix <= if network.is_ipv4() { 32 } else { 128 } {
                return ip.is_some_and(|ip| in_network(ip, network, prefix));
            }
        }
    }
    let (pattern, pattern_port) = if let Some(bracketed) = entry.strip_prefix('[') {
        match bracketed.split_once("]:") {
            Some((host, port)) if !host.contains(['[', ']']) => (host, port),
            _ => (entry.as_str(), ""),
        }
    } else if entry.matches(':').count() == 1 {
        entry.split_once(':').unwrap()
    } else {
        (entry.as_str(), "")
    };
    if pattern.is_empty() || (!pattern_port.is_empty() && pattern_port != port) {
        return false;
    }
    if let Ok(pattern_ip) = pattern.parse::<IpAddr>() {
        return ip == Some(normalize_ip(pattern_ip));
    }
    // Retain the dot so wildcard rules cannot match the apex.
    let pattern = if pattern.starts_with("*.") {
        &pattern[1..]
    } else {
        pattern
    };
    let pattern = ascii_host(pattern);
    if pattern.starts_with('.') {
        return host.ends_with(&pattern);
    }
    host == pattern || host.ends_with(&format!(".{pattern}"))
}

fn in_network(ip: IpAddr, network: IpAddr, prefix: u32) -> bool {
    let (network, prefix) = match network {
        IpAddr::V6(network) if prefix >= 96 && network.to_ipv4_mapped().is_some() => {
            (IpAddr::V4(network.to_ipv4_mapped().unwrap()), prefix - 96)
        }
        network => (network, prefix),
    };
    match (ip, network) {
        (IpAddr::V4(ip), IpAddr::V4(network)) => {
            let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
            u32::from(ip) & mask == u32::from(network) & mask
        }
        (IpAddr::V6(ip), IpAddr::V6(network)) => {
            let mask = u128::MAX.checked_shl(128 - prefix).unwrap_or(0);
            u128::from(ip) & mask == u128::from(network) & mask
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(values: &[(&str, &str)]) -> ProxyPolicy {
        ProxyPolicy::read_env(|name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map_or("", |(_, value)| *value)
                .to_owned()
        })
    }

    #[test]
    fn environment_precedence_and_proxy_parsing() {
        let cases: &[(&str, &[(&str, &str)], &str, Option<&str>)] = &[
            ("unset", &[], "http://example.com", None),
            (
                "uppercase wins",
                &[("HTTP_PROXY", "upper:8000"), ("http_proxy", "lower:9000")],
                "http://example.com",
                Some("http://upper:8000/"),
            ),
            (
                "empty uppercase falls back",
                &[("HTTP_PROXY", ""), ("http_proxy", "lower:9000")],
                "http://example.com",
                Some("http://lower:9000/"),
            ),
            (
                "https uppercase wins",
                &[
                    ("HTTPS_PROXY", "https://upper:8443"),
                    ("https_proxy", "lower"),
                ],
                "https://example.com",
                Some("https://upper:8443/"),
            ),
            (
                "https lowercase fallback",
                &[("HTTPS_PROXY", ""), ("https_proxy", "lower")],
                "https://example.com",
                Some("http://lower/"),
            ),
            (
                "credentials retained",
                &[("HTTP_PROXY", "https://user:p%40ss@proxy:8443")],
                "http://example.com",
                Some("https://user:p%40ss@proxy:8443/"),
            ),
            (
                "no scheme crossover",
                &[("HTTP_PROXY", "proxy")],
                "https://example.com",
                None,
            ),
            (
                "ALL_PROXY ignored",
                &[("ALL_PROXY", "proxy"), ("all_proxy", "proxy")],
                "http://example.com",
                None,
            ),
            (
                "invalid uppercase does not fall back",
                &[("HTTP_PROXY", "[bad"), ("http_proxy", "proxy")],
                "http://example.com",
                None,
            ),
            (
                "invalid proxy ignored",
                &[("HTTP_PROXY", "bad host")],
                "http://example.com",
                None,
            ),
            (
                "control character not silently stripped",
                &[("HTTP_PROXY", "http://proxy\n")],
                "http://example.com",
                None,
            ),
            (
                "whitespace is nonempty precedence",
                &[("HTTP_PROXY", " "), ("http_proxy", "proxy")],
                "http://example.com",
                None,
            ),
            (
                "uppercase no_proxy wins",
                &[
                    ("HTTP_PROXY", "proxy"),
                    ("NO_PROXY", "other.com"),
                    ("no_proxy", "*"),
                ],
                "http://example.com",
                Some("http://proxy/"),
            ),
            (
                "empty no_proxy falls back",
                &[("HTTP_PROXY", "proxy"), ("NO_PROXY", ""), ("no_proxy", "*")],
                "http://example.com",
                None,
            ),
            (
                "unknown scheme",
                &[("HTTPS_PROXY", "proxy"), ("HTTP_PROXY", "proxy")],
                "ftp://example.com",
                None,
            ),
        ];
        for (name, values, destination, expected) in cases {
            let result = policy(values)
                .select(&destination.parse().unwrap())
                .unwrap();
            assert_eq!(result.as_ref().map(Url::as_str), *expected, "{name}");
        }
    }

    #[test]
    fn no_proxy_boundaries_without_dns() {
        let cases = [
            ("", "http://localhost", true),
            ("", "http://localhost:8080", true),
            ("", "http://LOCALHOST", false),
            ("", "http://localhost.", false),
            ("", "http://sub.localhost", false),
            ("", "http://localhost.localdomain", false),
            ("", "http://127.23.4.5", true),
            ("", "http://[::1]", true),
            ("", "http://[::ffff:127.0.0.2]", true),
            (" * ", "https://example.com", true),
            ("example.com", "http://example.com", true),
            (" EXAMPLE.COM ", "http://Sub.Example.Com", true),
            ("example.com", "http://notexample.com", false),
            ("example.com", "http://example.com.evil", false),
            (".example.com", "http://example.com", false),
            (".example.com", "http://sub.example.com", true),
            ("*.example.com", "http://example.com", false),
            ("*.example.com", "http://sub.example.com", true),
            ("example.com:80", "http://example.com", true),
            ("example.com:443", "https://example.com", true),
            ("example.com:80", "https://example.com", false),
            ("example.com:8080", "http://example.com:8080", true),
            ("example.com:080", "http://example.com", false),
            ("example.com:", "https://example.com:8443", true),
            ("example.com.", "http://example.com", false),
            ("example.com.", "http://example.com.", true),
            ("bücher.example", "http://xn--bcher-kva.example", true),
            (".bücher.example", "http://sub.xn--bcher-kva.example", true),
            ("192.0.2.1", "http://192.0.2.1", true),
            ("192.0.2.1:80", "http://192.0.2.1", true),
            ("192.0.2.1:443", "http://192.0.2.1", false),
            ("192.0.2.1", "http://[::ffff:192.0.2.1]", true),
            ("::ffff:192.0.2.1", "http://192.0.2.1", true),
            ("2001:db8::1", "http://[2001:db8::1]", true),
            ("[2001:db8::1]:443", "https://[2001:db8::1]", true),
            ("[2001:db8::1]:80", "https://[2001:db8::1]", false),
            ("[2001:db8::1]", "https://[2001:db8::1]", false),
            ("192.0.2.0/24", "http://192.0.2.255", true),
            ("192.0.2.0/24", "http://192.0.3.1", false),
            ("192.0.2.0/24", "http://[::ffff:192.0.2.1]", true),
            ("::ffff:192.0.2.0/120", "http://192.0.2.1", true),
            ("2001:db8::/32", "http://[2001:db8:ffff::1]", true),
            ("2001:db8::/32", "http://[2001:db9::1]", false),
            ("0.0.0.0/0", "http://192.0.2.1", true),
            ("::/0", "http://[2001:db8::1]", true),
            ("::/0", "http://192.0.2.1", false),
            ("192.0.2.0/24", "http://example.com", false),
            ("192.0.2.0/33", "http://192.0.2.1", false),
            ("2001:db8::/129", "http://[2001:db8::1]", false),
            (
                ", :80, [bad], bad/xyz, *example.com",
                "http://example.com",
                false,
            ),
            (", bad, example.com ,", "http://example.com", true),
        ];
        for (no_proxy, destination, expected_bypass) in cases {
            let result = policy(&[
                ("HTTP_PROXY", "proxy:8080"),
                ("HTTPS_PROXY", "proxy:8080"),
                ("NO_PROXY", no_proxy),
            ])
            .select(&destination.parse().unwrap())
            .unwrap();
            assert_eq!(
                result.is_none(),
                expected_bypass,
                "{no_proxy:?} {destination}"
            );
        }
    }

    #[test]
    fn cgi_refusal_precedes_exclusions() {
        let cases = [
            ("GET", "proxy", "http://example.com", "", true, false),
            (" ", "proxy", "http://example.com", "", true, false),
            ("GET", "proxy", "http://localhost", "", true, false),
            ("GET", "proxy", "http://127.0.0.1", "", true, false),
            ("GET", "proxy", "http://example.com", "*", true, false),
            ("", "proxy", "http://example.com", "", false, true),
            ("GET", "", "http://example.com", "", false, false),
            ("GET", "[bad", "http://localhost", "", false, false),
            ("GET", "proxy", "https://example.com", "", false, true),
            ("GET", "proxy", "https://localhost", "", false, false),
        ];
        for (method, proxy, destination, no_proxy, error, selected) in cases {
            let result = policy(&[
                ("REQUEST_METHOD", method),
                ("http_proxy", proxy),
                ("HTTPS_PROXY", "proxy"),
                ("NO_PROXY", no_proxy),
            ])
            .select(&destination.parse().unwrap());
            assert_eq!(result.is_err(), error, "{method} {proxy} {destination}");
            if !error {
                assert_eq!(result.unwrap().is_some(), selected, "{destination}");
            }
        }
    }
}
