# Production Deployment — Rust CORS Proxy

This guide covers production deployment of the standalone Rust HLS/CORS proxy using **Railway, Docker, or a Linux VPS**. It includes one-click Railway deployment, environment configuration, health checks, HLS streaming, `Referer`/`Origin` forwarding, SSRF protection, Nginx, HTTPS, systemd, monitoring, upgrades, rollback, and troubleshooting.

## Architecture

```text
Browser / HLS.js
       │
       │ HTTPS
       ▼
┌──────────────────────┐
│ Railway / Nginx      │
│ TLS + public routing │
└──────────┬───────────┘
           │
           ▼
┌──────────────────────┐
│ Rust + Axum + Tokio  │
│                      │
│ URL validation       │
│ SSRF protection      │
│ HLS rewriting        │
│ CORS                 │
│ concurrency limits   │
└──────────┬───────────┘
           │ Reqwest
           │ HTTP/HTTPS
           ▼
     Upstream origin
```

The proxy performs upstream requests server-side and streams responses back to the browser. `Referer` and `Origin` are sent as actual upstream HTTP headers; they are not appended to the upstream resource URL.

---

# 1. Railway — Recommended Cloud Deployment

Railway is the simplest deployment option for this service because it runs the Rust HTTP server as a persistent container and provides a public HTTPS endpoint.

## One-click deployment

Click the button below to deploy the repository to Railway:

