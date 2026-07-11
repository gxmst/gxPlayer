use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, LOCATION};
use reqwest::{Method, StatusCode, Url};
use thiserror::Error;

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
    #[error("redirect response has no valid Location header")]
    InvalidRedirect,
    #[error("redirect limit exceeded")]
    TooManyRedirects,
    #[error("response exceeded {0} bytes")]
    ResponseTooLarge(usize),
}

pub fn execute(mut request: SafeHttpRequest) -> Result<SafeHttpResponse, SafeHttpError> {
    for redirect_count in 0..=MAX_REDIRECTS {
        let resolved = validate_and_resolve(&request.url)?;
        let host = request.url.host_str().ok_or(SafeHttpError::MissingHost)?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
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
            request.url = redirect_target(&request.url, &response)?;
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

fn redirect_target(base: &Url, response: &Response) -> Result<Url, SafeHttpError> {
    let location = response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(SafeHttpError::InvalidRedirect)?;
    base.join(location)
        .map_err(|_| SafeHttpError::InvalidRedirect)
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
        return Err(SafeHttpError::ResponseTooLarge(max_bytes));
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
    address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_unspecified()
        || address.is_documentation()
}

fn private_ipv6(address: Ipv6Addr) -> bool {
    address.is_loopback()
        || address.is_unspecified()
        || (address.segments()[0] & 0xfe00) == 0xfc00
        || (address.segments()[0] & 0xffc0) == 0xfe80
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_private_and_credentialed_urls() {
        for url in [
            "http://127.0.0.1/private",
            "http://10.0.0.1/private",
            "http://169.254.1.1/private",
            "http://[::1]/private",
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
}
