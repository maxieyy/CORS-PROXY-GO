package proxy

import (
	"context"
	"crypto/tls"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/url"
	"path"
	"regexp"
	"strings"
	"sync/atomic"
	"time"
)

var imageRE = regexp.MustCompile(`(?i)\.(png|jpg|jpeg|gif|svg|avif|webp)$`)

type Handler struct {
	cfg       Config
	client    *http.Client
	semaphore chan struct{}
	active    atomic.Int64
}

func NewHandler(cfg Config) *Handler {
	tr := &http.Transport{
		Proxy:                 http.ProxyFromEnvironment,
		DialContext:           (&net.Dialer{Timeout: 10 * time.Second, KeepAlive: 30 * time.Second}).DialContext,
		ForceAttemptHTTP2:     true,
		MaxIdleConns:          128,
		MaxIdleConnsPerHost:   32,
		MaxConnsPerHost:       64,
		IdleConnTimeout:       30 * time.Second,
		TLSHandshakeTimeout:   10 * time.Second,
		ExpectContinueTimeout: 1 * time.Second,
	}
	if cfg.AllowInsecureTLS {
		tr.TLSClientConfig = &tls.Config{InsecureSkipVerify: true, MinVersion: tls.VersionTLS12} // #nosec G402 -- explicitly opt-in for problematic upstreams.
	} else {
		tr.TLSClientConfig = &tls.Config{MinVersion: tls.VersionTLS12}
	}
	return &Handler{cfg: cfg, client: &http.Client{Transport: tr}, semaphore: make(chan struct{}, cfg.MaxConcurrentUpstream)}
}

func (h *Handler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.URL.Path == "/health" {
		writeJSON(w, http.StatusOK, map[string]any{"status": "ok", "active_upstreams": h.active.Load()})
		return
	}
	if r.URL.Path != h.cfg.ProxyPath {
		http.NotFound(w, r)
		return
	}
	if r.Method == http.MethodOptions {
		h.options(w)
		return
	}
	if r.Method != http.MethodGet && r.Method != http.MethodHead {
		w.Header().Set("Allow", "GET, HEAD, OPTIONS")
		writeJSON(w, http.StatusMethodNotAllowed, map[string]string{"message": "method not allowed"})
		return
	}

	raw := r.URL.Query().Get("url")
	if raw == "" || len(raw) > h.cfg.MaxURLLength {
		writeJSON(w, 400, map[string]string{"message": "invalid or missing `url` query parameter"})
		return
	}
	upstream, err := url.Parse(raw)
	if err != nil || (upstream.Scheme != "http" && upstream.Scheme != "https") || upstream.Host == "" || upstream.User != nil {
		writeJSON(w, 400, map[string]string{"message": "url must be an absolute http or https URL"})
		return
	}
	if err := h.validateUpstream(r.Context(), upstream); err != nil {
		writeJSON(w, 403, map[string]string{"message": err.Error()})
		return
	}

	headers, err := parseHeaders(r.URL.Query().Get("headers"), h.cfg)
	if err != nil {
		writeJSON(w, 400, map[string]string{"message": err.Error()})
		return
	}

	filename := sanitizeFilename(r.URL.Query().Get("filename"))
	isPlaylist := isPlaylistURL(upstream)
	reqHeaders := cloneHeaders(headers)
	if rangeValue := r.Header.Get("Range"); rangeValue != "" {
		reqHeaders.Set("Range", rangeValue)
	}

	if !h.acquire(w) {
		return
	}
	defer h.release()

	reqCtx, cancel := context.WithTimeout(r.Context(), h.cfg.UpstreamTimeout)
	defer cancel()
	req, err := http.NewRequestWithContext(reqCtx, r.Method, upstream.String(), nil)
	if err != nil {
		writeJSON(w, 400, map[string]string{"message": "unable to create upstream request"})
		return
	}
	req.Header = reqHeaders

	resp, err := h.client.Do(req)
	if err != nil {
		status := http.StatusBadGateway
		if errors.Is(reqCtx.Err(), context.DeadlineExceeded) {
			status = http.StatusGatewayTimeout
		}
		writeJSON(w, status, map[string]string{"message": "upstream request failed", "error": err.Error()})
		return
	}
	defer resp.Body.Close()

	if isPlaylist {
		h.servePlaylist(w, r, resp, upstream, headers, filename)
		return
	}
	h.serveStream(w, r, resp, filename)
}

