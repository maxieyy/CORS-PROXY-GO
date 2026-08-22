use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use reqwest::Client;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    env,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::{net::lookup_host, sync::Semaphore, time::timeout};
use tracing::{error, info};
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
    max_playlist_bytes: usize,
    max_url_length: usize,
    max_header_json_length: usize,
    max_concurrent: usize,
    user_agent: String,
    allow_private_ips: bool,
}

// These are safe browser/media request headers that are useful to preserve
// when the client talks to a protected HLS origin. We deliberately use an
// allowlist rather than blindly forwarding every incoming request header.
const SAFE_FORWARD_HEADERS: &[&str] = &[
    "accept",
    "accept-language",
    "cache-control",
    "pragma",
    "user-agent",
    "sec-fetch-dest",
    "sec-fetch-mode",
    "sec-fetch-site",
    "sec-gpc",
    "sec-ch-ua",
    "sec-ch-ua-mobile",
    "sec-ch-ua-platform",
    "if-none-match",
    "if-modified-since",
    "if-range",
    "dnt",
];

// Headers safe to carry into rewritten HLS URLs. Conditional headers such as
// If-None-Match are intentionally excluded because they describe the manifest
// request and should not be copied to every media segment.
const SAFE_PROPAGATED_HEADERS: &[&str] = &[
    "accept",
    "accept-language",
    "cache-control",
    "pragma",
    "user-agent",
    "sec-fetch-dest",
    "sec-fetch-mode",
    "sec-fetch-site",
    "sec-gpc",
    "sec-ch-ua",
    "sec-ch-ua-mobile",
    "sec-ch-ua-platform",
    "dnt",
];

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(env::var("RUST_LOG").unwrap_or_else(|_| "info".into()))
        .init();

    let cfg = Config::from_env();
    let client = Client::builder()
        .user_agent(cfg.user_agent.clone())
        .connect_timeout(Duration::from_secs(10))
        .pool_idle_timeout(Duration::from_secs(30))
        .http2_adaptive_window(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("failed to build HTTP client");

    let state = AppState {
        client,
        semaphore: Arc::new(Semaphore::new(cfg.max_concurrent)),
        cfg: cfg.clone(),
    };

    let app = Router::new()
        .route("/health", get(health))
        .route(&cfg.proxy_path, get(proxy).head(proxy).options(options))
        .with_state(state);

    let addr: SocketAddr = env::var("LISTEN_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:3000".into())
        .parse()
        .expect("invalid LISTEN_ADDR");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind");

    info!(%addr, path = %cfg.proxy_path, "Rust CORS proxy listening");

    if let Err(err) = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
    {
        error!(%err, "server stopped");
    }
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    info!("shutdown signal received");
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        json!({
            "status": "ok",
            "available_upstreams": state.semaphore.available_permits()
        })
        .to_string(),
    )
}

async fn options() -> impl IntoResponse {
    (StatusCode::NO_CONTENT, cors_headers())
}

async fn proxy(
    State(state): State<AppState>,
    method: Method,
    request_headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let raw = match query.get("url") {
        Some(v) if !v.is_empty() && v.len() <= state.cfg.max_url_length => v,
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid or missing `url` query parameter",
            )
        }
    };

    let upstream = match Url::parse(raw) {
        Ok(u)
            if matches!(u.scheme(), "http" | "https")
                && u.host_str().is_some()
                && u.username().is_empty() =>
        {
            u
        }
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "url must be an absolute http or https URL",
            )
        }
    };

    if !state.cfg.allow_private_ips {
        if let Err(msg) = validate_upstream(&upstream).await {
            return error_response(StatusCode::FORBIDDEN, &msg);
        }
    }

    let upstream_headers = match build_upstream_headers(&query, &request_headers, &state.cfg) {
        Ok(h) => h,
        Err(msg) => return error_response(StatusCode::BAD_REQUEST, &msg),
    };

    let _permit = match state.semaphore.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "proxy concurrency limit reached",
            )
        }
    };

    let response = match timeout(
        state.cfg.upstream_timeout,
        state
            .client
            .request(method.clone(), upstream.clone())
            .headers(upstream_headers.clone())
            .send(),
    )
    .await
    {
        Ok(Ok(r)) => r,
        Ok(Err(err)) => {
            error!(%err, %upstream, "upstream request failed");
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream request failed: {err}"),
            );
        }
        Err(_) => {
            error!(%upstream, "upstream request timed out");
            return error_response(StatusCode::GATEWAY_TIMEOUT, "upstream request timed out");
        }
    };

    info!(
        upstream = %upstream,
        status = %response.status(),
        server = ?response.headers().get("server"),
        content_type = ?response.headers().get("content-type"),
        content_length = ?response.headers().get("content-length"),
        location = ?response.headers().get("location"),
        "upstream response"
    );

    if is_playlist(&upstream) && response.status().is_success() {
        return playlist_response(
            response,
            upstream,
            &state,
            &query,
            &upstream_headers,
            method == Method::HEAD,
        )
        .await;
    }

    stream_response(
        response,
        method == Method::HEAD,
        query.get("filename").map(String::as_str),
    )
    .await
}

