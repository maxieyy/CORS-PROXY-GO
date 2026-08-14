# Production Deployment — Rust CORS Proxy

This guide covers a complete production deployment of the standalone Rust HLS/CORS proxy on a Linux VPS. It covers source deployment, configuration, systemd, Nginx, HTTPS, firewalling, Docker, testing, logging, upgrades, rollback, troubleshooting, performance, and security.

> **Architecture:** Browser → HTTPS/Nginx → Rust/Axum → upstream HTTP/HTTPS origin.
>
> The browser never needs to connect directly to an HTTP upstream. The Rust service performs the upstream request server-side and streams the response back to the browser.

## 1. Requirements

### Recommended VPS

For a small deployment:

- Debian 12 or Ubuntu 24.04 LTS
- 1 vCPU minimum
- 1 GB RAM recommended
- 10+ GB SSD
- Public IPv4 address
- Optional IPv6
- At least 1 Gbps network interface where available

For this workload, **bandwidth and network throughput are normally more important than RAM**. A proxy that relays a 5 Mbps stream consumes roughly 5 Mbps of upstream traffic and 5 Mbps of downstream traffic per viewer, before protocol overhead.

### Required software

- Rust stable toolchain
- Cargo
- Git
- Nginx
- Certbot + Let's Encrypt
- curl
- ca-certificates
- build-essential / compiler toolchain
- systemd

The proxy itself does not require a database, Redis, Node.js, Python, or a frontend.

## 2. Create a dedicated deployment user

Do not run the proxy as root.

```bash
sudo adduser --system --group --no-create-home --shell /usr/sbin/nologin corsproxy
```

Create the application directory:

```bash
sudo mkdir -p /opt/cors-proxy
sudo chown -R corsproxy:corsproxy /opt/cors-proxy
```

For building from source, either use a separate build user or temporarily grant the deployment user access to the source directory. The final service should run as `corsproxy`.

## 3. Install system packages

Debian/Ubuntu:

```bash
sudo apt update
sudo apt upgrade -y
sudo apt install -y \
  curl \
  ca-certificates \
  git \
  build-essential \
  pkg-config \
  nginx \
  ufw
```

Verify:

```bash
curl --version
nginx -v
systemctl --version
```

## 4. Install Rust

Use the official Rust toolchain installer for the deployment/build account:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Reload the shell environment:

```bash
source "$HOME/.cargo/env"
```

Verify:

```bash
rustc --version
cargo --version
rustup show
```

Use the stable toolchain:

```bash
rustup default stable
rustup update stable
```

## 5. Obtain the source

Clone the repository:

```bash
cd /opt
sudo git clone https://github.com/maxieyy/CORS-PROXY-GO.git cors-proxy
sudo chown -R "$USER":"$USER" /opt/cors-proxy
cd /opt/cors-proxy
```

If deploying a specific release/commit, check out that exact revision rather than running an arbitrary moving branch:

```bash
git fetch --tags --prune
git checkout <COMMIT_OR_TAG>
```

Confirm the Rust project is present:

```bash
ls -la
cat Cargo.toml
```

## 6. Build a production binary

Run formatting and compilation checks:

```bash
cargo fmt --all -- --check
cargo check --locked
cargo test --locked
```

Build the optimized binary:

```bash
cargo build --release --locked
```

Verify it exists:

```bash
ls -lh target/release/cors-proxy
file target/release/cors-proxy
```

For a production deployment, install the resulting binary outside the source tree:

```bash
sudo install -o root -g root -m 0755 \
  target/release/cors-proxy \
  /usr/local/bin/cors-proxy
```

## 7. Environment configuration

Create the environment file:

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

### Configuration reference

| Variable | Default | Purpose |
|---|---:|---|
| `LISTEN_ADDR` | `0.0.0.0:3000` | Address/port used by Axum |
| `PROXY_PATH` | `/m3u8-proxy` | Proxy endpoint |
| `UPSTREAM_TIMEOUT` | `10` | Upstream connection/request timeout in seconds |
| `MAX_PLAYLIST_BYTES` | `4194304` | Maximum playlist body accepted for rewriting |
| `MAX_URL_LENGTH` | `8192` | Maximum target URL length |
| `MAX_HEADER_JSON_LENGTH` | `8192` | Maximum custom-header JSON size |
| `MAX_CONCURRENT_UPSTREAM` | `256` | Maximum concurrent proxied requests |
| `USER_AGENT` | `cors-proxy-rust/1.0` | Default upstream User-Agent |
| `ALLOW_PRIVATE_IPS` | `false` | Allows private/special-use upstream IPs when explicitly enabled |
| `RUST_LOG` | `info` | Rust tracing filter |

