use axum::{extract::State, http::{HeaderMap, HeaderValue, Method, StatusCode}, response::{IntoResponse, Response}, routing::get, Router};
use reqwest::Client;
use serde::Deserialize;
use std::{env, net::{IpAddr, SocketAddr}, sync::Arc, time::Duration};
use tokio::{net::lookup_host, sync::Semaphore, time::timeout};
use tracing::{debug, error, info};
use url::Url;

#[derive(Clone)]
struct AppState {
    client: Client,
    semaphore: Arc<Semaphore>,
    cfg: Config,
}

#[derive(Clone)]
struct Config {
    proxy_path: String,
    upstream_timeout: Duration,
    stall_timeout: Duration,
    max_playlist_bytes: usize,
    max_url_length: usize,
    max_header_json_length: usize,
    max_concurrent: usize,
    user_agent: String,
    allow_private_ips: bool,
}

#[derive(Debug, Deserialize)]
struct HeaderInput(serde_json::Value);

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter(
        env::var("RUST_LOG").unwrap_or_else(|_| "info".into())
    ).init();

    let cfg = Config::from_env();
    let client = Client::builder()
        .user_agent(cfg.user_agent.clone())
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(30))
        .http2_adaptive_window(true)
        .build()
        .expect("failed to build HTTP client");

    let state = AppState {
        semaphore: Arc::new(Semaphore::new(cfg.max_concurrent)),
        client,
        cfg: cfg.clone(),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route(&cfg.proxy_path, get(proxy).head(proxy).options(options))
        .with_state(state);

    let addr: SocketAddr = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into())
        .parse().expect("invalid LISTEN_ADDR");
    let listener = tokio::net::TcpListener::bind(addr).await.expect("failed to bind");
    info!(%addr, path = %cfg.proxy_path, "CORS proxy listening");
    if let Err(err) = axum::serve(listener, app).with_graceful_shutdown(shutdown()).await {
        error!(%err, "server stopped");
    }
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutdown signal received");
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    (StatusCode::OK, [("content-type", "application/json")], format!(r#"{{"status":"ok","available_upstreams":{}}}"#, state.semaphore.available_permits()))
}

async fn options() -> impl IntoResponse {
    let mut headers = cors_headers();
    headers.insert("access-control-expose-headers", HeaderValue::from_static("Content-Length, Content-Range, Accept-Ranges, ETag"));
    (StatusCode::NO_CONTENT, headers)
}

async fn proxy(State(state): State<AppState>, method: Method, headers: HeaderMap, query: axum::extract::Query<std::collections::HashMap<String, String>>) -> Response {
    let raw = match query.get("url") {
        Some(v) if !v.is_empty() && v.len() <= state.cfg.max_url_length => v,
        _ => return error_response(StatusCode::BAD_REQUEST, "invalid or missing `url` query parameter"),
    };
    let upstream = match Url::parse(raw) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host_str().is_some() && u.username().is_empty() => u,
        _ => return error_response(StatusCode::BAD_REQUEST, "url must be an absolute http or https URL"),
    };
    if !state.cfg.allow_private_ips {
        if let Err(msg) = validate_upstream(&upstream).await { return error_response(StatusCode::FORBIDDEN, &msg); }
    }

    let mut upstream_headers = match parse_headers(query.get("headers"), &state.cfg) {
        Ok(h) => h,
        Err(msg) => return error_response(StatusCode::BAD_REQUEST, &msg),
    };
    // These are HTTP headers to the upstream, never URL query parameters.
    for name in ["referer", "origin"] {
        if let Some(value) = query.get(name) {
            if value.len() <= 4096 {
                if let Ok(v) = HeaderValue::from_str(value) { upstream_headers.insert(name, v); }
            }
        }
    }
    if let Some(range) = headers.get("range") { upstream_headers.insert("range", range.clone()); }

    let _permit = match state.semaphore.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => return error_response(StatusCode::SERVICE_UNAVAILABLE, "proxy concurrency limit reached"),
    };
    let request = state.client.request(method.clone(), upstream.clone()).headers(upstream_headers);
    let response = match timeout(state.cfg.upstream_timeout, request.send()).await {
        Ok(Ok(r)) => r,
        Ok(Err(err)) => return error_response(StatusCode::BAD_GATEWAY, &format!("upstream request failed: {err}")),
        Err(_) => return error_response(StatusCode::GATEWAY_TIMEOUT, "upstream request timed out"),
    };

    if is_playlist(&upstream) && response.status().is_success() {
        return playlist_response(response, upstream, &state, &query, method == Method::HEAD).await;
    }
    stream_response(response, method == Method::HEAD, query.get("filename").map(String::as_str), state.cfg.stall_timeout).await
}