fn build_upstream_headers(
    query: &HashMap<String, String>,
    request_headers: &HeaderMap,
    cfg: &Config,
) -> Result<HeaderMap, String> {
    let mut out = HeaderMap::new();

    // Preserve the useful browser/media request headers from the original
    // client request. Only this explicit allowlist is copied automatically.
    for name in SAFE_FORWARD_HEADERS {
        if let Some(value) = request_headers.get(*name) {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| format!("invalid forwarded header name: {name}"))?;
            out.insert(header_name, value.clone());
        }
    }

    // Fall back to the configured UA only when the client did not send one.
    if !out.contains_key("user-agent") {
        out.insert(
            "user-agent",
            HeaderValue::from_str(&cfg.user_agent).map_err(|_| "invalid USER_AGENT")?,
        );
    }

    // Explicit JSON headers remain supported and override automatically
    // copied safe headers where the same name is supplied.
    if let Some(raw) = query.get("headers") {
        if raw.len() > cfg.max_header_json_length {
            return Err("headers parameter is too large".into());
        }

        let value: Value =
            serde_json::from_str(raw).map_err(|_| "headers must be valid JSON")?;
        let object = value
            .as_object()
            .ok_or("headers must be a JSON object")?;

        for (name, value) in object {
            set_safe_header(&mut out, name, value);
        }
    }

    // Explicit query parameters are authoritative for Origin and Referer.
    for name in ["referer", "origin"] {
        if let Some(value) = query.get(name) {
            if value.len() <= 4096 {
                set_safe_header(&mut out, name, &Value::String(value.clone()));
            }
        }
    }

    // Range belongs to the current media request and must not be persisted in
    // rewritten playlist URLs.
    if let Some(range) = request_headers.get("range") {
        out.insert("range", range.clone());
    }

    Ok(out)
}

fn set_safe_header(out: &mut HeaderMap, name: &str, value: &Value) {
    let lower = name.to_ascii_lowercase();
    let blocked = [
        "host",
        "connection",
        "content-length",
        "transfer-encoding",
        "upgrade",
        "proxy-authorization",
        "proxy-authenticate",
        "proxy-connection",
        "te",
        "trailer",
        "expect",
        "access-control-allow-origin",
        "access-control-allow-methods",
        "access-control-allow-headers",
        "access-control-expose-headers",
    ];

    if blocked.contains(&lower.as_str()) || name.len() > 128 {
        return;
    }

    let Some(value) = value.as_str() else {
        return;
    };

    if value.len() > 4096 {
        return;
    }

    if let (Ok(name), Ok(value)) = (
        HeaderName::from_bytes(name.as_bytes()),
        HeaderValue::from_str(value),
    ) {
        out.insert(name, value);
    }
}