### Security-sensitive configuration

Keep this in production:

```env
ALLOW_PRIVATE_IPS=false
```

The proxy resolves upstream hostnames and rejects private, loopback, link-local, multicast, unspecified, and other special-use destinations by default.

Do **not** enable `ALLOW_PRIVATE_IPS=true` on an internet-facing unrestricted proxy unless you fully understand the SSRF implications and have an explicit trusted use case.

## 8. Test the binary manually

Before creating the systemd service, start it manually:

```bash
sudo -u corsproxy /usr/local/bin/cors-proxy
```

In another SSH session:

```bash
curl -i http://127.0.0.1:3000/health
```

You should receive HTTP 200 and JSON similar to:

```json
{"status":"ok","available_upstreams":256}
```

Stop the process with `Ctrl+C`.

## 9. Configure systemd

Create:

```bash
sudo nano /etc/systemd/system/cors-proxy.service
```

Use:

```ini
[Unit]
Description=Rust HLS CORS Proxy
Documentation=https://github.com/maxieyy/CORS-PROXY-GO
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

# The proxy needs outbound networking but does not need to listen publicly.
# Keep the application bound to 127.0.0.1 when Nginx is the public entry point.

[Install]
WantedBy=multi-user.target
```

Reload systemd:

```bash
sudo systemctl daemon-reload
```

Enable and start:

```bash
sudo systemctl enable --now cors-proxy
```

Check:

```bash
sudo systemctl status cors-proxy --no-pager
```

Check logs:

```bash
sudo journalctl -u cors-proxy -n 100 --no-pager
```

Follow logs live:

```bash
sudo journalctl -u cors-proxy -f
```

## 10. Firewall

If Nginx is the public entry point, expose only SSH, HTTP, and HTTPS:

```bash
sudo ufw allow OpenSSH
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw enable
sudo ufw status verbose
```

Do **not** expose port `3000` publicly when the application is configured as:

```env
LISTEN_ADDR=127.0.0.1:3000
```

Confirm:

```bash
sudo ss -lntp
```

The Rust process should listen on loopback, while Nginx listens on ports 80/443.

## 11. Configure DNS

At your DNS provider create an A record:

```text
Type: A
Name: proxy
Value: YOUR_SERVER_IPV4
```

For IPv6, only publish an AAAA record if the server is correctly configured for IPv6.

Verify:

```bash
getent hosts proxy.example.com
```

The returned address must point to your server.

## 12. Configure Nginx

Create a site configuration:

```bash
sudo nano /etc/nginx/sites-available/cors-proxy
```

### HTTP configuration for initial certificate issuance

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
        proxy_read_timeout 1h;
        proxy_send_timeout 1h;

        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

Enable it:

```bash
sudo ln -s /etc/nginx/sites-available/cors-proxy /etc/nginx/sites-enabled/cors-proxy
sudo nginx -t
sudo systemctl reload nginx
```

Test:

```bash
curl -i http://proxy.example.com/health
```

## 13. HTTPS with Let's Encrypt

Install Certbot:

```bash
sudo apt install -y certbot python3-certbot-nginx
```

Request the certificate:

```bash
sudo certbot --nginx -d proxy.example.com
```

Allow Certbot to configure the HTTPS redirect when prompted.

Test Nginx:

```bash
sudo nginx -t
sudo systemctl reload nginx
```

Test HTTPS:

```bash
curl -i https://proxy.example.com/health
```

Expected:

```text
HTTP/2 200
```

Check certificate renewal:

```bash
sudo certbot renew --dry-run
```

## 14. Production Nginx streaming configuration

For HLS and media streaming, buffering is undesirable because it can increase latency and memory/disk usage. Use:

