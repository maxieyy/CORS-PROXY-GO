# Go M3U8 Proxy — Production Deployment

This guide deploys the Go proxy on a small Linux VPS and exposes it as:

```text
https://example.com/m3u8-proxy
```

The upstream stream may still be HTTP:

```text
http://example.com/iptv/MCHUP9AS7DBP5W/7342/index.m3u8
```

The browser talks to your HTTPS proxy, while the VPS talks to the IPTV origin. This is the mechanism that avoids browser mixed-content blocking.

## 1. Prerequisites

You need:

- A Debian/Ubuntu VPS.
- A domain such as `example.com`.
- DNS control for that domain.
- SSH access.
- Ports 22, 80, and 443 available.

Recommended minimum for a lightweight proxy: 1 vCPU, 1 GB RAM. Bandwidth is normally the important scaling resource, not RAM.

## 2. SSH into the VPS

From Windows PowerShell, macOS, or Linux:

```bash
ssh root@YOUR_SERVER_IP
```

For a non-root account:

```bash
ssh deploy@YOUR_SERVER_IP
```

Confirm the server:

```bash
cat /etc/os-release
uname -a
```

## 3. Update the operating system

Debian/Ubuntu:

```bash
apt update && apt upgrade -y
apt install -y curl ca-certificates git unzip nginx ufw build-essential
```

## 4. Create a deployment user

If you currently use root, create a dedicated user:

```bash
adduser --disabled-password --gecos "" deploy
usermod -aG sudo deploy
```

Copy your SSH key if needed:

```bash
mkdir -p /home/deploy/.ssh
cp /root/.ssh/authorized_keys /home/deploy/.ssh/authorized_keys
chown -R deploy:deploy /home/deploy/.ssh
chmod 700 /home/deploy/.ssh
chmod 600 /home/deploy/.ssh/authorized_keys
```

Then open a second terminal and verify:

```bash
ssh deploy@YOUR_SERVER_IP
```

Do not disable root SSH or password authentication until key-based login works.

## 5. Firewall

Allow SSH, HTTP, and HTTPS:

```bash
sudo ufw allow OpenSSH
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw enable
sudo ufw status verbose
```

Do NOT expose port 3000 publicly. The Go service should listen on `127.0.0.1:3000` and Nginx should be the public entry point.

## 6. Configure DNS

At your DNS provider create:

```text
Type: A
Name: @
Value: YOUR_SERVER_IP
```

For a subdomain instead:

```text
Type: A
Name: proxy
Value: YOUR_SERVER_IP
```

Wait for DNS propagation and verify from the VPS or another machine:

```bash
getent hosts example.com
```

The returned address must be your VPS IP.

## 7. Install Go

Use the official Go release for your architecture. Check the architecture first:

```bash
uname -m
```

For x86_64, download the current amd64 Linux archive from:

```text
https://go.dev/dl/
```

Example installation pattern:

```bash
cd /tmp
curl -LO https://go.dev/dl/go1.XX.X.linux-amd64.tar.gz
sudo rm -rf /usr/local/go
sudo tar -C /usr/local -xzf go1.XX.X.linux-amd64.tar.gz
```

Replace `go1.XX.X` with the exact current version shown on go.dev.

Add Go to PATH:

```bash
echo 'export PATH=/usr/local/go/bin:$PATH' | sudo tee /etc/profile.d/go.sh
source /etc/profile.d/go.sh
```

Verify:

```bash
go version
```

## 8. Create the application directory

```bash
sudo mkdir -p /opt/m3u8-proxy
sudo chown -R deploy:deploy /opt/m3u8-proxy
```

Switch to the deployment user:

```bash
sudo -iu deploy
cd /opt/m3u8-proxy
```

## 9. Upload or clone the repository

From GitHub:

```bash
git clone YOUR_GITHUB_REPOSITORY_URL .
```

Or copy your repository from your workstation with SCP:

```bash
scp -r ./m3u8-proxy-go/* deploy@YOUR_SERVER_IP:/opt/m3u8-proxy/
```

Confirm:

```bash
find /opt/m3u8-proxy -maxdepth 3 -type f | sort
```

## 10. Configure environment

Copy the template:

```bash
cd /opt/m3u8-proxy
cp .env.example .env
nano .env
```

Recommended small-VPS configuration:

```env
LISTEN_ADDR=127.0.0.1:3000
UPSTREAM_TIMEOUT=10s
STALL_TIMEOUT=15s
MAX_PLAYLIST_BYTES=4194304
MAX_URL_LENGTH=8192
MAX_HEADER_JSON_LENGTH=8192
MAX_CONCURRENT_UPSTREAM=256
USER_AGENT=m3u8-proxy/1.0
ALLOW_PRIVATE_IPS=false
ALLOW_INSECURE_TLS=false
TRUST_PROXY_HEADERS=false
PROXY_PATH=/m3u8-proxy
```