async fn playlist_response(
    response: reqwest::Response,
    base: Url,
    state: &AppState,
    query: &HashMap<String, String>,
    upstream_headers: &HeaderMap,
    head: bool,
) -> Response {
    let status = StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::BAD_GATEWAY);

    let body = match response.bytes().await {
        Ok(b) if b.len() <= state.cfg.max_playlist_bytes => b,
        Ok(_) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "upstream playlist exceeds configured size limit",
            )
        }
        Err(err) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("failed reading upstream playlist: {err}"),
            )
        }
    };

    let rewritten = match rewrite_playlist(
        &String::from_utf8_lossy(&body),
        &base,
        &state.cfg.proxy_path,
        query,
        upstream_headers,
    ) {
        Ok(v) => v,
        Err(err) => return error_response(StatusCode::BAD_GATEWAY, &err),
    };

    let mut headers = cors_headers();
    headers.insert(
        "content-type",
        HeaderValue::from_static("application/vnd.apple.mpegurl"),
    );
    headers.insert(
        "cache-control",
        HeaderValue::from_static("no-cache, no-store, must-revalidate"),
    );
    headers.insert("pragma", HeaderValue::from_static("no-cache"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    add_filename(&mut headers, query.get("filename"));
    headers.insert(
        "content-length",
        HeaderValue::from_str(&rewritten.len().to_string()).unwrap(),
    );

    if head {
        return (status, headers).into_response();
    }

    (status, headers, rewritten).into_response()
}

async fn stream_response(
    response: reqwest::Response,
    head: bool,
    filename: Option<&str>,
) -> Response {
    let status = StatusCode::from_u16(response.status().as_u16())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = cors_headers();

    for name in [
        "content-type",
        "content-length",
        "content-range",
        "accept-ranges",
        "cache-control",
        "etag",
        "expires",
        "last-modified",
    ] {
        if let Some(v) = response.headers().get(name) {
            headers.insert(name, v.clone());
        }
    }

    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );

    if let Some(name) = filename.and_then(sanitize_filename) {
        add_filename_value(&mut headers, &name);
    }

    if head {
        return (status, headers).into_response();
    }

    (
        status,
        headers,
        axum::body::Body::from_stream(response.bytes_stream()),
    )
        .into_response()
}

fn rewrite_playlist(
    text: &str,
    base: &Url,
    proxy_path: &str,
    query: &HashMap<String, String>,
    upstream_headers: &HeaderMap,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len() + 256);
    let suffix = propagated_headers_query(query, upstream_headers)?;

    for line in text.replace("\r\n", "\n").lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('#') {
            out.push_str(&rewrite_uri_attribute(line, base, proxy_path, &suffix)?);
        } else if trimmed.is_empty() {
            out.push_str(line);
        } else {
            let target = resolve_url(base, trimmed)?;
            out.push_str(proxy_path);
            out.push_str("?url=");
            out.push_str(&encode(target.as_str()));
            out.push_str(&suffix);
        }

        out.push('\n');
    }

    Ok(out)
}

fn propagated_headers_query(
    query: &HashMap<String, String>,
    upstream_headers: &HeaderMap,
) -> Result<String, String> {
    let mut headers = serde_json::Map::new();

    // Preserve the existing explicit `headers=` behaviour. This means custom
    // per-stream headers such as Cookie/Authorization continue to propagate to
    // child resources exactly as before.
    if let Some(raw) = query.get("headers") {
        let value: Value =
            serde_json::from_str(raw).map_err(|_| "headers must be valid JSON")?;

        if let Some(obj) = value.as_object() {
            for (k, v) in obj {
                if v.is_string() {
                    headers.insert(k.clone(), v.clone());
                }
            }
        }
    }

    // Automatically propagated request headers use a separate safe allowlist.
    // Do not propagate conditional/cache validators or Range into every child.
    for name in SAFE_PROPAGATED_HEADERS {
        if let Some(value) = upstream_headers.get(*name) {
            if let Ok(value) = value.to_str() {
                headers.insert((*name).to_string(), Value::String(value.to_string()));
            }
        }
    }

    // Origin/Referer are explicit and always propagate when supplied.
    for name in ["referer", "origin"] {
        if let Some(v) = query.get(name) {
            headers.insert(name.to_string(), Value::String(v.clone()));
        }
    }

    if headers.is_empty() {
        return Ok(String::new());
    }

    Ok(format!(
        "&headers={}",
        encode(&Value::Object(headers).to_string())
    ))
}

fn rewrite_uri_attribute(
    line: &str,
    base: &Url,
    proxy_path: &str,
    suffix: &str,
) -> Result<String, String> {
    let Some(start) = line.find("URI=\"") else {
        return Ok(line.to_string());
    };

    let value_start = start + 5;
    let rest = &line[value_start..];
    let Some(end) = rest.find('"') else {
        return Ok(line.to_string());
    };

    let target = resolve_url(base, &rest[..end])?;

    Ok(format!(
        "{}{}?url={}{}{}",
        &line[..value_start],
        proxy_path,
        encode(target.as_str()),
        suffix,
        &rest[end + 1..]
    ))
}

