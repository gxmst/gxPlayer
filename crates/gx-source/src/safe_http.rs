use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use gx_contracts::NetworkRoute;
use reqwest::blocking::{Client, Response};
use reqwest::header::{
    AUTHORIZATION, COOKIE, HeaderMap, HeaderName, HeaderValue, LOCATION, PROXY_AUTHORIZATION,
};
use reqwest::{Method, StatusCode, Url};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::network_policy::{
    configure_async_client_builder, configure_async_client_builder_for_route,
    configure_client_builder, configure_client_builder_for_route,
};

const MAX_REDIRECTS: usize = 10;

#[derive(Debug, Clone)]
pub struct SafeHttpRequest {
    pub url: Url,
    pub method: Method,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout: Duration,
    pub max_response_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct SafeHttpResponse {
    pub final_url: Url,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
pub struct RequestCancellation(CancellationToken);

impl RequestCancellation {
    pub fn new() -> Self {
        Self(CancellationToken::new())
    }

    pub fn cancel(&self) {
        self.0.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.0.cancelled().await;
    }
}

#[derive(Debug, Error)]
pub enum SafeHttpError {
    #[error("only HTTP(S) URLs are allowed")]
    InvalidScheme,
    #[error("URL credentials are not allowed")]
    CredentialsDenied,
    #[error("URL has no host")]
    MissingHost,
    #[error("destination resolves to a loopback, link-local, or private address")]
    PrivateDestination,
    #[error("failed to resolve destination: {0}")]
    Dns(String),
    #[error("invalid request header: {0}")]
    InvalidHeader(String),
    #[error("HTTP request failed: {0}")]
    Request(String),
    #[error("HTTP request was cancelled")]
    Cancelled,
    #[error("redirect response has no valid Location header")]
    InvalidRedirect,
    #[error("redirect limit exceeded")]
    TooManyRedirects,
    #[error("HTTP {status} response exceeded {limit} bytes")]
    ResponseTooLarge { limit: usize, status: u16 },
}

pub fn execute(request: SafeHttpRequest) -> Result<SafeHttpResponse, SafeHttpError> {
    execute_with_route(request, None)
}

pub fn execute_on_route(
    request: SafeHttpRequest,
    route: NetworkRoute,
) -> Result<SafeHttpResponse, SafeHttpError> {
    execute_with_route(request, Some(route))
}

pub async fn execute_async(
    request: SafeHttpRequest,
    cancellation: &RequestCancellation,
) -> Result<SafeHttpResponse, SafeHttpError> {
    execute_with_route_async(request, None, cancellation).await
}

pub async fn execute_on_route_async(
    request: SafeHttpRequest,
    route: NetworkRoute,
    cancellation: &RequestCancellation,
) -> Result<SafeHttpResponse, SafeHttpError> {
    execute_with_route_async(request, Some(route), cancellation).await
}

fn execute_with_route(
    mut request: SafeHttpRequest,
    route: Option<NetworkRoute>,
) -> Result<SafeHttpResponse, SafeHttpError> {
    for redirect_count in 0..=MAX_REDIRECTS {
        let resolved = validate_and_resolve(&request.url)?;
        let host = request.url.host_str().ok_or(SafeHttpError::MissingHost)?;
        let builder = Client::builder().redirect(reqwest::redirect::Policy::none());
        let builder = match route {
            Some(route) => configure_client_builder_for_route(builder, route),
            None => configure_client_builder(builder),
        };
        let client = builder
            // Private destinations are still rejected before every request and redirect. When
            // the user enables an OS proxy, the proxy owns the final DNS resolution, so the
            // direct-mode DNS pin below cannot constrain the proxy-side connection target.
            .connect_timeout(request.timeout.min(Duration::from_secs(10)))
            .timeout(request.timeout)
            .resolve(host, resolved)
            .build()
            .map_err(|error| SafeHttpError::Request(error.to_string()))?;
        let mut builder = client.request(request.method.clone(), request.url.clone());
        for (name, value) in &request.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| SafeHttpError::InvalidHeader(error.to_string()))?;
            let value = HeaderValue::from_str(value)
                .map_err(|error| SafeHttpError::InvalidHeader(error.to_string()))?;
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }
        let response = builder
            .send()
            .map_err(|error| SafeHttpError::Request(error.to_string()))?;
        if response.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err(SafeHttpError::TooManyRedirects);
            }
            let next_url = redirect_target(&request.url, response.headers())?;
            if !same_origin(&request.url, &next_url) {
                strip_sensitive_headers(&mut request.headers);
            }
            request.url = next_url;
            if response.status() == StatusCode::SEE_OTHER
                || ((response.status() == StatusCode::MOVED_PERMANENTLY
                    || response.status() == StatusCode::FOUND)
                    && request.method == Method::POST)
            {
                request.method = Method::GET;
                request.body = None;
            }
            continue;
        }
        return read_response(response, request.max_response_bytes);
    }
    Err(SafeHttpError::TooManyRedirects)
}