async fn playlist_response(response: reqwest::Response, base: Url, state: &AppState, query: &std::collections::HashMap<String, String>, head: bool) -> Response {
    let body = match response.bytes().await {
        Ok(b) if b.len() <= state.cfg.max_playlist_bytes => b,
        Ok(_) => return error_response(StatusCode::BAD_GATEWAY, "upstream playlist exceeds configured size limit"),
        Err(err) => return error_response(StatusCode::BAD_GATEWAY, &format!("failed reading upstream playlist: {err}")),
    };
    let text = String::from_utf8_lossy(&body);
    let rewritten = match rewrite_playlist(&text, &base, &state.cfg.proxy_path, query) {
        Ok(v) => v,
        Err(err) => return error_response(StatusCode::BAD_GATEWAY, &err),
    };
    let mut headers = cors_headers();
    headers.insert("content-type", HeaderValue::from_static("application/vnd.apple.mpegurl"));
    headers.insert("cache-control", HeaderValue::from_static("no-cache, no-store, must-revalidate"));
    headers.insert("pragma", HeaderValue::from_static("no-cache"));
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    if let Some(name) = query.get("filename").and_then(sanitize_filename) {
        if let Ok(v) = HeaderValue::from_str(&format!("inline; filename=\"{name}\"")) { headers.insert("content-disposition", v); }
    }
    headers.insert("content-length", HeaderValue::from_str(&rewritten.len().to_string()).unwrap());
    if head { return (StatusCode::OK, headers).into_response(); }
    (StatusCode::OK, headers, rewritten).into_response()
}

async fn stream_response(response: reqwest::Response, head: bool, filename: Option<&str>, stall: Duration) -> Response {
    let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = cors_headers();
    for name in ["content-type", "content-length", "content-range", "accept-ranges", "cache-control", "etag", "expires", "last-modified"] {
        if let Some(v) = response.headers().get(name) { headers.insert(name, v.clone()); }
    }
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    if let Some(name) = filename.and_then(sanitize_filename) {
        if let Ok(v) = HeaderValue::from_str(&format!("inline; filename=\"{name}\"")) { headers.insert("content-disposition", v); }
    }
    if head { return (status, headers).into_response(); }
    let stream = response.bytes_stream();
    let body = axum::body::Body::from_stream(stall_stream(stream, stall));
    (status, headers, body).into_response()
}

async fn stall_stream<S, E>(mut stream: S, stall: Duration) -> impl futures_core::Stream<Item = Result<bytes::Bytes, E>>
where S: futures_util::Stream<Item = Result<bytes::Bytes, E>> + Unpin {
    futures_util::stream::poll_fn(move |cx| {
        std::pin::Pin::new(&mut stream).poll_next(cx)
    })
}

fn rewrite_playlist(text: &str, base: &Url, proxy_path: &str, query: &std::collections::HashMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(text.len() + 256);
    let mut header_query = String::new();
    for key in ["referer", "origin", "headers"] {
        if let Some(v) = query.get(key) {
            if key != "headers" || v.len() <= 8192 { header_query.push('&'); header_query.push_str(key); header_query.push('='); header_query.push_str(&percent_encode(v)); }
        }
    }
    for line in text.replace("\r\n", "\n").lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            out.push_str(&rewrite_uri_attribute(line, base, proxy_path, &header_query)?);
        } else if trimmed.is_empty() {
            out.push_str(line);
        } else {
            let target = resolve_url(base, trimmed)?;
            out.push_str(proxy_path); out.push_str("?url="); out.push_str(&percent_encode(target.as_str())); out.push_str(&header_query);
        }
        out.push('\n');
    }
    Ok(out)
}

fn rewrite_uri_attribute(line: &str, base: &Url, proxy_path: &str, suffix: &str) -> Result<String, String> {
    let Some(start) = line.find("URI=\"") else { return Ok(line.to_string()); };
    let value_start = start + 5;
    let rest = &line[value_start..];
    let Some(end_rel) = rest.find('"') else { return Ok(line.to_string()); };
    let raw = &rest[..end_rel];
    let target = resolve_url(base, raw)?;
    Ok(format!("{}{}?url={}{}{}", &line[..value_start], proxy_path, percent_encode(target.as_str()), suffix, &rest[end_rel + 1..]))
}

fn resolve_url(base: &Url, raw: &str) -> Result<Url, String> {
    let parsed = Url::parse(raw).or_else(|_| base.join(raw)).map_err(|e| format!("invalid playlist resource URL: {e}"))?;
    if !matches!(parsed.scheme(), "http" | "https") { return Err(format!("unsupported resource URL scheme: {}", parsed.scheme())); }
    Ok(parsed)
}