fn resolve_url(base: &Url, raw: &str) -> Result<Url, String> {
    let parsed = Url::parse(raw)
        .or_else(|_| base.join(raw))
        .map_err(|e| format!("invalid playlist resource URL: {e}"))?;

    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "unsupported resource URL scheme: {}",
            parsed.scheme()
        ));
    }

    Ok(parsed)
}

async fn validate_upstream(url: &Url) -> Result<(), String> {
    let host = url.host_str().ok_or("upstream host is missing")?;

    if let Ok(ip) = host.parse::<IpAddr>() {
        return if private_ip(ip) {
            Err("private or special-use upstream addresses are blocked".into())
        } else {
            Ok(())
        };
    }

    let port = url
        .port_or_known_default()
        .ok_or("unsupported upstream port")?;
    let addrs = lookup_host((host, port))
        .await
        .map_err(|_| "unable to resolve upstream host")?;
    let mut found = false;

    for addr in addrs {
        found = true;

        if private_ip(addr.ip()) {
            return Err("upstream host resolves to a private or special-use address".into());
        }
    }

    if !found {
        return Err("upstream host has no addresses".into());
    }

    Ok(())
}

fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_multicast()
                || v.octets()[0] == 0
                || (v.octets()[0] == 100 && (64..=127).contains(&v.octets()[1]))
        }
        IpAddr::V6(v) => {
            v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || v.is_unique_local()
                || ((v.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

fn is_playlist(url: &Url) -> bool {
    let p = url.path().to_ascii_lowercase();
    p.ends_with(".m3u8") || p.ends_with(".txt")
}

fn encode(v: &str) -> String {
    url::form_urlencoded::byte_serialize(v.as_bytes()).collect()
}

fn sanitize_filename(v: &str) -> Option<String> {
    let name = v.rsplit('/').next()?.trim();

    if name.is_empty() || name.len() > 180 {
        return None;
    }

    Some(
        name.chars()
            .map(|c| {
                if c.is_control() || r#"/\\?%*:|\"<>"#.contains(c) {
                    '_'
                } else {
                    c
                }
            })
            .collect(),
    )
}

fn add_filename(headers: &mut HeaderMap, value: Option<&String>) {
    if let Some(name) = value.and_then(|v| sanitize_filename(v)) {
        add_filename_value(headers, &name);
    }
}

fn add_filename_value(headers: &mut HeaderMap, name: &str) {
    if let Ok(v) = HeaderValue::from_str(&format!("inline; filename=\"{name}\"")) {
        headers.insert("content-disposition", v);
    }
}

fn cors_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        "access-control-allow-origin",
        HeaderValue::from_static("*"),
    );
    h.insert(
        "access-control-allow-methods",
        HeaderValue::from_static("GET, HEAD, OPTIONS"),
    );
    h.insert(
        "access-control-allow-headers",
        HeaderValue::from_static("Range, Content-Type, *"),
    );
    h.insert(
        "access-control-expose-headers",
        HeaderValue::from_static("Content-Length, Content-Range, Accept-Ranges, ETag"),
    );
    h
}

fn error_response(status: StatusCode, message: &str) -> Response {
    let mut h = cors_headers();
    h.insert("content-type", HeaderValue::from_static("application/json"));
    (
        status,
        h,
        json!({ "message": message }).to_string(),
    )
        .into_response()
}

impl Config {
    fn from_env() -> Self {
        Self {
            proxy_path: env::var("PROXY_PATH").unwrap_or_else(|_| "/m3u8-proxy".into()),
            upstream_timeout: duration_env("UPSTREAM_TIMEOUT", 10),
            max_playlist_bytes: usize_env("MAX_PLAYLIST_BYTES", 4 << 20),
            max_url_length: usize_env("MAX_URL_LENGTH", 8192),
            max_header_json_length: usize_env("MAX_HEADER_JSON_LENGTH", 8192),
            max_concurrent: usize_env("MAX_CONCURRENT_UPSTREAM", 256).max(1),
            user_agent: env::var("USER_AGENT")
                .unwrap_or_else(|_| "cors-proxy-rust/1.0".into()),
            allow_private_ips: bool_env("ALLOW_PRIVATE_IPS", false),
        }
    }
}

fn usize_env(k: &str, d: usize) -> usize {
    env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(d)
}

fn duration_env(k: &str, secs: u64) -> Duration {
    env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_secs)
        .filter(|v| !v.is_zero())
        .unwrap_or(Duration::from_secs(secs))
}

fn bool_env(k: &str, d: bool) -> bool {
    env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}