func (h *Handler) options(w http.ResponseWriter) {
	w.Header().Set("Access-Control-Allow-Origin", "*")
	w.Header().Set("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS")
	w.Header().Set("Access-Control-Allow-Headers", "Range, Content-Type, *")
	w.Header().Set("Access-Control-Expose-Headers", "Content-Length, Content-Range, Accept-Ranges, ETag")
	w.WriteHeader(http.StatusNoContent)
}

func (h *Handler) acquire(w http.ResponseWriter) bool {
	select {
	case h.semaphore <- struct{}{}:
		h.active.Add(1)
		return true
	default:
		writeJSON(w, http.StatusServiceUnavailable, map[string]string{"message": "proxy concurrency limit reached"})
		return false
	}
}
func (h *Handler) release() { <-h.semaphore; h.active.Add(-1) }

func (h *Handler) serveStream(w http.ResponseWriter, r *http.Request, resp *http.Response, filename string) {
	copyResponseHeaders(w, resp, filename)
	w.WriteHeader(resp.StatusCode)
	if r.Method == http.MethodHead {
		return
	}
	if err := streamWithStallTimeout(r.Context(), w, resp.Body, h.cfg.StallTimeout); err != nil && !errors.Is(err, context.Canceled) {
		slog.Debug("upstream stream ended", "error", err)
	}
}

func (h *Handler) servePlaylist(w http.ResponseWriter, r *http.Request, resp *http.Response, upstream *url.URL, headers http.Header, filename string) {
	if resp.StatusCode < 200 || resp.StatusCode > 299 {
		copyResponseHeaders(w, resp, filename)
		w.WriteHeader(resp.StatusCode)
		if r.Method != http.MethodHead {
			_, _ = io.Copy(w, io.LimitReader(resp.Body, 64<<10))
		}
		return
	}
	body, err := io.ReadAll(io.LimitReader(resp.Body, h.cfg.MaxPlaylistBytes+1))
	if err != nil {
		writeJSON(w, 502, map[string]string{"message": "failed reading upstream playlist", "error": err.Error()})
		return
	}
	if int64(len(body)) > h.cfg.MaxPlaylistBytes {
		writeJSON(w, 502, map[string]string{"message": "upstream playlist exceeds configured size limit"})
		return
	}
	rewritten, err := h.rewritePlaylist(string(body), upstream, headers)
	if err != nil {
		writeJSON(w, 502, map[string]string{"message": "failed rewriting playlist", "error": err.Error()})
		return
	}
	w.Header().Set("Content-Type", "application/vnd.apple.mpegurl")
	w.Header().Set("Cache-Control", "no-cache, no-store, must-revalidate")
	w.Header().Set("Pragma", "no-cache")
	w.Header().Set("X-Content-Type-Options", "nosniff")
	setCORS(w)
	if filename != "" {
		w.Header().Set("Content-Disposition", contentDisposition(filename))
	}
	w.Header().Set("Content-Length", fmt.Sprintf("%d", len(rewritten)))
	w.WriteHeader(resp.StatusCode)
	if r.Method != http.MethodHead {
		_, _ = io.WriteString(w, rewritten)
	}
}