async fn execute_with_route_async(
    mut request: SafeHttpRequest,
    route: Option<NetworkRoute>,
    cancellation: &RequestCancellation,
) -> Result<SafeHttpResponse, SafeHttpError> {
    for redirect_count in 0..=MAX_REDIRECTS {
        let resolved = validate_and_resolve_async(request.url.clone(), cancellation).await?;
        let host = request.url.host_str().ok_or(SafeHttpError::MissingHost)?;
        let builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
        let builder = match route {
            Some(route) => configure_async_client_builder_for_route(builder, route),
            None => configure_async_client_builder(builder),
        };
        let client = builder
            // This mirrors the blocking path: validation happens before every request and the
            // resolved address is pinned for direct connections. A system proxy still owns its
            // final target-side DNS resolution.
            .connect_timeout(request.timeout.min(Duration::from_secs(10)))
            .timeout(request.timeout)
            .resolve(host, resolved)
            .build()
            .map_err(|error| SafeHttpError::Request(error.to_string()))?;
        let mut builder = client.request(request.method.clone(), request.url.clone());
        for (name, value) in &request.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| SafeHttpError::InvalidHeader(error.to_string()))?;
            let value = HeaderValue::from_str(value)
                .map_err(|error| SafeHttpError::InvalidHeader(error.to_string()))?;
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }
        let response = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(SafeHttpError::Cancelled),
            result = builder.send() => {
                result.map_err(|error| SafeHttpError::Request(error.to_string()))?
            }
        };
        if response.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err(SafeHttpError::TooManyRedirects);
            }
            let next_url = redirect_target(&request.url, response.headers())?;
            if !same_origin(&request.url, &next_url) {
                strip_sensitive_headers(&mut request.headers);
            }
            request.url = next_url;
            if response.status() == StatusCode::SEE_OTHER
                || ((response.status() == StatusCode::MOVED_PERMANENTLY
                    || response.status() == StatusCode::FOUND)
                    && request.method == Method::POST)
            {
                request.method = Method::GET;
                request.body = None;
            }
            continue;
        }
        return read_response_async(response, request.max_response_bytes, cancellation).await;
    }
    Err(SafeHttpError::TooManyRedirects)
}

async fn validate_and_resolve_async(
    url: Url,
    cancellation: &RequestCancellation,
) -> Result<SocketAddr, SafeHttpError> {
    let resolution = tokio::task::spawn_blocking(move || validate_and_resolve(&url));
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(SafeHttpError::Cancelled),
        result = resolution => result
            .map_err(|error| SafeHttpError::Dns(format!("resolution task failed: {error}")))?,
    }
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str().map(str::to_ascii_lowercase)
            == right.host_str().map(str::to_ascii_lowercase)
        && left.port_or_known_default() == right.port_or_known_default()
}

fn strip_sensitive_headers(headers: &mut Vec<(String, String)>) {
    headers.retain(|(name, _)| {
        !name.eq_ignore_ascii_case(AUTHORIZATION.as_str())
            && !name.eq_ignore_ascii_case(PROXY_AUTHORIZATION.as_str())
            && !name.eq_ignore_ascii_case(COOKIE.as_str())
    });
}

pub fn validate_and_resolve(url: &Url) -> Result<SocketAddr, SafeHttpError> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(SafeHttpError::InvalidScheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(SafeHttpError::CredentialsDenied);
    }
    let host = url.host_str().ok_or(SafeHttpError::MissingHost)?;
    if host.eq_ignore_ascii_case("localhost") {
        return Err(SafeHttpError::PrivateDestination);
    }
    let port = url
        .port_or_known_default()
        .ok_or(SafeHttpError::MissingHost)?;
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| SafeHttpError::Dns(error.to_string()))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(SafeHttpError::Dns("no addresses returned".into()));
    }
    if addresses.iter().any(|address| is_private(address.ip())) {
        return Err(SafeHttpError::PrivateDestination);
    }
    Ok(addresses[0])
}

fn redirect_target(base: &Url, headers: &HeaderMap) -> Result<Url, SafeHttpError> {
    let location = headers
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(SafeHttpError::InvalidRedirect)?;
    base.join(location)
        .map_err(|_| SafeHttpError::InvalidRedirect)
}

