# CORS Proxy — Rust

A small, production-oriented HTTP/HTTPS proxy for HLS playlists and media resources, built with **Rust + Axum + Tokio + Reqwest**.

The repository is intentionally focused on proxying only: no player, frontend, playground, or unrelated application code.

## Features

- HTTP and HTTPS upstreams.
- HLS `.m3u8` playlist fetching and URL rewriting.
- Relative and absolute segment/resource URL resolution.
- Rewrites `URI="..."` resources such as `EXT-X-KEY`, `EXT-X-MAP`, `EXT-X-MEDIA`, and iframe resources.
- Streams `.ts`, `.mp4`, `.key`, initialization segments, images, and other resources without buffering them in memory.
- HTTP `Range` forwarding for media requests.
- `Referer` and `Origin` are treated as **upstream HTTP headers**, not target URL parameters.
- Optional custom upstream headers through JSON.
- CORS response headers for browser HLS clients such as hls.js.
- Bounded upstream concurrency.
- Connection pooling and HTTP/2 support.
- SSRF protection by default: private, loopback, link-local, multicast, unspecified, and other special-use addresses are blocked.
- TLS verification is enabled by default.
- Graceful shutdown and `/health` endpoint.
- Single native binary or minimal container image.

## Request format

Raw upstream playlist:

```text
https://example.com/live/channel/index.m3u8
```

Proxy:

```text
https://proxy.example.com/m3u8-proxy?url=https%3A%2F%2Fexample.com%2Flive%2Fchannel%2Findex.m3u8
```

With upstream headers:

```text
https://proxy.example.com/m3u8-proxy?url=https%3A%2F%2Fexample.com%2Flive%2Fchannel%2Findex.m3u8&referer=https%3A%2F%2Fexample.com%2F&origin=https%3A%2F%2Fexample.com
```

The proxy converts those values into upstream `Referer` and `Origin` headers. They are not appended to the upstream resource URL.

For arbitrary headers, pass a JSON object using the `headers` query parameter, for example:

```json
{"Authorization":"Bearer token","X-Custom":"value"}
```

Use URL encoding when constructing the complete request URL.

## Configuration

| Variable | Default | Purpose |
|---|---:|---|
| `LISTEN_ADDR` | `0.0.0.0:3000` | Listen address |
| `PROXY_PATH` | `/m3u8-proxy` | Proxy endpoint |
| `UPSTREAM_TIMEOUT` | `10` | Seconds to establish/receive upstream response headers |
| `MAX_PLAYLIST_BYTES` | `4194304` | Maximum playlist size |
| `MAX_URL_LENGTH` | `8192` | Maximum target URL length |
| `MAX_HEADER_JSON_LENGTH` | `8192` | Maximum custom-header JSON size |
| `MAX_CONCURRENT_UPSTREAM` | `256` | Maximum concurrent upstream requests |
| `USER_AGENT` | `cors-proxy-rust/1.0` | Default upstream User-Agent |
| `ALLOW_PRIVATE_IPS` | `false` | Explicitly allow private/special-use upstream addresses |
| `RUST_LOG` | `info` | Log filter |

## Local development

Requirements: Rust stable toolchain.

```bash
cargo fmt --check
cargo check
cargo run --release
```

Health check:

```bash
curl http://127.0.0.1:3000/health
```

Test a playlist:

```bash
curl -G 'http://127.0.0.1:3000/m3u8-proxy' \
  --data-urlencode 'url=https://example.com/live/channel/index.m3u8'
```

## Docker

```bash
docker build -t cors-proxy .
docker run --rm -p 3000:3000 cors-proxy
```

## Security

This is an open HTTP proxy unless you add access control at the application or reverse-proxy layer. Do not expose unrestricted proxy access to untrusted users without rate limiting, authentication, or an upstream allowlist appropriate for your deployment.

`ALLOW_PRIVATE_IPS=false` should remain enabled unless internal destinations are deliberately required. The proxy validates DNS results before connecting and rejects private and special-use addresses.

Automatic upstream redirects are deliberately not required for the proxy's security model; if redirect following is introduced later, every redirect target must undergo the same URL and DNS validation.

Caller-controlled headers are filtered to prevent overriding hop-by-hop and proxy-controlled headers.

## Production deployment

See [`DEPLOYMENT.md`](./DEPLOYMENT.md) for VPS, systemd, Nginx, TLS, and Docker deployment guidance.
