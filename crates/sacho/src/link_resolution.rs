//! HTTP redirect resolution for reference links.

use std::time::Duration;

use ureq::Agent;
use url::Url;

use crate::config::Config;
use crate::error::{Error, Result, redact_url_credentials};

const MAX_REDIRECTS: usize = 10;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Controls whether a command follows redirects for unpinned reference links.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LinkResolutionPolicy {
    /// Never make network requests for reference links.
    #[default]
    Never,

    /// Follow the repository's `[link-resolution]` setting.
    Configured,

    /// Resolve every unpinned reference link.
    Always,
}

impl LinkResolutionPolicy {
    /// Returns whether this policy enables network resolution for a repository.
    pub fn is_enabled(self, config: &Config) -> bool {
        match self {
            Self::Never => false,
            Self::Configured => config.link_resolution.enabled,
            Self::Always => true,
        }
    }
}

pub(crate) fn is_http_reference_url(value: &str) -> bool {
    matches!(
        Url::parse(value),
        Ok(url) if matches!(url.scheme(), "http" | "https")
    )
}

/// Resolves one expanded reference URL and returns its final effective URL.
pub fn resolve_reference_url(label: &str, source_url: &str) -> Result<String> {
    ReferenceUrlResolver::new().resolve(label, source_url)
}

pub(crate) struct ReferenceUrlResolver {
    agent: Agent,
}

impl ReferenceUrlResolver {
    pub(crate) fn new() -> Self {
        let agent: Agent = Agent::config_builder()
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .user_agent(format!("sacho/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { agent }
    }

    pub(crate) fn resolve(&self, label: &str, source_url: &str) -> Result<String> {
        let (response, resolved) =
            request_following_redirects(&self.agent, label, source_url, RequestMethod::Head)?;
        let status = response.status().as_u16();
        if matches!(status, 405 | 501) {
            let (response, resolved) = request_following_redirects(
                &self.agent,
                label,
                source_url,
                RequestMethod::RangedGet,
            )?;
            return resolved_response_url(label, source_url, response, resolved);
        }
        resolved_response_url(label, source_url, response, resolved)
    }
}

#[derive(Clone, Copy)]
enum RequestMethod {
    Head,
    RangedGet,
}

fn request_following_redirects(
    agent: &Agent,
    label: &str,
    source_url: &str,
    method: RequestMethod,
) -> Result<(ureq::http::Response<ureq::Body>, Url)> {
    let mut current = validate_request_url(label, source_url)?;
    for redirect_count in 0..=MAX_REDIRECTS {
        let response = match method {
            RequestMethod::Head => agent.head(current.as_str()).call(),
            RequestMethod::RangedGet => agent
                .get(current.as_str())
                .header("Range", "bytes=0-0")
                .call(),
        }
        .map_err(|source| resolution_error(label, current.as_str(), source))?;
        let status = response.status().as_u16();
        if !matches!(status, 301 | 302 | 303 | 307 | 308) {
            return Ok((response, current));
        }
        if redirect_count == MAX_REDIRECTS {
            return Err(link_resolution_failure(
                label,
                source_url,
                format!("more than {MAX_REDIRECTS} redirects"),
            ));
        }
        let location = response
            .headers()
            .get("Location")
            .ok_or_else(|| {
                link_resolution_failure(
                    label,
                    current.as_str(),
                    format!("HTTP redirect status {status} has no Location header"),
                )
            })?
            .to_str()
            .map_err(|source| {
                link_resolution_failure(
                    label,
                    current.as_str(),
                    format!("invalid Location header: {source}"),
                )
            })?;
        let next = current.join(location).map_err(|source| {
            link_resolution_failure(label, location, format!("invalid redirect URL: {source}"))
        })?;
        validate_request_url(label, next.as_str())?;
        current = next;
    }
    unreachable!("the redirect loop always returns")
}

fn resolved_response_url(
    label: &str,
    source_url: &str,
    response: ureq::http::Response<ureq::Body>,
    resolved: Url,
) -> Result<String> {
    if !response.status().is_success() {
        return Err(link_resolution_failure(
            label,
            source_url,
            format!("HTTP status {}", response.status().as_u16()),
        ));
    }
    Ok(resolved.into())
}

fn validate_request_url(label: &str, value: &str) -> Result<Url> {
    let url = Url::parse(value)
        .map_err(|source| link_resolution_failure(label, value, source.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(link_resolution_failure(
            label,
            value,
            "scheme must be http or https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(link_resolution_failure(
            label,
            value,
            "userinfo is not allowed",
        ));
    }
    Ok(url)
}

fn resolution_error(label: &str, url: &str, source: ureq::Error) -> Error {
    link_resolution_failure(label, url, source.to_string())
}

fn link_resolution_failure(label: &str, url: &str, reason: impl Into<String>) -> Error {
    Error::LinkResolutionFailed {
        label: label.to_owned(),
        url: redact_url_credentials(url),
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use super::*;

    struct TestServer {
        base: String,
        address: SocketAddr,
        response_count: usize,
        handle: Option<thread::JoinHandle<Vec<String>>>,
    }

    struct ConnectionCountingServer {
        base: String,
        handle: Option<JoinHandle<(usize, Vec<String>)>>,
    }

    impl ConnectionCountingServer {
        fn spawn(expected_requests: usize) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let address = listener.local_addr().expect("address");
            let base = format!("http://{address}");
            let handle = thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut connections = Vec::<(TcpStream, Vec<u8>)>::new();
                let mut accepted = 0;
                let mut requests = Vec::new();

                while requests.len() < expected_requests {
                    loop {
                        match listener.accept() {
                            Ok((stream, _)) => {
                                stream.set_nonblocking(true).expect("nonblocking stream");
                                connections.push((stream, Vec::new()));
                                accepted += 1;
                            }
                            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(source) => panic!("accept connection: {source}"),
                        }
                    }

                    for (stream, buffer) in &mut connections {
                        let mut chunk = [0_u8; 1024];
                        loop {
                            match stream.read(&mut chunk) {
                                Ok(0) => break,
                                Ok(count) => buffer.extend_from_slice(&chunk[..count]),
                                Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                                    break;
                                }
                                Err(source) => panic!("read request: {source}"),
                            }
                        }

                        while let Some(header_end) =
                            buffer.windows(4).position(|window| window == b"\r\n\r\n")
                        {
                            let request = buffer.drain(..header_end + 4).collect::<Vec<_>>();
                            let request = String::from_utf8(request).expect("HTTP request");
                            requests.push(request.lines().next().expect("request line").to_owned());
                            stream
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                                .expect("response");
                        }
                    }

                    assert!(
                        Instant::now() < deadline,
                        "received {} of {expected_requests} requests",
                        requests.len()
                    );
                    thread::sleep(Duration::from_millis(1));
                }

                (accepted, requests)
            });
            Self {
                base,
                handle: Some(handle),
            }
        }

        fn finish(mut self) -> (usize, Vec<String>) {
            self.handle.take().expect("handle").join().expect("server")
        }
    }