async fn read_response_async(
    mut response: reqwest::Response,
    max_bytes: usize,
    cancellation: &RequestCancellation,
) -> Result<SafeHttpResponse, SafeHttpError> {
    let final_url = response.url().clone();
    let status = response.status().as_u16();
    let headers = flatten_headers(response.headers());
    let mut body = Vec::new();
    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(SafeHttpError::Cancelled),
            result = response.chunk() => {
                result.map_err(|error| SafeHttpError::Request(error.to_string()))?
            }
        };
        let Some(chunk) = chunk else {
            break;
        };
        if chunk.len() > max_bytes.saturating_sub(body.len()) {
            return Err(SafeHttpError::ResponseTooLarge {
                limit: max_bytes,
                status,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(SafeHttpResponse {
        final_url,
        status,
        headers,
        body,
    })
}

fn read_response(
    mut response: Response,
    max_bytes: usize,
) -> Result<SafeHttpResponse, SafeHttpError> {
    let final_url = response.url().clone();
    let status = response.status().as_u16();
    let headers = flatten_headers(response.headers());
    let mut body = Vec::new();
    response
        .by_ref()
        .take(max_bytes as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|error| SafeHttpError::Request(error.to_string()))?;
    if body.len() > max_bytes {
        return Err(SafeHttpError::ResponseTooLarge {
            limit: max_bytes,
            status,
        });
    }
    Ok(SafeHttpResponse {
        final_url,
        status,
        headers,
        body,
    })
}

fn flatten_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_owned(), value.to_owned()))
        })
        .collect()
}

fn is_private(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => private_ipv4(address),
        IpAddr::V6(address) => private_ipv6(address),
    }
}

fn private_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_unspecified()
        || address.is_documentation()
        || address.is_multicast()
        || octets[0] == 0
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 198 && (18..=19).contains(&octets[1]))
        || octets[0] >= 240
}

fn private_ipv6(address: Ipv6Addr) -> bool {
    // `Ipv6Addr::to_ipv4` covers both IPv4-compatible (`::127.0.0.1`) and
    // IPv4-mapped (`::ffff:127.0.0.1`) spellings. Checking the embedded IPv4 address is
    // essential: otherwise those literals bypass the IPv4 policy below DNS resolution.
    if let Some(address) = address.to_ipv4() {
        return private_ipv4(address);
    }
    let segments = address.segments();
    address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    #[test]
    fn rejects_private_and_credentialed_urls() {
        for url in [
            "http://127.0.0.1/private",
            "http://10.0.0.1/private",
            "http://169.254.1.1/private",
            "http://[::1]/private",
            "http://[::127.0.0.1]/private",
            "http://[::ffff:127.0.0.1]/private",
            "http://[::ffff:10.0.0.1]/private",
            "http://localhost/private",
        ] {
            assert!(matches!(
                validate_and_resolve(&Url::parse(url).unwrap()),
                Err(SafeHttpError::PrivateDestination)
            ));
        }
        assert!(matches!(
            validate_and_resolve(&Url::parse("https://user:pass@example.com/").unwrap()),
            Err(SafeHttpError::CredentialsDenied)
        ));
    }

    #[test]
    fn cross_origin_redirects_drop_credentials() {
        assert!(same_origin(
            &Url::parse("https://example.com/a").unwrap(),
            &Url::parse("https://EXAMPLE.com/b").unwrap()
        ));
        assert!(!same_origin(
            &Url::parse("https://example.com/a").unwrap(),
            &Url::parse("http://example.com/b").unwrap()
        ));
        let mut headers = vec![
            ("Authorization".into(), "secret".into()),
            ("cookie".into(), "secret".into()),
            ("Referer".into(), "https://example.com".into()),
        ];
        strip_sensitive_headers(&mut headers);
        assert_eq!(
            headers,
            vec![("Referer".into(), "https://example.com".into())]
        );
    }

    #[test]
    fn bounded_reader_rejects_oversized_responses() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n12345678",
                )
                .unwrap();
        });
        let response = Client::builder()
            .build()
            .unwrap()
            .get(format!("http://{address}/"))
            .send()
            .unwrap();
        assert!(matches!(
            read_response(response, 4),
            Err(SafeHttpError::ResponseTooLarge {
                limit: 4,
                status: 200
            })
        ));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn async_execution_rejects_private_destinations() {
        let cancellation = RequestCancellation::new();
        let result = execute_async(
            SafeHttpRequest {
                url: Url::parse("http://127.0.0.1/private").unwrap(),
                method: Method::GET,
                headers: Vec::new(),
                body: None,
                timeout: Duration::from_secs(1),
                max_response_bytes: 1024,
            },
            &cancellation,
        )
        .await;

        assert!(matches!(result, Err(SafeHttpError::PrivateDestination)));
    }

    #[tokio::test]
    async fn async_reader_cancels_a_stalled_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n")
                .unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
        });
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        let cancellation = RequestCancellation::new();
        let cancellation_for_task = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            cancellation_for_task.cancel();
        });

        let result = read_response_async(response, 16, &cancellation).await;

        assert!(matches!(result, Err(SafeHttpError::Cancelled)));
        let _ = release_tx.send(());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn async_reader_rejects_oversized_responses_before_extending() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n12345678",
                )
                .unwrap();
        });
        let response = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        let cancellation = RequestCancellation::new();

        let result = read_response_async(response, 4, &cancellation).await;

        assert!(matches!(
            result,
            Err(SafeHttpError::ResponseTooLarge {
                limit: 4,
                status: 200
            })
        ));
        server.join().unwrap();
    }
}