```nginx
server {
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name proxy.example.com;

    ssl_certificate /etc/letsencrypt/live/proxy.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/proxy.example.com/privkey.pem;

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

Do not add an aggressive `client_max_body_size` requirement to the proxy unless your deployment needs request-body restrictions. Normal proxy traffic is primarily GET/HEAD.

## 15. Test the proxy

Basic upstream playlist:

```bash
curl -G 'https://proxy.example.com/m3u8-proxy' \
  --data-urlencode 'url=https://example.com/live/index.m3u8'
```

The response should contain rewritten URLs pointing back to your proxy instead of directly to the upstream resources.

### With Referer and Origin

```bash
curl -G 'https://proxy.example.com/m3u8-proxy' \
  --data-urlencode 'url=https://origin.example/live/index.m3u8' \
  --data-urlencode 'referer=https://origin.example/' \
  --data-urlencode 'origin=https://origin.example'
```

The proxy sends:

```text
Referer: https://origin.example/
Origin: https://origin.example
```

to the upstream HTTP server. They are **not** appended to the upstream target URL.

### With custom headers

The `headers` parameter accepts JSON string values:

```bash
curl -G 'https://proxy.example.com/m3u8-proxy' \
  --data-urlencode 'url=https://origin.example/live/index.m3u8' \
  --data-urlencode 'headers={"Referer":"https://origin.example/","X-Custom-Header":"value"}'
```

Do not attempt to override hop-by-hop or proxy-controlled headers such as `Host`, `Connection`, `Content-Length`, or CORS response headers. The service filters protected headers.

## 16. HLS playlist rewriting

The proxy rewrites resources inside playlists so the player continues through the proxy.

For example:

```text
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=2000000
video/720p/index.m3u8
```

becomes conceptually:

```text
#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=2000000
/m3u8-proxy?url=https%3A%2F%2Forigin.example%2Flive%2Fvideo%2F720p%2Findex.m3u8
```

The same mechanism is used for URI-bearing HLS tags such as encryption keys and initialization resources.

Relative URLs are resolved against the original playlist URL before being proxied.

## 17. Range requests

Media players may request only a byte range of a resource. The proxy forwards the incoming `Range` header upstream and returns relevant response headers such as:

- `Content-Length`
- `Content-Range`
- `Accept-Ranges`
- `ETag`
- `Last-Modified`
- `Cache-Control`

Test with:

```bash
curl -i \
  -H 'Range: bytes=0-1023' \
  'https://proxy.example.com/m3u8-proxy?url=https%3A%2F%2Fexample.com%2Fmedia.ts'
```

A supporting upstream may return `206 Partial Content`.

## 18. CORS behavior

The Rust service returns CORS headers intended for browser-based media clients. This allows an HTTPS page on another origin to consume the proxied response.

The proxy exposes useful streaming headers including:

```text
Content-Length
Content-Range
Accept-Ranges
ETag
```

Do not confuse **proxy CORS headers** with the upstream server's CORS policy. The browser sees the Rust proxy as its HTTP endpoint; the Rust proxy performs the upstream request server-side.

## 19. Browser/player integration

For an HTTPS website, do not give the player a raw HTTP upstream URL.

Instead:

```javascript
const upstream = 'http://origin.example/live/index.m3u8';
const proxyUrl = `/m3u8-proxy?url=${encodeURIComponent(upstream)}`;

hls.loadSource(proxyUrl);
```

The browser requests your HTTPS proxy, and the server performs the HTTP upstream request.

## 20. Docker deployment

Build:

```bash
docker build -t cors-proxy:latest .
```

Create `.env`:

```env
LISTEN_ADDR=0.0.0.0:3000
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

Run behind Nginx:

```bash
docker run -d \
  --name cors-proxy \
  --restart unless-stopped \
  --env-file .env \
  -p 127.0.0.1:3000:3000 \
  cors-proxy:latest
```

Check:

```bash
curl http://127.0.0.1:3000/health
```

Nginx should proxy to the container exactly as it would proxy to the native systemd service.

## 21. Docker Compose example