Keep `ALLOW_PRIVATE_IPS=false` unless you intentionally need internal upstreams.

## 11. Format, test, and build

```bash
cd /opt/m3u8-proxy
go mod tidy
gofmt -w ./cmd ./internal
go test ./...
go vet ./...
go build -trimpath -ldflags="-s -w" -o m3u8-proxy ./cmd/m3u8-proxy
```

Check the binary:

```bash
file ./m3u8-proxy
./m3u8-proxy
```

Stop it with `Ctrl+C` after confirming it starts.

## 12. Run locally on the VPS

```bash
cd /opt/m3u8-proxy
./m3u8-proxy
```

In a second SSH session:

```bash
curl -i http://127.0.0.1:3000/health
```

Expected status:

```text
HTTP/1.1 200 OK
```

Stop the foreground process with `Ctrl+C`.

## 13. Create a dedicated systemd service

As root or with sudo:

```bash
sudo nano /etc/systemd/system/m3u8-proxy.service
```

Use:

```ini
[Unit]
Description=Go M3U8 Proxy
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=deploy
Group=deploy
WorkingDirectory=/opt/m3u8-proxy
EnvironmentFile=/opt/m3u8-proxy/.env
ExecStart=/opt/m3u8-proxy/m3u8-proxy
Restart=always
RestartSec=2

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ProtectControlGroups=true
ProtectKernelModules=true
ProtectKernelTunables=true
RestrictSUIDSGID=true
LockPersonality=true
RestrictRealtime=true

[Install]
WantedBy=multi-user.target
```

Reload and enable:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now m3u8-proxy
```

Check:

```bash
sudo systemctl status m3u8-proxy --no-pager
```

Logs:

```bash
sudo journalctl -u m3u8-proxy -f
```

Health check:

```bash
curl http://127.0.0.1:3000/health
```

## 14. Configure Nginx

Remove the default site if you do not need it:

```bash
sudo rm -f /etc/nginx/sites-enabled/default
```

Create:

```bash
sudo nano /etc/nginx/sites-available/example.com
```

Use:

```nginx
server {
    listen 80;
    listen [::]:80;

    server_name example.com www.example.com;

    location / {
        proxy_pass http://127.0.0.1:3000;
        proxy_http_version 1.1;

        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        proxy_buffering off;
        proxy_request_buffering off;
        proxy_read_timeout 60s;
        proxy_send_timeout 60s;

        # Streaming responses may be large.
        client_max_body_size 0;
    }
}
```

Enable it:

```bash
sudo ln -s /etc/nginx/sites-available/example.com /etc/nginx/sites-enabled/example.com
sudo nginx -t
sudo systemctl reload nginx
```

Test HTTP:

```bash
curl -I http://example.com/health
```

## 15. Install SSL with Let's Encrypt

Install Certbot:

```bash
sudo apt install -y certbot python3-certbot-nginx
```

Request the certificate:

```bash
sudo certbot --nginx -d example.com -d www.example.com
```

Choose the option that redirects HTTP to HTTPS when Certbot offers it.

Verify:

```bash
sudo nginx -t
sudo systemctl reload nginx
```

Test:

```bash
curl -I https://example.com/health
```

You should receive `200 OK`.

## 16. Test an HTTP IPTV origin through HTTPS

Example source:

```text
http://example.com/iptv/MCHUP9AS7DBP5W/7342/index.m3u8
```

URL-encode it when calling the proxy:

```bash
curl -G 'https://example.com/m3u8-proxy' \
  --data-urlencode 'url=http://example.com/iptv/MCHUP9AS7DBP5W/7342/index.m3u8'
```

The returned playlist should contain proxy URLs rather than direct origin URLs.

The browser flow becomes:

```text
HTTPS browser
      |
      v
https://example.com/m3u8-proxy?url=<HTTP upstream>
      |
      | server-side HTTP/HTTPS
      v
IPTV origin
```

This prevents the browser from directly loading the HTTP origin and therefore avoids mixed-content blocking.

## 17. Test a real player

With hls.js, use the HTTPS proxy URL as the source:

```js
const upstream = 'http://example.com/iptv/MCHUP9AS7DBP5W/7342/index.m3u8';
const proxy = `/m3u8-proxy?url=${encodeURIComponent(upstream)}`;
hls.loadSource(proxy);
```

Do not give hls.js the raw HTTP IPTV URL when your page is HTTPS.

## 18. Optional upstream headers

Some origins require a custom `Referer` or `User-Agent`.

Example JSON:

```json
{"Referer":"https://origin.example/","User-Agent":"Mozilla/5.0"}
```

Encode it as the `headers` query parameter:

```bash
curl -G 'https://example.com/m3u8-proxy' \
  --data-urlencode 'url=http://example.com/live/index.m3u8' \
  --data-urlencode 'headers={"Referer":"https://origin.example/","User-Agent":"Mozilla/5.0"}'
