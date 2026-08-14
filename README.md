# M3U8 Proxy — Go

A small production-oriented HTTP/HTTPS proxy for HLS playlists and media resources. It is designed for VPS deployment and focuses only on proxying: no player, frontend, playground, or unrelated application code.

## What it does

- Accepts `http://` and `https://` upstream URLs.
- Fetches `.m3u8`/`.txt` playlists server-side.
- Rewrites playlist resource URLs so browsers request them through the HTTPS proxy instead of the original HTTP origin.
- Proxies `.ts`, `.mp4`, `.key`, initialization segments, images, and other HLS resources.
- Preserves useful response headers and byte-range requests.
- Supports optional upstream request headers through a JSON query parameter.
- Includes CORS headers for browser HLS clients such as hls.js.
- Includes a `/health` endpoint.
- Uses connection pooling and bounded upstream concurrency.
- Includes SSRF protection by default: private/special-use upstream IPs are blocked unless explicitly enabled.
- Builds to a single Go binary with no runtime dependency on Node.js or Python.

## Example

Raw upstream:

```text
http://example.com/iptv/MCHUP9AS7DBP5W/7342/index.m3u8
```

Proxy endpoint:

```text
https://proxy.example.com/m3u8-proxy?url=http%3A%2F%2Fexample.com%2Fiptv%2FMCHUP9AS7DBP5W%2F7342%2Findex.m3u8
```

The browser communicates only with the HTTPS proxy. The proxy may communicate with an HTTP or HTTPS origin, so an HTTPS page does not directly load an HTTP `.m3u8` and trigger mixed-content blocking.

## Requirements

- Go 1.22 or newer.
- Linux VPS recommended.
- Nginx and Let's Encrypt/Certbot recommended for public HTTPS.

## Local development

```bash
go mod tidy
go run ./cmd/m3u8-proxy
```

Test health:

```bash
curl http://127.0.0.1:3000/health
```

Test a playlist through the proxy:

```bash
curl -G 'http://127.0.0.1:3000/m3u8-proxy' \
  --data-urlencode 'url=http://example.com/iptv/MCHUP9AS7DBP5W/7342/index.m3u8'
```

## Production

See [`DEPLOYMENT.md`](./DEPLOYMENT.md) for a complete SSH → DNS → Go → systemd → Nginx → SSL deployment.

## Security notes

This is an HTTP proxy, which means it must be protected from abuse. Keep `ALLOW_PRIVATE_IPS=false` unless there is a deliberate reason to proxy internal destinations. Do not expose unrestricted proxy access to untrusted users without rate limiting, authentication, or an upstream allowlist appropriate for your application.

The optional `headers` parameter is deliberately filtered; hop-by-hop and CORS response-control headers cannot be injected by callers.