```yaml
services:
  cors-proxy:
    build: .
    restart: unless-stopped
    env_file:
      - .env
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

Logs:

```bash
docker compose logs -f cors-proxy
```

## 22. Monitoring and logs

systemd logs:

```bash
sudo journalctl -u cors-proxy -f
```

Recent errors:

```bash
sudo journalctl -u cors-proxy -p warning -n 100 --no-pager
```

Service status:

```bash
systemctl is-active cors-proxy
systemctl is-enabled cors-proxy
```

Listening sockets:

```bash
sudo ss -lntp
```

Network connections:

```bash
sudo ss -ntp
```

Resource usage:

```bash
systemctl status cors-proxy
ps aux --sort=-%cpu | head
free -h
```

For serious production deployments, collect at least:

- Request rate
- HTTP status distribution
- Upstream failure rate
- Upstream latency
- Active connections
- Bandwidth in/out
- CPU utilization
- Memory utilization
- File descriptor usage
- Number of rejected SSRF requests

## 23. Rate limiting and abuse protection

An unrestricted proxy can be abused as a bandwidth relay. For a public deployment, put rate limiting and authentication at the edge.

Possible controls include:

- Nginx `limit_req`
- Cloudflare rate limiting
- API keys
- Signed proxy URLs
- Per-IP request limits
- Maximum concurrent streams per client
- Maximum upstream bandwidth
- Origin allowlists for private deployments

Do not rely on `MAX_CONCURRENT_UPSTREAM` as a complete abuse-prevention system. It protects the Rust process from unlimited concurrency, but it does not identify abusive clients.

## 24. SSRF considerations

This service accepts an arbitrary upstream URL, which makes SSRF protection a critical security boundary.

The default behavior rejects private and special-use addresses. Keep:

```env
ALLOW_PRIVATE_IPS=false
```

Do not expose internal metadata endpoints, local services, databases, admin panels, Docker sockets, or private network resources through this proxy.

Automatic redirect following should remain disabled unless redirects are implemented with validation of **every redirect destination**.

DNS rebinding is another consideration for unrestricted public proxies. A production-hardening effort may pin validated DNS results to the connection path or use a controlled resolver/network policy so validation and connection cannot be separated by a DNS change.

## 25. Upgrading the service

Pull the latest source:

```bash
cd /opt/cors-proxy
git fetch --prune
git checkout main
git pull --ff-only
```

Run checks:

```bash
cargo fmt --all -- --check
cargo check --locked
cargo test --locked
cargo build --release --locked
```

Install the new binary:

```bash
sudo install -o root -g root -m 0755 \
  target/release/cors-proxy \
  /usr/local/bin/cors-proxy
