package proxy

import (
	"fmt"
	"net"
	"os"
	"strconv"
	"strings"
	"time"
)

type Config struct {
	ListenAddr            string
	UpstreamTimeout       time.Duration
	StallTimeout          time.Duration
	MaxPlaylistBytes      int64
	MaxURLLength          int
	MaxHeaderJSONLength   int
	MaxConcurrentUpstream int
	UserAgent             string
	AllowPrivateIPs       bool
	AllowInsecureTLS      bool
	TrustProxyHeaders     bool
	ProxyPath             string
}

func ConfigFromEnv() (Config, error) {
	c := Config{
		ListenAddr:            envString("LISTEN_ADDR", ":3000"),
		UpstreamTimeout:       envDuration("UPSTREAM_TIMEOUT", 10*time.Second),
		StallTimeout:          envDuration("STALL_TIMEOUT", 15*time.Second),
		MaxPlaylistBytes:      envInt64("MAX_PLAYLIST_BYTES", 4<<20),
		MaxURLLength:          envInt("MAX_URL_LENGTH", 8192),
		MaxHeaderJSONLength:   envInt("MAX_HEADER_JSON_LENGTH", 8192),
		MaxConcurrentUpstream: envInt("MAX_CONCURRENT_UPSTREAM", 256),
		UserAgent:             envString("USER_AGENT", "m3u8-proxy/1.0"),
		AllowPrivateIPs:       envBool("ALLOW_PRIVATE_IPS", false),
		AllowInsecureTLS:      envBool("ALLOW_INSECURE_TLS", false),
		TrustProxyHeaders:     envBool("TRUST_PROXY_HEADERS", false),
		ProxyPath:             envString("PROXY_PATH", "/m3u8-proxy"),
	}
	if !strings.HasPrefix(c.ProxyPath, "/") || strings.Contains(c.ProxyPath, "?") || strings.Contains(c.ProxyPath, "#") {
		return c, fmt.Errorf("PROXY_PATH must be a clean absolute URL path")
	}
	if c.MaxConcurrentUpstream < 1 {
		return c, fmt.Errorf("MAX_CONCURRENT_UPSTREAM must be >= 1")
	}
	return c, nil
}

func envString(key, fallback string) string {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		return v
	}
	return fallback
}
func envInt(key string, fallback int) int {
	v, err := strconv.Atoi(os.Getenv(key))
	if err != nil || v == 0 {
		return fallback
	}
	return v
}
func envInt64(key string, fallback int64) int64 {
	v, err := strconv.ParseInt(os.Getenv(key), 10, 64)
	if err != nil || v == 0 {
		return fallback
	}
	return v
}
func envDuration(key string, fallback time.Duration) time.Duration {
	v := strings.TrimSpace(os.Getenv(key))
	if v == "" {
		return fallback
	}
	d, err := time.ParseDuration(v)
	if err != nil || d <= 0 {
		return fallback
	}
	return d
}
func envBool(key string, fallback bool) bool {
	v := strings.TrimSpace(os.Getenv(key))
	if v == "" {
		return fallback
	}
	b, err := strconv.ParseBool(v)
	if err != nil {
		return fallback
	}
	return b
}

func isPrivateIP(ip net.IP) bool {
	if ip.IsLoopback() || ip.IsPrivate() || ip.IsLinkLocalUnicast() || ip.IsLinkLocalMulticast() || ip.IsUnspecified() || ip.IsMulticast() {
		return true
	}
	if ip4 := ip.To4(); ip4 != nil {
		// IPv4-mapped and special-use ranges not covered by net.IP.IsPrivate.
		if ip4[0] == 0 || ip4[0] == 127 || ip4[0] == 169 && ip4[1] == 254 {
			return true
		}
		if ip4[0] == 100 && ip4[1] >= 64 && ip4[1] <= 127 {
			return true
		}
	}
	return false
}