    impl TestServer {
        fn spawn(responses: Vec<String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
            let address = listener.local_addr().expect("address");
            let base = format!("http://{address}");
            let response_count = responses.len();
            let handle = thread::spawn(move || {
                let mut requests = Vec::new();
                for response in responses {
                    let (mut stream, _) = listener.accept().expect("connection");
                    requests.push(read_request_line(&stream));
                    stream.write_all(response.as_bytes()).expect("response");
                }
                requests
            });
            Self {
                base,
                address,
                response_count,
                handle: Some(handle),
            }
        }

        fn finish(mut self) -> Vec<String> {
            wake_server(self.address, self.response_count);
            self.handle.take().expect("handle").join().expect("server")
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            let Some(handle) = self.handle.take() else {
                return;
            };
            wake_server(self.address, self.response_count);
            handle.join().expect("server");
        }
    }

    fn wake_server(address: SocketAddr, attempts: usize) {
        for _ in 0..attempts {
            let Ok(mut stream) = TcpStream::connect(address) else {
                break;
            };
            stream
                .write_all(b"HEAD /test-server-shutdown HTTP/1.1\r\n\r\n")
                .expect("shutdown request");
        }
    }

    fn read_request_line(stream: &TcpStream) -> String {
        BufReader::new(stream)
            .lines()
            .next()
            .expect("request line")
            .expect("request line contents")
    }

    fn response(status: &str, headers: &[(&str, &str)]) -> String {
        let mut output = format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: 0\r\n");
        for (name, value) in headers {
            output.push_str(name);
            output.push_str(": ");
            output.push_str(value);
            output.push_str("\r\n");
        }
        output.push_str("\r\n");
        output
    }

    #[test]
    fn follows_the_complete_redirect_chain() {
        let server = TestServer::spawn(vec![
            response("302 Found", &[("Location", "/discussion/42")]),
            response(
                "301 Moved Permanently",
                &[("Location", "/org/discussion/42")],
            ),
            response("200 OK", &[]),
        ]);
        let source = format!("{}/issues/42", server.base);

        let resolved = resolve_reference_url("#42", &source).expect("resolved URL");

        assert_eq!(resolved, format!("{}/org/discussion/42", server.base));
        assert_eq!(
            server.finish(),
            vec![
                "HEAD /issues/42 HTTP/1.1",
                "HEAD /discussion/42 HTTP/1.1",
                "HEAD /org/discussion/42 HTTP/1.1",
            ]
        );
    }