```

Restart:

```bash
sudo systemctl restart cors-proxy
sudo systemctl status cors-proxy --no-pager
```

Verify immediately:

```bash
curl -fsS http://127.0.0.1:3000/health
```

Then test the public HTTPS endpoint.

## 26. Zero/minimal-downtime upgrade strategy

For higher traffic deployments, do not replace a production binary blindly. Recommended options include:

1. Run two application instances on different localhost ports.
2. Let Nginx route traffic to the active instance.
3. Start and health-check the new instance.
4. Switch Nginx upstream traffic.
5. Drain existing connections from the old instance.
6. Stop the old instance.

For a small deployment, a normal systemd restart is usually sufficient because HLS clients can retry segments.

## 27. Rollback

Keep the previous known-good binary:

```bash
sudo cp /usr/local/bin/cors-proxy /usr/local/bin/cors-proxy.previous
```

If a new release fails:

```bash
sudo systemctl stop cors-proxy
sudo cp /usr/local/bin/cors-proxy.previous /usr/local/bin/cors-proxy
sudo systemctl start cors-proxy
```

Verify:

```bash
curl -fsS http://127.0.0.1:3000/health
sudo systemctl status cors-proxy --no-pager
```

A better release process is to keep versioned binaries such as:

```text
/usr/local/lib/cors-proxy/1.0.0/cors-proxy
/usr/local/lib/cors-proxy/1.1.0/cors-proxy
```

and make `/usr/local/bin/cors-proxy` point to the active release.

## 28. Troubleshooting

### Service will not start

```bash
sudo systemctl status cors-proxy --no-pager
sudo journalctl -u cors-proxy -n 200 --no-pager
```

Check configuration:

```bash
sudo cat /etc/cors-proxy.env
```

Check the binary:

```bash
/usr/local/bin/cors-proxy --help
```

The current service does not require a CLI argument parser, so environment configuration is authoritative.

### Port 3000 already in use

```bash
sudo ss -lntp | grep ':3000'
```

Either stop the conflicting process or change:

```env
LISTEN_ADDR=127.0.0.1:3001
```

and update Nginx's `proxy_pass` accordingly.

### Nginx returns 502

Check the application:

```bash
curl -i http://127.0.0.1:3000/health
```

If that fails, inspect:

```bash
sudo journalctl -u cors-proxy -n 100 --no-pager
```

Then validate Nginx:

```bash
sudo nginx -t
sudo tail -n 100 /var/log/nginx/error.log
```

### Playlist loads but segments fail

Inspect the rewritten playlist and browser network requests. Check that:

- The generated proxy URL is HTTPS when the page is HTTPS.
- Relative URLs resolve against the correct playlist URL.
- Required `Referer`/`Origin` values are supplied.
- The upstream accepts the proxy's User-Agent.
- The upstream is not resolving to a blocked private/special-use address.
- Nginx has streaming-friendly timeouts.

### Upstream returns 403

The origin may require specific HTTP headers. Supply only the required headers:

```text
referer
origin
user-agent
```

Use the `headers` parameter for additional legitimate upstream requirements.

### Upstream returns 404

Check whether the playlist uses relative paths. The proxy resolves them against the original playlist URL. Inspect the upstream playlist directly to determine whether the advertised resource exists.

### Upstream times out

Check network reachability from the VPS:

```bash
curl -I --max-time 15 https://example.com/
```

Then inspect the proxy's logs. Increase `UPSTREAM_TIMEOUT` only when slow upstream behavior is expected.

### Browser reports CORS errors

Confirm the response includes:

```text
Access-Control-Allow-Origin: *
Access-Control-Allow-Methods: GET, HEAD, OPTIONS
```

Also confirm Nginx is not stripping response headers.

## 29. Performance tuning

Start with conservative values:

```env
MAX_CONCURRENT_UPSTREAM=256
UPSTREAM_TIMEOUT=10
MAX_PLAYLIST_BYTES=4194304
```

Increase concurrency only after measuring CPU, memory, file descriptors, bandwidth, and upstream behavior.

For media-heavy traffic:

- Keep Nginx buffering disabled.
- Use long read/send timeouts.
- Avoid unnecessary application-level buffering.
- Keep connection pooling enabled.
- Prefer HTTP/2 where appropriate.
- Monitor network throughput before increasing CPU/RAM.

The proxy is intentionally not an HLS transcoder. It should relay bytes with as little processing as possible except where playlist URLs need rewriting.

## 30. Operational checklist

Before going live:

- [ ] DNS points to the VPS.
- [ ] SSH is secured.
- [ ] Firewall allows only required public ports.
- [ ] Rust proxy runs as a non-root user.
- [ ] Proxy listens on `127.0.0.1` behind Nginx.
- [ ] `ALLOW_PRIVATE_IPS=false`.
- [ ] Nginx buffering is disabled.
- [ ] Nginx streaming timeouts are configured.
- [ ] HTTPS is active.
- [ ] Certificate renewal test succeeds.
- [ ] `/health` returns 200.
- [ ] HLS playlist rewriting works.
- [ ] Relative resources work.
- [ ] Range requests work.
- [ ] Referer/Origin forwarding works.
- [ ] Upstream errors are visible in logs.
- [ ] Rate limiting/authentication is considered for public deployment.
- [ ] Backups/rollback binaries exist.
- [ ] Monitoring and bandwidth alerts are configured.

## 31. Production request flow

```text
                         INTERNET
                            │
                     HTTPS :443
                            │
                            ▼
                    ┌───────────────┐
                    │     Nginx     │
                    │ TLS + timeout │
                    │ no buffering  │
                    └───────┬───────┘
                            │
                    HTTP localhost
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
                    Reqwest / HTTPS
                            │
                            ▼
                  ┌───────────────────┐
                  │  Upstream Origin  │
                  │                   │
                  │ .m3u8 / segments  │
                  │ keys / resources  │
                  └───────────────────┘
```

The browser-facing URL remains HTTPS, while the Rust proxy can make server-side HTTP or HTTPS requests to the upstream according to the target URL.