func (h *Handler) rewritePlaylist(text string, base *url.URL, headers http.Header) (string, error) {
	var b strings.Builder
	proxyBase := h.proxyURLBase(headers)
	lines := strings.Split(strings.ReplaceAll(text, "\r\n", "\n"), "\n")
	for _, line := range lines {
		if strings.HasPrefix(line, "#EXT-X-KEY:") || strings.HasPrefix(line, "#EXT-X-MAP:") || strings.HasPrefix(line, "#EXT-X-MEDIA:") || strings.HasPrefix(line, "#EXT-X-I-FRAME-STREAM-INF:") {
			rewritten, err := rewriteURIAttribute(line, base, proxyBase, headersToQuery(headers))
			if err != nil {
				return "", err
			}
			b.WriteString(rewritten)
		} else if strings.HasPrefix(line, "#") || strings.TrimSpace(line) == "" {
			b.WriteString(line)
		} else {
			target, err := resolveURL(base, strings.TrimSpace(line))
			if err != nil {
				return "", err
			}
			b.WriteString(proxyBase)
			b.WriteString(url.QueryEscape(target.String()))
			if h := headersToQuery(headers); h != "" {
				b.WriteString("&headers=")
				b.WriteString(url.QueryEscape(h))
			}
		}
		b.WriteByte('\n')
	}
	return b.String(), nil
}

func rewriteURIAttribute(line string, base *url.URL, proxyBase, headersJSON string) (string, error) {
	idx := strings.Index(line, "URI=")
	if idx < 0 {
		return line, nil
	}
	rest := line[idx+4:]
	if len(rest) == 0 {
		return line, nil
	}
	quote := rest[0]
	if quote != '"' && quote != '\'' {
		return line, nil
	}
	end := strings.IndexByte(rest[1:], quote)
	if end < 0 {
		return line, nil
	}
	end++
	raw := rest[1:end]
	target, err := resolveURL(base, raw)
	if err != nil {
		return "", err
	}
	replacement := proxyBase + url.QueryEscape(target.String())
	if headersJSON != "" {
		replacement += "&headers=" + url.QueryEscape(headersJSON)
	}
	return line[:idx+5] + replacement + line[idx+4+end:], nil
}

func resolveURL(base *url.URL, raw string) (*url.URL, error) {
	u, err := url.Parse(raw)
	if err != nil {
		return nil, err
	}
	if u.Scheme != "" && u.Scheme != "http" && u.Scheme != "https" {
		return nil, fmt.Errorf("unsupported resource URL scheme: %q", u.Scheme)
	}
	return base.ResolveReference(u), nil
}

func (h *Handler) proxyURLBase(headers http.Header) string {
	// Relative proxy URLs keep the playlist portable across HTTP/HTTPS frontends and avoid mixed content.
	return h.cfg.ProxyPath + "?url="
}

func headersToQuery(headers http.Header) string {
	if len(headers) == 0 {
		return ""
	}
	m := map[string]string{}
	for k, values := range headers {
		if len(values) > 0 {
			m[k] = values[0]
		}
	}
	raw, _ := json.Marshal(m)
	return string(raw)
}

func parseHeaders(raw string, cfg Config) (http.Header, error) {
	h := make(http.Header)
	h.Set("User-Agent", cfg.UserAgent)
	if raw == "" {
		return h, nil
	}
	decoded, err := url.QueryUnescape(raw)
	if err != nil {
		return nil, fmt.Errorf("invalid headers parameter")
	}
	if len(decoded) > cfg.MaxHeaderJSONLength {
		return nil, fmt.Errorf("headers parameter is too large")
	}
	var values map[string]any
	if err := json.Unmarshal([]byte(decoded), &values); err != nil {
		return nil, fmt.Errorf("headers must be valid JSON")
	}
	blocked := map[string]bool{"host": true, "connection": true, "content-length": true, "transfer-encoding": true, "access-control-allow-origin": true, "access-control-allow-methods": true, "access-control-allow-headers": true}
	for k, v := range values {
		if blocked[strings.ToLower(k)] {
			continue
		}
		s, ok := v.(string)
		if !ok || len(k) > 128 || len(s) > 4096 {
			continue
		}
		h.Set(k, s)
	}
	return h, nil
}

