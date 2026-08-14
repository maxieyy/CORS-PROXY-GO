# Production Deployment — Rust CORS Proxy

This service is a standalone Rust HTTP/HTTPS proxy for HLS playlists and media resources.

## 1. Build requirements

Recommended VPS: Debian 12/Ubuntu 24.04, 1+ vCPU and 512 MB+ RAM.

Install Rust from the official Rust toolchain installer, then:

```bash
cargo build --release
```

The binary is:

```text
target/release/cors-proxy
```

## 2. Environment

Create `/etc/cors-proxy.env`:

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

Keep `ALLOW_PRIVATE_IPS=false` unless access to internal addresses is an explicit requirement.

## 3. systemd

Install the binary:

```bash
sudo install -m 0755 target/release/cors-proxy /usr/local/bin/cors-proxy
sudo useradd --system --no-create-home --shell /usr/sbin/nologin corsproxy || true
sudo chown root:root /usr/local/bin/cors-proxy
```

Create `/etc/systemd/system/cors-proxy.service`:

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

Enable it:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now cors-proxy
sudo systemctl status cors-proxy
```

Health check:

```bash
curl http://127.0.0.1:3000/health
```

## 4. Nginx + HTTPS

Create a DNS `A`/`AAAA` record pointing your proxy hostname to the VPS.

Install Nginx and Certbot, then use a server block similar to:

```nginx
server {
    listen 443 ssl http2;
    server_name proxy.example.com;

    ssl_certificate /etc/letsencrypt/live/proxy.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/proxy.example.com/privkey.pem;

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

For streaming, keep proxy buffering disabled and use a long read timeout.

## 5. Docker

Build:

```bash
docker build -t cors-proxy .
```

Run:

```bash
docker run -d \
  --name cors-proxy \
  --restart unless-stopped \
  -p 127.0.0.1:3000:3000 \
  --env-file .env \
  cors-proxy
```

Put Nginx in front of the container for public TLS.

## 6. Security checklist

- Keep `ALLOW_PRIVATE_IPS=false`.
- Put authentication/rate limiting in front of a public unrestricted proxy.
- Keep the service bound to `127.0.0.1` when Nginx is the public entry point.
- Do not add automatic upstream redirect following without validating every redirect destination for SSRF.
- Do not expose arbitrary internal services through this proxy.
- Monitor CPU, memory, open connections, bandwidth, and upstream error rates.

## 7. HLS request example

```text
https://proxy.example.com/m3u8-proxy?url=https%3A%2F%2Fexample.com%2Flive%2Findex.m3u8&referer=https%3A%2F%2Fexample.com%2F&origin=https%3A%2F%2Fexample.com
```

`Referer` and `Origin` are converted into upstream HTTP request headers. They are not sent as part of the upstream target URL.