    #[test]
    fn falls_back_to_a_ranged_get_when_head_is_unsupported() {
        let server = TestServer::spawn(vec![
            response("405 Method Not Allowed", &[]),
            response("200 OK", &[]),
        ]);
        let source = format!("{}/issues/7", server.base);

        let resolved = resolve_reference_url("#7", &source).expect("resolved URL");

        assert_eq!(resolved, source);
        assert_eq!(
            server.finish(),
            vec!["HEAD /issues/7 HTTP/1.1", "GET /issues/7 HTTP/1.1"]
        );
    }

    #[test]
    fn resolver_reuses_a_connection_across_multiple_urls() {
        let server = ConnectionCountingServer::spawn(2);
        let resolver = ReferenceUrlResolver::new();

        resolver
            .resolve("#1", &format!("{}/issues/1", server.base))
            .expect("first URL");
        resolver
            .resolve("#2", &format!("{}/issues/2", server.base))
            .expect("second URL");

        assert_eq!(
            server.finish(),
            (
                1,
                vec![
                    String::from("HEAD /issues/1 HTTP/1.1"),
                    String::from("HEAD /issues/2 HTTP/1.1"),
                ],
            )
        );
    }

    #[test]
    fn rejects_unsuccessful_final_status() {
        let server = TestServer::spawn(vec![response("404 Not Found", &[])]);
        let source = format!("{}/issues/9", server.base);

        let error = resolve_reference_url("#9", &source).expect_err("404 must fail");

        assert!(matches!(
            error,
            Error::LinkResolutionFailed { ref label, ref reason, .. }
                if label == "#9" && reason.contains("404")
        ));
        server.finish();
    }

    #[test]
    fn rejects_redirect_without_location() {
        let server = TestServer::spawn(vec![response("302 Found", &[])]);
        let source = format!("{}/issues/10", server.base);

        let error = resolve_reference_url("#10", &source).expect_err("Location is required");

        assert!(matches!(
            error,
            Error::LinkResolutionFailed { ref label, ref reason, .. }
                if label == "#10" && reason.contains("no Location")
        ));
        assert_eq!(server.finish(), vec!["HEAD /issues/10 HTTP/1.1"]);
    }

    #[test]
    fn rejects_more_than_ten_redirects() {
        let server = TestServer::spawn(
            (0..=MAX_REDIRECTS)
                .map(|_| response("302 Found", &[("Location", "/loop")]))
                .collect(),
        );
        let source = format!("{}/loop", server.base);

        let error = resolve_reference_url("#10", &source).expect_err("redirect limit");

        assert!(matches!(
            error,
            Error::LinkResolutionFailed { ref label, ref reason, .. }
                if label == "#10" && reason.contains("more than 10 redirects")
        ));
        assert_eq!(server.finish().len(), MAX_REDIRECTS + 1);
    }

    #[test]
    fn rejects_credentials_before_making_a_request() {
        for source in [
            "http://user:secret@127.0.0.1:1/issues/10",
            "http://:secret@127.0.0.1:1/issues/10",
        ] {
            let error = resolve_reference_url("#10", source).expect_err("userinfo must fail");

            assert!(matches!(
                error,
                Error::LinkResolutionFailed { ref label, ref reason, .. }
                    if label == "#10" && reason.contains("userinfo")
            ));
            let Error::LinkResolutionFailed { url, .. } = &error else {
                unreachable!("checked above");
            };
            assert!(!url.contains("user:secret"));
            assert!(!url.contains(":secret@"));
            assert!(!error.to_string().contains("user:secret"));
            assert!(!error.to_string().contains(":secret@"));
        }
    }

    #[test]
    fn rejects_redirect_userinfo_before_requesting_the_target() {
        let target = TestServer::spawn(vec![response("200 OK", &[])]);
        let target_authority = target.base.strip_prefix("http://").expect("authority");
        let location = format!("http://user:secret@{target_authority}/private");
        let source = TestServer::spawn(vec![response(
            "302 Found",
            &[("Location", location.as_str())],
        )]);
        let source_url = format!("{}/issues/11", source.base);

        let error = resolve_reference_url("#11", &source_url)
            .expect_err("redirect userinfo must fail before the target request");

        assert!(matches!(
            error,
            Error::LinkResolutionFailed { ref label, ref reason, .. }
                if label == "#11" && reason.contains("userinfo")
        ));
        assert_eq!(source.finish(), vec!["HEAD /issues/11 HTTP/1.1"]);
        assert_eq!(target.finish(), vec!["HEAD /test-server-shutdown HTTP/1.1"]);
    }
}