[![Deploy on Railway](https://railway.com/button.svg)](https://railway.com/deploy?repo=https://github.com/maxieyy/CORS-PROXY-GO)

### What happens

The Railway deployment uses the repository `Dockerfile` and `railway.toml`.

```text
GitHub repository
       │
       ▼
Railway detects Dockerfile
       │
       ▼
Build Rust release image
       │
       ▼
Start cors-proxy
       │
       ▼
GET /health
       │
       ▼
Railway marks deployment healthy
```

The service automatically binds to Railway's `PORT` environment variable. You do **not** need to hard-code a Railway port.

## Railway configuration

The repository includes `railway.toml` with:

- Dockerfile builder
- `/health` health check
- 30-second health-check timeout
- automatic restart on failure
- maximum restart attempts
- `PROXY_PATH=/m3u8-proxy`
- `ALLOW_PRIVATE_IPS=false`
- `RUST_LOG=info`

Railway supplies `PORT` dynamically. The Docker entrypoint converts it to:

```text
LISTEN_ADDR=0.0.0.0:$PORT
```

## After deployment

Open the Railway-generated public domain and test:

```bash
curl -i https://YOUR-RAILWAY-DOMAIN/health
```

Expected response:

```http
HTTP/2 200
content-type: application/json
```

with a body similar to:

```json
{"status":"ok","available_upstreams":256}
```

## Railway environment variables

The important production settings are:

```text
PROXY_PATH=/m3u8-proxy
ALLOW_PRIVATE_IPS=false
RUST_LOG=info
```

Optional tuning variables:

```text
UPSTREAM_TIMEOUT=10
MAX_PLAYLIST_BYTES=4194304
MAX_URL_LENGTH=8192
MAX_HEADER_JSON_LENGTH=8192
MAX_CONCURRENT_UPSTREAM=256
USER_AGENT=cors-proxy-rust/1.0
```

Do **not** set `LISTEN_ADDR` manually on Railway unless you have a specific reason. The container derives it from `$PORT`.

## Custom Railway domain

After deployment:

1. Open the Railway service.
2. Open **Settings / Networking**.
3. Generate a Railway domain or attach your own domain.
4. Configure your DNS according to Railway's instructions.
5. Test `/health` over HTTPS.

For example:

```text
https://proxy.example.com/health
```

Your proxy endpoint then becomes:

```text
https://proxy.example.com/m3u8-proxy?url=...
```

## Railway logs

Use the Railway deployment logs to monitor:

- application startup
- binding errors
- upstream failures
- rejected requests
- health checks
- restarts

The Rust service uses `RUST_LOG` for structured application logging.

---

# 2. Railway Production Checklist

Before exposing the proxy publicly:

- [ ] Deployment is healthy.
- [ ] `/health` returns HTTP 200.
- [ ] HTTPS is enabled.
- [ ] `ALLOW_PRIVATE_IPS=false`.
- [ ] Upstream playlist can be fetched.
- [ ] HLS URLs are rewritten.
- [ ] Relative playlist URLs work.
- [ ] Media segments stream.
- [ ] Range requests work.
- [ ] `Referer` and `Origin` are forwarded.
- [ ] Required custom headers work.
- [ ] Public proxy abuse controls are considered.
- [ ] Railway resource usage is monitored.

---

# 3. Docker Deployment

The repository contains a multi-stage Dockerfile. The final image contains only the compiled Rust binary and CA certificates.

Build locally:

```bash
docker build -t cors-proxy:latest .
```

Run:

```bash
docker run -d \
  --name cors-proxy \
  --restart unless-stopped \
  -e LISTEN_ADDR=0.0.0.0:3000 \
  -e PROXY_PATH=/m3u8-proxy \
  -e ALLOW_PRIVATE_IPS=false \
  -e RUST_LOG=info \
  -p 127.0.0.1:3000:3000 \
  cors-proxy:latest
```

Health check:

```bash
curl -fsS http://127.0.0.1:3000/health
```

Logs:

```bash
docker logs -f cors-proxy
```

For a VPS, put Nginx in front of the container and do not expose port 3000 publicly.

## Docker Compose

```yaml
services:
  cors-proxy:
    build: .
    restart: unless-stopped
    environment:
      LISTEN_ADDR: 0.0.0.0:3000
      PROXY_PATH: /m3u8-proxy
      ALLOW_PRIVATE_IPS: "false"
      RUST_LOG: info
      MAX_CONCURRENT_UPSTREAM: 256
    ports:
      - "127.0.0.1:3000:3000"
    read_only: true
    security_opt:
      - no-new-privileges:true
    cap_drop:
      - ALL
```

Start:

```bash
docker compose up -d --build
```

---

# 4. Linux VPS Deployment

## Recommended VPS

For a small deployment:

- Debian 12 or Ubuntu 24.04 LTS
- 1 vCPU minimum
- 1 GB RAM recommended
- 10+ GB SSD
- public IPv4
- reliable network connection

For streaming proxies, **network throughput and bandwidth are generally more important than RAM**.

The application does not require Node.js, Python, Redis, a database, or a frontend.

## Install dependencies

```bash
sudo apt update
sudo apt upgrade -y
sudo apt install -y curl ca-certificates git build-essential pkg-config nginx ufw
```

Install Rust:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
rustup default stable
```

Verify:

```bash
rustc --version
cargo --version
```

## Dedicated user

```bash
sudo adduser --system --group --no-create-home --shell /usr/sbin/nologin corsproxy
sudo mkdir -p /opt/cors-proxy
sudo chown -R "$USER":"$USER" /opt/cors-proxy
```

Clone:

```bash
cd /opt
git clone https://github.com/maxieyy/CORS-PROXY-GO.git cors-proxy
cd /opt/cors-proxy
```

## Build

```bash
cargo fmt --all -- --check
cargo check --locked
cargo test --locked
cargo build --release --locked
```

Install:

```bash
sudo install -o root -g root -m 0755 target/release/cors-proxy /usr/local/bin/cors-proxy
```

---

# 5. Environment Configuration

For a VPS, create:

```bash
sudo nano /etc/cors-proxy.env
```

Example:

```env
LISTEN_ADDR=127.0.0.1:3000
PROXY_PATH=/m3u8-proxy
UPSTREAM_TIMEOUT=10
MAX_PLAYLIST_BYTES=4194304
MAX_URL_LENGTH=8192
MAX_HEADER_JSON_LENGTH=8192
MAX_CONCURRENT_UPSTREAM=256
USER_AGENT=cors-proxy-rust/1.0
ALLOW_PRIVATE_IPS=false
RUST_LOG=info
```

Protect it:

```bash
sudo chown root:root /etc/cors-proxy.env
sudo chmod 0640 /etc/cors-proxy.env
```

## Configuration reference

| Variable | Default | Purpose |
|---|---:|---|
| `LISTEN_ADDR` | `0.0.0.0:3000` | Axum bind address |
| `PROXY_PATH` | `/m3u8-proxy` | Proxy endpoint |
| `UPSTREAM_TIMEOUT` | `10` | Upstream timeout in seconds |
| `MAX_PLAYLIST_BYTES` | `4194304` | Maximum playlist size |
| `MAX_URL_LENGTH` | `8192` | Maximum target URL length |
| `MAX_HEADER_JSON_LENGTH` | `8192` | Maximum custom-header JSON size |
| `MAX_CONCURRENT_UPSTREAM` | `256` | Concurrent upstream requests |
| `USER_AGENT` | `cors-proxy-rust/1.0` | Default upstream User-Agent |
| `ALLOW_PRIVATE_IPS` | `false` | Allow private/special-use destinations |
| `RUST_LOG` | `info` | Log filter |

For public deployments, keep:

```env
ALLOW_PRIVATE_IPS=false
```

---

# 6. systemd Deployment

Create:

```bash
sudo nano /etc/systemd/system/cors-proxy.service
```

Use:

```ini
[Unit]
Description=Rust HLS CORS Proxy
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=corsproxy
Group=corsproxy
EnvironmentFile=/etc/cors-proxy.env
ExecStart=/usr/local/bin/cors-proxy
Restart=on-failure
RestartSec=3
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictSUIDSGID=true
LockPersonality=true
MemoryDenyWriteExecute=true

[Install]
WantedBy=multi-user.target
```

Enable:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now cors-proxy
```

Check:

```bash
sudo systemctl status cors-proxy --no-pager
curl -i http://127.0.0.1:3000/health
```

Logs:

```bash
sudo journalctl -u cors-proxy -f
```

---

# 7. Nginx + HTTPS

Install:

```bash
sudo apt install -y nginx certbot python3-certbot-nginx
```

Create `/etc/nginx/sites-available/cors-proxy`:

```nginx
server {
    listen 80;
    listen [::]:80;
    server_name proxy.example.com;

    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_http_version 1.1;
        proxy_buffering off;
        proxy_request_buffering off;
        proxy_cache off;
        proxy_read_timeout 1h;
        proxy_send_timeout 1h;
        send_timeout 1h;

        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

Enable:

```bash
sudo ln -s /etc/nginx/sites-available/cors-proxy /etc/nginx/sites-enabled/cors-proxy
sudo nginx -t
sudo systemctl reload nginx
```

Request HTTPS:

```bash
sudo certbot --nginx -d proxy.example.com
sudo certbot renew --dry-run
```

For HLS, keep proxy buffering disabled and use long read/send timeouts so long-running media responses are not terminated prematurely.

---

# 8. Firewall

If Nginx is public:

```bash
sudo ufw allow OpenSSH
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw enable
sudo ufw status verbose
```

Do not expose `3000` publicly when using Nginx.

---

# 9. HLS Proxy Usage

Basic playlist:

```bash
curl -G 'https://proxy.example.com/m3u8-proxy' \
  --data-urlencode 'url=https://example.com/live/index.m3u8'
```

The proxy rewrites playlist resources so child playlists, segments, encryption keys, and initialization resources continue through the proxy.

Relative URLs are resolved against the original playlist URL.

## Referer and Origin

```bash
curl -G 'https://proxy.example.com/m3u8-proxy' \
  --data-urlencode 'url=https://origin.example/live/index.m3u8' \
  --data-urlencode 'referer=https://origin.example/' \
  --data-urlencode 'origin=https://origin.example'
```

The upstream receives:

```text
Referer: https://origin.example/
Origin: https://origin.example
```

They are **HTTP request headers**, not URL parameters sent to the origin.

## Custom headers

```bash
curl -G 'https://proxy.example.com/m3u8-proxy' \
  --data-urlencode 'url=https://origin.example/live/index.m3u8' \
  --data-urlencode 'headers={"X-Custom-Header":"value"}'
```

Protected hop-by-hop and proxy-controlled headers are filtered.

## Range requests

```bash
curl -i \
  -H 'Range: bytes=0-1023' \
  'https://proxy.example.com/m3u8-proxy?url=https%3A%2F%2Fexample.com%2Fmedia.ts'
```

The proxy forwards `Range` and relevant response headers including `Content-Range`, `Accept-Ranges`, `ETag`, and `Content-Length`.

---

# 10. Browser / HLS.js Integration

```javascript
const upstream = 'https://origin.example/live/index.m3u8';
const proxyUrl = `/m3u8-proxy?url=${encodeURIComponent(upstream)}`;

hls.loadSource(proxyUrl);
```

For a page served over HTTPS, use the HTTPS proxy URL to avoid mixed-content blocking.

---

# 11. CORS

The proxy returns browser-compatible CORS headers and exposes streaming headers such as:

```text
Content-Length
Content-Range
Accept-Ranges
ETag
```

The browser talks to the proxy, while Reqwest talks to the upstream. Therefore the upstream's browser CORS policy does not have to permit the browser directly.

---

# 12. SSRF Security

This proxy accepts arbitrary upstream URLs, so SSRF protection is a critical security boundary.

By default it validates upstream DNS results and rejects private and special-use destinations, including loopback, private networks, link-local, multicast, unspecified, and other reserved addresses.

Keep:

```env
ALLOW_PRIVATE_IPS=false
```

Do not expose internal services, cloud metadata endpoints, databases, Docker sockets, admin interfaces, or private network resources through a public instance.

Upstream redirects are intentionally disabled. If redirect support is introduced, **every redirect destination must undergo the same URL/DNS/IP validation**.

For a public unrestricted proxy, also consider DNS rebinding defenses, authentication, signed URLs, rate limiting, and upstream allowlists.

---

# 13. Rate Limiting and Abuse Protection

A public proxy can become a bandwidth relay. Application concurrency limits are not a complete abuse-control mechanism.

Recommended edge controls:

- Railway/Cloudflare rate limiting where available
- Nginx `limit_req`
- API keys
- signed proxy URLs
- per-IP request limits
- per-client stream limits
- bandwidth quotas
- upstream allowlists for private deployments

Monitor bandwidth as well as CPU and memory.

---

# 14. Monitoring

### systemd

```bash
sudo journalctl -u cors-proxy -f
sudo journalctl -u cors-proxy -p warning -n 100 --no-pager
systemctl status cors-proxy
```

### Docker

```bash
docker logs -f cors-proxy
docker stats cors-proxy
```

### VPS

```bash
sudo ss -lntp
sudo ss -ntp
free -h
df -h
```

Monitor request rate, status codes, upstream latency, upstream errors, active connections, bandwidth, CPU, memory, file descriptors, and SSRF rejections.

Railway deployments should additionally be monitored through Railway's deployment and resource metrics.

---

# 15. Upgrades

## Railway

The recommended workflow is:

```text
Push to main
    ↓
Railway detects change
    ↓
Build Docker image
    ↓
Start new deployment
    ↓
/health check
    ↓
Traffic served by healthy deployment
```

Pin production deployments to reviewed commits/releases where reproducibility matters.

## VPS

```bash
cd /opt/cors-proxy
git fetch --prune
git checkout main
git pull --ff-only
cargo fmt --all -- --check
cargo check --locked
cargo test --locked
cargo build --release --locked
sudo install -m 0755 target/release/cors-proxy /usr/local/bin/cors-proxy
sudo systemctl restart cors-proxy
curl -fsS http://127.0.0.1:3000/health
```

---

# 16. Rollback

For a VPS, preserve the previous binary:

```bash
sudo cp /usr/local/bin/cors-proxy /usr/local/bin/cors-proxy.previous
```

Rollback:

```bash
sudo systemctl stop cors-proxy
sudo cp /usr/local/bin/cors-proxy.previous /usr/local/bin/cors-proxy
sudo systemctl start cors-proxy
curl -fsS http://127.0.0.1:3000/health
```

For Railway, redeploy the previous known-good deployment/revision from the Railway deployment history.

---

# 17. Troubleshooting

## Railway deployment fails

Check Docker build logs, runtime logs, that the service listens on `0.0.0.0:$PORT`, that `/health` returns 200, and that `Cargo.lock` and source are present.

The included Docker entrypoint handles Railway's dynamic `PORT` automatically.

## `/health` returns 502 behind Nginx

```bash
curl -i http://127.0.0.1:3000/health
sudo journalctl -u cors-proxy -n 100 --no-pager
sudo nginx -t
sudo tail -n 100 /var/log/nginx/error.log
```

## Playlist loads but segments fail

Check rewritten URLs, relative URL resolution, HTTPS/mixed-content issues, `Referer`, `Origin`, User-Agent, upstream status codes, Nginx streaming timeouts, and upstream DNS/IP validation.

## Upstream returns 403

The origin may require specific headers. Supply only legitimate required headers such as `Referer`, `Origin`, and `User-Agent`.

## Upstream returns 404

Verify the original playlist's relative path resolution and inspect the upstream playlist directly.

## Upstream times out

```bash
curl -I --max-time 15 https://example.com/
```

Increase `UPSTREAM_TIMEOUT` only when the upstream legitimately requires more time.

## Browser reports CORS errors

Check the proxy response for:

```text
Access-Control-Allow-Origin: *
Access-Control-Allow-Methods: GET, HEAD, OPTIONS
```

Also ensure Nginx is not stripping response headers.

---

# 18. Performance Tuning

Start with:

```env
MAX_CONCURRENT_UPSTREAM=256
UPSTREAM_TIMEOUT=10
MAX_PLAYLIST_BYTES=4194304
```

Increase concurrency only after measuring bandwidth, open connections, CPU, memory, file descriptors, and upstream limits.

For streaming workloads:

- keep Nginx buffering disabled
- use long streaming timeouts
- avoid unnecessary application buffering
- retain Reqwest connection pooling
- use HTTP/2 where beneficial
- scale based on network throughput before simply adding RAM

The proxy is not a transcoder. Its job is to validate, rewrite playlists when necessary, and relay bytes efficiently.

---

# 19. Production Checklist

- [ ] HTTPS enabled.
- [ ] `/health` returns 200.
- [ ] `ALLOW_PRIVATE_IPS=false`.
- [ ] Proxy is not exposed directly on an unnecessary port.
- [ ] Nginx buffering disabled for VPS deployments.
- [ ] Long streaming timeouts configured.
- [ ] HLS master/media playlists tested.
- [ ] Relative URLs tested.
- [ ] Segments tested.
- [ ] Range requests tested.
- [ ] `Referer`/`Origin` tested.
- [ ] Required custom headers tested.
- [ ] SSRF protection verified.
- [ ] Authentication/rate limiting considered for public use.
- [ ] Logs monitored.
- [ ] Bandwidth monitored.
- [ ] Rollback procedure tested.
- [ ] Railway health check verified if using Railway.

---

# 20. Production Request Flow

```text
                       INTERNET
                           │
                           ▼
                  HTTPS / Railway / Nginx
                           │
                           ▼
                 ┌───────────────────┐
                 │   Rust / Axum     │
                 │                   │
                 │ URL validation    │
                 │ SSRF protection   │
                 │ HLS rewriting     │
                 │ CORS              │
                 │ concurrency       │
                 └─────────┬─────────┘
                           │
                           ▼
                    Reqwest client
                           │
                    Referer / Origin
                    as HTTP headers
                           │
                           ▼
                    Upstream origin
                           │
                    .m3u8 / segments
                    keys / resources
```

For most deployments, **Railway is the easiest option**: click the Deploy on Railway button, verify `/health`, configure a domain if desired, and start sending your HLS requests through the generated HTTPS proxy endpoint.