func cloneHeaders(in http.Header) http.Header {
	out := make(http.Header, len(in))
	for k, vv := range in {
		for _, v := range vv {
			out.Add(k, v)
		}
	}
	return out
}

func copyResponseHeaders(w http.ResponseWriter, resp *http.Response, filename string) {
	for _, key := range []string{"Content-Type", "Content-Length", "Content-Range", "Accept-Ranges", "Cache-Control", "ETag", "Expires", "Last-Modified"} {
		if v := resp.Header.Values(key); len(v) > 0 {
			w.Header()[key] = append([]string(nil), v...)
		}
	}
	if filename != "" {
		w.Header().Set("Content-Disposition", contentDisposition(filename))
	}
	w.Header().Set("X-Content-Type-Options", "nosniff")
	setCORS(w)
	w.Header().Del("Content-Encoding")
}
func setCORS(w http.ResponseWriter) {
	w.Header().Set("Access-Control-Allow-Origin", "*")
	w.Header().Set("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS")
	w.Header().Set("Access-Control-Allow-Headers", "Range, Content-Type, *")
	w.Header().Set("Access-Control-Expose-Headers", "Content-Length, Content-Range, Accept-Ranges, ETag")
}
func contentDisposition(filename string) string {
	return `inline; filename="` + strings.ReplaceAll(filename, `"`, "_") + `"`
}
func sanitizeFilename(v string) string {
	v = path.Base(strings.TrimSpace(v))
	if v == "." || v == "/" || v == "" || len(v) > 180 {
		return ""
	}
	return strings.Map(func(r rune) rune {
		if r < 32 || strings.ContainsRune(`/\\?%*:|"<>`, r) {
			return '_'
		}
		return r
	}, v)
}
func isPlaylistURL(u *url.URL) bool {
	p := strings.ToLower(u.Path)
	return strings.HasSuffix(p, ".m3u8") || strings.HasSuffix(p, ".txt")
}

func (h *Handler) validateUpstream(ctx context.Context, u *url.URL) error {
	host := u.Hostname()
	if host == "" {
		return fmt.Errorf("upstream host is missing")
	}
	if h.cfg.AllowPrivateIPs {
		return nil
	}
	if ip := net.ParseIP(host); ip != nil {
		if isPrivateIP(ip) {
			return fmt.Errorf("private or special-use upstream addresses are blocked")
		}
		return nil
	}
	ips, err := net.DefaultResolver.LookupIPAddr(ctx, host)
	if err != nil {
		return fmt.Errorf("unable to resolve upstream host")
	}
	if len(ips) == 0 {
		return fmt.Errorf("upstream host has no addresses")
	}
	for _, addr := range ips {
		if isPrivateIP(addr.IP) {
			return fmt.Errorf("upstream host resolves to a private or special-use address")
		}
	}
	return nil
}

func streamWithStallTimeout(ctx context.Context, dst http.ResponseWriter, src io.Reader, stall time.Duration) error {
	flusher, _ := dst.(http.Flusher)
	buf := make([]byte, 32*1024)
	for {
		timer := time.NewTimer(stall)
		type result struct {
			n   int
			err error
		}
		ch := make(chan result, 1)
		go func() { n, err := src.Read(buf); ch <- result{n, err} }()
		select {
		case <-ctx.Done():
			timer.Stop()
			return ctx.Err()
		case <-timer.C:
			return fmt.Errorf("upstream data stalled")
		case res := <-ch:
			timer.Stop()
			if res.n > 0 {
				if _, err := dst.Write(buf[:res.n]); err != nil {
					return err
				}
				if flusher != nil {
					flusher.Flush()
				}
			}
			if res.err != nil {
				if errors.Is(res.err, io.EOF) {
					return nil
				}
				return res.err
			}
		}
	}
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	setCORS(w)
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}