fn parse_headers(raw: Option<&String>, cfg: &Config) -> Result<HeaderMap, String> {
    let mut out = HeaderMap::new();
    out.insert("user-agent", HeaderValue::from_str(&cfg.user_agent).map_err(|_| "invalid USER_AGENT".to_string())?);
    let Some(raw) = raw else { return Ok(out); };
    if raw.len() > cfg.max_header_json_length { return Err("headers parameter is too large".into()); }
    let decoded = percent_decode(raw)?;
    let value: serde_json::Value = serde_json::from_str(&decoded).map_err(|_| "headers must be valid JSON")?;
    let obj = value.as_object().ok_or("headers must be a JSON object")?;
    for (name, value) in obj {
        let lower = name.to_ascii_lowercase();
        if ["host", "connection", "content-length", "transfer-encoding", "access-control-allow-origin", "access-control-allow-methods", "access-control-allow-headers"].contains(&lower.as_str()) { continue; }
        let Some(value) = value.as_str() else { continue; };
        if name.len() > 128 || value.len() > 4096 { continue; }
        if let (Ok(n), Ok(v)) = (http::header::HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(value)) { out.insert(n, v); }
    }
    Ok(out)
}

async fn validate_upstream(url: &Url) -> Result<(), String> {
    let host = url.host_str().ok_or("upstream host is missing")?;
    if let Ok(ip) = host.parse::<IpAddr>() { return if private_ip(ip) { Err("private or special-use upstream addresses are blocked".into()) } else { Ok(()) }; }
    let port = url.port_or_known_default().ok_or("unsupported upstream port")?;
    let addrs = lookup_host((host, port)).await.map_err(|_| "unable to resolve upstream host")?;
    let mut found = false;
    for addr in addrs {
        found = true;
        if private_ip(addr.ip()) { return Err("upstream host resolves to a private or special-use address".into()); }
    }
    if !found { return Err("upstream host has no addresses".into()); }
    Ok(())
}

fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local() || v.is_unspecified() || v.is_multicast() || v.octets()[0] == 0 || (v.octets()[0] == 100 && (64..=127).contains(&v.octets()[1])),
        IpAddr::V6(v) => v.is_loopback() || v.is_unspecified() || v.is_multicast() || v.is_unique_local() || ((v.segments()[0] & 0xffc0) == 0xfe80),
    }
}

fn is_playlist(url: &Url) -> bool { let p = url.path().to_ascii_lowercase(); p.ends_with(".m3u8") || p.ends_with(".txt") }
fn sanitize_filename(v: &str) -> Option<String> { let name = v.rsplit('/').next()?.trim(); if name.is_empty() || name.len() > 180 { return None; } Some(name.chars().map(|c| if c.is_control() || r#"/\\?%*:|\"<>"#.contains(c) { '_' } else { c }).collect()) }
fn percent_encode(v: &str) -> String { url::form_urlencoded::byte_serialize(v.as_bytes()).collect() }
fn percent_decode(v: &str) -> Result<String, String> { percent_encoding::percent_decode_str(v).decode_utf8().map(|s| s.into_owned()).map_err(|_| "invalid percent encoding".into()) }
fn cors_headers() -> HeaderMap { let mut h = HeaderMap::new(); h.insert("access-control-allow-origin", HeaderValue::from_static("*")); h.insert("access-control-allow-methods", HeaderValue::from_static("GET, HEAD, OPTIONS")); h.insert("access-control-allow-headers", HeaderValue::from_static("Range, Content-Type, *")); h.insert("access-control-expose-headers", HeaderValue::from_static("Content-Length, Content-Range, Accept-Ranges, ETag")); h }
fn error_response(status: StatusCode, message: &str) -> Response { let body = serde_json::json!({"message": message}).to_string(); let mut h = cors_headers(); h.insert("content-type", HeaderValue::from_static("application/json")); (status, h, body).into_response() }

impl Config {
    fn from_env() -> Self {
        Self {
            proxy_path: env::var("PROXY_PATH").unwrap_or_else(|_| "/m3u8-proxy".into()),
            upstream_timeout: duration_env("UPSTREAM_TIMEOUT", 10),
            stall_timeout: duration_env("STALL_TIMEOUT", 15),
            max_playlist_bytes: usize_env("MAX_PLAYLIST_BYTES", 4 << 20),
            max_url_length: usize_env("MAX_URL_LENGTH", 8192),
            max_header_json_length: usize_env("MAX_HEADER_JSON_LENGTH", 8192),
            max_concurrent: usize_env("MAX_CONCURRENT_UPSTREAM", 256).max(1),
            user_agent: env::var("USER_AGENT").unwrap_or_else(|_| "cors-proxy-rust/1.0".into()),
            allow_private_ips: bool_env("ALLOW_PRIVATE_IPS", false),
        }
    }
}
fn usize_env(k: &str, d: usize) -> usize { env::var(k).ok().and_then(|v| v.parse().ok()).filter(|v| *v > 0).unwrap_or(d) }
fn duration_env(k: &str, secs: u64) -> Duration { env::var(k).ok().and_then(|v| v.parse().ok()).map(Duration::from_secs).filter(|v| !v.is_zero()).unwrap_or(Duration::from_secs(secs)) }
fn bool_env(k: &str, d: bool) -> bool { env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d) }
