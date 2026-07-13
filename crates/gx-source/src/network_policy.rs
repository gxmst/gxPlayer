use std::ffi::OsString;
use std::sync::atomic::{AtomicU8, Ordering};

use gx_contracts::NetworkRoute;
use reqwest::blocking::ClientBuilder;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    #[default]
    Auto = 0,
    On = 1,
    Off = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyStatus {
    pub mode: ProxyMode,
    pub detected: bool,
    pub effective: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyRoute {
    System,
    Direct,
}

static PROXY_MODE: AtomicU8 = AtomicU8::new(ProxyMode::Auto as u8);

pub fn set_mode(mode: ProxyMode) {
    PROXY_MODE.store(mode as u8, Ordering::Release);
}

pub fn mode() -> ProxyMode {
    match PROXY_MODE.load(Ordering::Acquire) {
        value if value == ProxyMode::On as u8 => ProxyMode::On,
        value if value == ProxyMode::Off as u8 => ProxyMode::Off,
        _ => ProxyMode::Auto,
    }
}

pub fn status() -> ProxyStatus {
    status_for(mode(), system_proxy_detected())
}

pub fn configure_client_builder(builder: ClientBuilder) -> ClientBuilder {
    match route_for(mode(), system_proxy_detected()) {
        ProxyRoute::System => builder,
        ProxyRoute::Direct => builder.no_proxy(),
    }
}

pub fn configure_client_builder_for_route(
    builder: ClientBuilder,
    route: NetworkRoute,
) -> ClientBuilder {
    match route {
        NetworkRoute::Direct => builder.no_proxy(),
        NetworkRoute::SystemProxy => builder,
    }
}

pub fn source_route_attempts(preferred: Option<NetworkRoute>) -> Vec<NetworkRoute> {
    source_route_attempts_for(mode(), system_proxy_detected(), preferred)
}

pub fn system_proxy_detected() -> bool {
    if std::env::var_os("REQUEST_METHOD").is_some() {
        // Match reqwest's CGI protection: its system proxy matcher is disabled in this case.
        return false;
    }
    environment_proxy_detected(|name| std::env::var_os(name)) || platform_proxy_detected()
}

fn status_for(mode: ProxyMode, detected: bool) -> ProxyStatus {
    ProxyStatus {
        mode,
        detected,
        effective: route_for(mode, detected) == ProxyRoute::System && detected,
    }
}

fn route_for(mode: ProxyMode, detected: bool) -> ProxyRoute {
    match mode {
        ProxyMode::Off => ProxyRoute::Direct,
        ProxyMode::On => ProxyRoute::System,
        ProxyMode::Auto if detected => ProxyRoute::System,
        ProxyMode::Auto => ProxyRoute::Direct,
    }
}

fn source_route_attempts_for(
    mode: ProxyMode,
    detected: bool,
    preferred: Option<NetworkRoute>,
) -> Vec<NetworkRoute> {
    if mode == ProxyMode::Off || !detected {
        return vec![NetworkRoute::Direct];
    }
    match preferred {
        Some(NetworkRoute::SystemProxy) => {
            vec![NetworkRoute::SystemProxy, NetworkRoute::Direct]
        }
        Some(NetworkRoute::Direct) | None => {
            vec![NetworkRoute::Direct, NetworkRoute::SystemProxy]
        }
    }
}

fn environment_proxy_detected(mut get: impl FnMut(&str) -> Option<OsString>) -> bool {
    [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .into_iter()
    .filter_map(&mut get)
    .any(|value| !value.to_string_lossy().trim().is_empty())
}

#[cfg(windows)]
fn platform_proxy_detected() -> bool {
    let Ok(settings) = windows_registry::CURRENT_USER
        .open("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
    else {
        return false;
    };
    settings.get_u32("ProxyEnable").unwrap_or(0) != 0
        && settings
            .get_string("ProxyServer")
            .is_ok_and(|value| !value.trim().is_empty())
}

#[cfg(not(windows))]
fn platform_proxy_detected() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Duration;

    use super::*;

    struct RestoreMode(ProxyMode);

    impl Drop for RestoreMode {
        fn drop(&mut self) {
            set_mode(self.0);
        }
    }

    fn serve_once(listener: TcpListener, body: &'static str) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        })
    }

    #[test]
    fn environment_detection_accepts_supported_non_empty_values() {
        let values = HashMap::from([
            ("HTTPS_PROXY", OsString::from("  ")),
            ("http_proxy", OsString::from("proxy.invalid:8080")),
        ]);
        assert!(environment_proxy_detected(|name| values.get(name).cloned()));
        assert!(!environment_proxy_detected(|_| None));
    }

    #[test]
    fn mode_and_detection_select_expected_route() {
        assert_eq!(route_for(ProxyMode::Auto, true), ProxyRoute::System);
        assert_eq!(route_for(ProxyMode::Auto, false), ProxyRoute::Direct);
        assert_eq!(route_for(ProxyMode::On, false), ProxyRoute::System);
        assert_eq!(route_for(ProxyMode::Off, true), ProxyRoute::Direct);
    }

    #[test]
    fn status_never_claims_an_absent_proxy_is_effective() {
        assert!(status_for(ProxyMode::Auto, true).effective);
        assert!(status_for(ProxyMode::On, true).effective);
        assert!(!status_for(ProxyMode::On, false).effective);
        assert!(!status_for(ProxyMode::Off, true).effective);
    }

    #[test]
    fn source_routes_are_direct_first_unless_proxy_succeeded_last() {
        assert_eq!(
            source_route_attempts_for(ProxyMode::Off, true, Some(NetworkRoute::SystemProxy)),
            [NetworkRoute::Direct]
        );
        assert_eq!(
            source_route_attempts_for(ProxyMode::Auto, false, Some(NetworkRoute::SystemProxy)),
            [NetworkRoute::Direct]
        );
        assert_eq!(
            source_route_attempts_for(ProxyMode::On, true, None),
            [NetworkRoute::Direct, NetworkRoute::SystemProxy]
        );
        assert_eq!(
            source_route_attempts_for(ProxyMode::Auto, true, Some(NetworkRoute::Direct)),
            [NetworkRoute::Direct, NetworkRoute::SystemProxy]
        );
        assert_eq!(
            source_route_attempts_for(ProxyMode::Auto, true, Some(NetworkRoute::SystemProxy)),
            [NetworkRoute::SystemProxy, NetworkRoute::Direct]
        );
    }

    #[test]
    fn explicit_route_configuration_uses_requested_route() {
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_address = proxy.local_addr().unwrap();
        let proxy_server = serve_once(proxy, "proxy");
        let unused_target = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_url = format!("http://{}/route", unused_target.local_addr().unwrap());

        let mut response = configure_client_builder_for_route(
            reqwest::blocking::Client::builder()
                .proxy(reqwest::Proxy::all(format!("http://{proxy_address}")).unwrap()),
            NetworkRoute::SystemProxy,
        )
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
        .get(&target_url)
        .send()
        .unwrap();
        let mut body = String::new();
        response.read_to_string(&mut body).unwrap();
        assert_eq!(body, "proxy");
        proxy_server.join().unwrap();
        drop(unused_target);

        let direct = TcpListener::bind("127.0.0.1:0").unwrap();
        let direct_url = format!("http://{}/route", direct.local_addr().unwrap());
        let direct_server = serve_once(direct, "direct");
        let mut response = configure_client_builder_for_route(
            reqwest::blocking::Client::builder()
                .proxy(reqwest::Proxy::all(format!("http://{proxy_address}")).unwrap()),
            NetworkRoute::Direct,
        )
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
        .get(direct_url)
        .send()
        .unwrap();
        let mut body = String::new();
        response.read_to_string(&mut body).unwrap();
        assert_eq!(body, "direct");
        direct_server.join().unwrap();
    }

    #[test]
    fn configured_clients_use_or_clear_the_selected_proxy_route() {
        let _restore = RestoreMode(mode());
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_address = proxy.local_addr().unwrap();
        let proxy_server = serve_once(proxy, "proxy");
        let unused_target = TcpListener::bind("127.0.0.1:0").unwrap();
        let target_url = format!("http://{}/route", unused_target.local_addr().unwrap());

        set_mode(ProxyMode::On);
        let mut response = configure_client_builder(
            reqwest::blocking::Client::builder()
                .proxy(reqwest::Proxy::all(format!("http://{proxy_address}")).unwrap()),
        )
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
        .get(&target_url)
        .send()
        .unwrap();
        let mut body = String::new();
        response.read_to_string(&mut body).unwrap();
        assert_eq!(body, "proxy");
        proxy_server.join().unwrap();
        drop(unused_target);

        let direct = TcpListener::bind("127.0.0.1:0").unwrap();
        let direct_url = format!("http://{}/route", direct.local_addr().unwrap());
        let direct_server = serve_once(direct, "direct");
        set_mode(ProxyMode::Off);
        let mut response = configure_client_builder(
            reqwest::blocking::Client::builder()
                .proxy(reqwest::Proxy::all(format!("http://{proxy_address}")).unwrap()),
        )
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap()
        .get(direct_url)
        .send()
        .unwrap();
        let mut body = String::new();
        response.read_to_string(&mut body).unwrap();
        assert_eq!(body, "direct");
        direct_server.join().unwrap();
    }
}