```

Do not forward `Host`, `Connection`, `Content-Length`, CORS response-control headers, or other hop-by-hop headers through this parameter.

## 19. Range requests

The proxy forwards the viewer's `Range` request to the upstream. This matters for MP4 and other resources that require byte ranges.

Test:

```bash
curl -i -H 'Range: bytes=0-1023' \
  'https://example.com/m3u8-proxy?url=https%3A%2F%2Fexample.com%2Fvideo.mp4'
```

A supporting origin should return `206 Partial Content` and a `Content-Range` header.

## 20. Service management

Restart after a new build:

```bash
sudo systemctl restart m3u8-proxy
```

Status:

```bash
sudo systemctl status m3u8-proxy --no-pager
```

Logs:

```bash
sudo journalctl -u m3u8-proxy --since '15 minutes ago' --no-pager
```

Follow logs:

```bash
sudo journalctl -u m3u8-proxy -f
```

Nginx logs:

```bash
sudo tail -f /var/log/nginx/access.log /var/log/nginx/error.log
```

## 21. Updating the proxy

As `deploy`:

```bash
cd /opt/m3u8-proxy
git pull

go mod tidy
go test ./...
go vet ./...
go build -trimpath -ldflags="-s -w" -o m3u8-proxy.new ./cmd/m3u8-proxy
mv m3u8-proxy.new m3u8-proxy
```

Restart:

```bash
sudo systemctl restart m3u8-proxy
```

Verify:

```bash
curl -fsS https://example.com/health
sudo systemctl status m3u8-proxy --no-pager
```

## 22. Troubleshooting

### `502 Upstream request failed`

Check:

```bash
sudo journalctl -u m3u8-proxy -n 100 --no-pager
```

Then test the origin from the VPS itself:

```bash
curl -I 'http://origin.example/index.m3u8'
```

Possible causes include DNS failure, origin blocking your VPS IP, an invalid TLS certificate, or an unreachable origin.

### `403 private or special-use upstream addresses are blocked`

This is the built-in SSRF protection. Only set:

```env
ALLOW_PRIVATE_IPS=true
```

when you intentionally want to proxy private/internal addresses and understand the security implications.

### Browser reports mixed content

Make sure the player loads:

```text
https://example.com/m3u8-proxy?url=<encoded HTTP URL>
```

and NOT:

```text
http://origin.example/index.m3u8
```

### Playlist loads but segments fail

Inspect the playlist returned by the proxy. Segment and key URLs should point back to the proxy. Also inspect browser Network requests for `403`, `404`, CORS, or upstream failures.

### HTTPS upstream has certificate problems

Keep:

```env
ALLOW_INSECURE_TLS=false
```

as the normal production setting. Only enable it when you deliberately need to connect to an upstream with an invalid/self-signed certificate.

### Streams stall

Check:

- VPS network throughput.
- Upstream response time.
- Nginx timeouts.
- `UPSTREAM_TIMEOUT` and `STALL_TIMEOUT`.
- Number of concurrent viewers.
- Whether the IPTV source itself is unstable.

## 23. Production security checklist

- [ ] SSH key authentication works.
- [ ] UFW allows only required public ports.
- [ ] Go listens on `127.0.0.1:3000`.
- [ ] Port 3000 is not exposed by the firewall/provider.
- [ ] HTTPS is enabled and HTTP redirects to HTTPS.
- [ ] `ALLOW_PRIVATE_IPS=false` unless intentionally required.
- [ ] `ALLOW_INSECURE_TLS=false` unless intentionally required.
- [ ] You have rate limiting/authentication or an upstream allowlist if the proxy is not strictly private.
- [ ] Proxy bandwidth is monitored.
- [ ] systemd is enabled and restarting the process.
- [ ] `/health` returns `200`.
- [ ] Playlist requests rewrite segment/key URLs correctly.
- [ ] Browser requests use the HTTPS proxy URL.

## 24. Architecture

```text
                         HTTPS
Browser / hls.js ----------------------> Nginx :443
                                           |
                                           | HTTP localhost
                                           v
                                   Go M3U8 Proxy :3000
                                           |
                         +-----------------+-----------------+
                         |                                   |
                    HTTP origin                        HTTPS origin
                         |                                   |
                         +-----------------+-----------------+
                                           |
                                      .m3u8 / .ts
                                      .key / .mp4
```

The Go process is intentionally small and stateless. Horizontal scaling can be added by putting multiple instances behind a load balancer when bandwidth requirements justify it.
