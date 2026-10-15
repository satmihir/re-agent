package reagent

import (
	"encoding/json"
	"net/url"
	"strings"
)

// Redact URL userinfo, not the password alone: short passwords may be ordinary
// provider text. Match the authority prefix even when the echoed path differs.
func redactEndpointText(text, endpoint string) string {
	u, err := url.Parse(endpoint)
	if err != nil || u.User == nil {
		return text
	}
	if _, ok := u.User.Password(); !ok {
		return text
	}
	prefix := u.Scheme + "://" + u.User.String() + "@"
	masked := u.Scheme + "://" + url.UserPassword(u.User.Username(), "xxxxx").String() + "@"
	text = strings.ReplaceAll(text, prefix, masked)
	// URL parsing normalizes percent escapes; also cover the configured spelling.
	if start := strings.Index(endpoint, "://"); start >= 0 {
		start += 3
		authority := strings.SplitN(endpoint[start:], "/", 2)[0]
		if at := strings.LastIndex(authority, "@"); at >= 0 {
			text = strings.ReplaceAll(text, endpoint[:start]+authority[:at+1], masked)
		}
	}
	return text
}

// Decode only string tokens, so escaped URLs are masked without re-encoding
// native objects, changing number spellings, or disturbing SSE framing.
func redactEndpointBody(body, endpoint string) string {
	if redacted(endpoint) == endpoint {
		return body
	}
	body = redactEndpointText(body, endpoint)
	var out strings.Builder
	copied := 0
	for i := 0; i < len(body); i++ {
		if body[i] != '"' {
			continue
		}
		start := i
		for i++; i < len(body) && body[i] != '"'; i++ {
			if body[i] == '\\' {
				i++
			}
		}
		if i >= len(body) {
			break
		}
		var text string
		if json.Unmarshal([]byte(body[start:i+1]), &text) != nil {
			continue
		}
		masked := redactEndpointText(text, endpoint)
		if masked == text {
			continue
		}
		encoded, _ := json.Marshal(masked)
		out.WriteString(body[copied:start])
		out.Write(encoded)
		copied = i + 1
	}
	if copied == 0 {
		return body
	}
	out.WriteString(body[copied:])
	return out.String()
}
