package reagent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptrace"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"
)

const chatGPTReconnect = "Your ChatGPT connection is no longer valid; run reagent login."
const chatGPTResource = "https://api.openai.com/v1"
const chatGPTRefreshOutcomeUnknown = "refresh_outcome_unknown"

// testConfigPath is installed by TestMain before any credentials are accessed.
var testConfigPath func(string)

type chatGPTCredentials struct {
	HostID            string `json:"host_id"`
	ClientID          string `json:"client_id"`
	Subject           string `json:"sub"`
	Email             string `json:"email"`
	IDToken           string `json:"id_token,omitempty"`
	AccessToken       string `json:"access_token,omitempty"`
	RefreshToken      string `json:"refresh_token,omitempty"`
	ExpiresAt         int64  `json:"expires_at,omitempty"`
	EarliestRefreshAt int64  `json:"earliest_refresh_at,omitempty"`
	Scope             string `json:"scope,omitempty"`
	NeedsLogin        bool   `json:"needs_login,omitempty"`
	// A repeated logout must invalidate pending sign-ins even without active tokens.
	LogoutGeneration uint64 `json:"logout_generation,omitempty"`
}

func (c chatGPTCredentials) planGranted() bool {
	for _, scope := range strings.Fields(c.Scope) {
		if scope == "chatgpt.tokens.use.direct" {
			return true
		}
	}
	return false
}

func chatGPTCredentialPath() (string, error) {
	root, err := os.UserConfigDir()
	if err != nil {
		return "", fmt.Errorf("user configuration directory is unavailable")
	}
	path := filepath.Join(root, "reagent", "chatgpt.json")
	if testConfigPath != nil {
		testConfigPath(path)
	}
	return path, nil
}

func loadChatGPTCredentials() (chatGPTCredentials, error) {
	var c chatGPTCredentials
	path, err := chatGPTCredentialPath()
	if err != nil {
		return c, err
	}
	if _, err := os.Lstat(filepath.Dir(path)); os.IsNotExist(err) {
		return c, nil
	}
	if err := privateDirectory(filepath.Dir(path), false); err != nil {
		return c, fmt.Errorf("open ChatGPT credential directory: %w", err)
	}
	file, err := os.OpenFile(path, os.O_RDONLY|syscall.O_NOFOLLOW, 0)
	if os.IsNotExist(err) {
		return c, nil
	}
	if err != nil {
		return c, fmt.Errorf("open ChatGPT credentials: %s", stableError(err))
	}
	defer file.Close()
	info, err := file.Stat()
	if err != nil {
		return c, fmt.Errorf("inspect ChatGPT credentials: %s", stableError(err))
	}
	if !info.Mode().IsRegular() || info.Mode().Perm() != 0o600 {
		return c, fmt.Errorf("ChatGPT credentials must be a private regular file (0600)")
	}
	if err := json.NewDecoder(io.LimitReader(file, 1<<20)).Decode(&c); err != nil {
		return c, fmt.Errorf("ChatGPT credentials are corrupt; run reagent login")
	}
	return c, nil
}

func saveChatGPTCredentials(c chatGPTCredentials) error {
	path, err := chatGPTCredentialPath()
	if err != nil {
		return err
	}
	dir := filepath.Dir(path)
	if err := privateDirectory(dir, true); err != nil {
		return fmt.Errorf("open credential directory: %w", err)
	}
	file, err := os.CreateTemp(dir, ".chatgpt-*")
	if err != nil {
		return fmt.Errorf("stage ChatGPT credentials: %s", stableError(err))
	}
	defer os.Remove(file.Name())
	defer file.Close()
	if err := file.Chmod(0o600); err != nil {
		return fmt.Errorf("protect ChatGPT credentials: %s", stableError(err))
	}
	if err := json.NewEncoder(file).Encode(c); err != nil {
		return fmt.Errorf("write ChatGPT credentials: %s", stableError(err))
	}
	if err := file.Sync(); err != nil {
		return fmt.Errorf("sync ChatGPT credentials: %s", stableError(err))
	}
	if err := file.Close(); err != nil {
		return fmt.Errorf("close ChatGPT credentials: %s", stableError(err))
	}
	if err := os.Rename(file.Name(), path); err != nil {
		return fmt.Errorf("replace ChatGPT credentials: %s", stableError(err))
	}
	parent, err := os.Open(dir)
	if err != nil {
		return fmt.Errorf("open ChatGPT credential directory: %s", stableError(err))
	}
	defer parent.Close()
	if err := parent.Sync(); err != nil {
		return fmt.Errorf("sync ChatGPT credential directory: %s", stableError(err))
	}
	return nil
}

func lockChatGPTCredentials(ctx context.Context) (*os.File, error) {
	path, err := chatGPTCredentialPath()
	if err != nil {
		return nil, err
	}
	if err := privateDirectory(filepath.Dir(path), true); err != nil {
		return nil, err
	}
	lock, err := os.OpenFile(path+".lock", os.O_CREATE|os.O_RDWR|syscall.O_NOFOLLOW, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open ChatGPT credential lock: %s", stableError(err))
	}
	for {
		err = syscall.Flock(int(lock.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
		if err == nil {
			return lock, nil
		}
		if err != syscall.EWOULDBLOCK && err != syscall.EAGAIN {
			lock.Close()
			return nil, boundaryError{"lock ChatGPT credentials", err}
		}
		if err := waitBeforeRetry(ctx, 25*time.Millisecond); err != nil {
			lock.Close()
			return nil, boundaryError{"lock ChatGPT credentials", err}
		}
	}
}

func unlockChatGPTCredentials(lock *os.File) {
	syscall.Flock(int(lock.Fd()), syscall.LOCK_UN)
	lock.Close()
}

func chatGPTIssuer() string {
	return endpointBaseURL("REAGENT_OPENAI_AUTH_ISSUER", "https://auth.openai.com")
}

type chatGPTTokenSet struct {
	AccessToken       string `json:"access_token"`
	RefreshToken      string `json:"refresh_token"`
	IDToken           string `json:"id_token"`
	TokenType         string `json:"token_type"`
	ExpiresIn         int64  `json:"expires_in"`
	EarliestRefreshAt int64  `json:"earliest_refresh_at"`
	Scope             string `json:"scope"`
}

type chatGPTOAuthError struct{ code string }

func (e *chatGPTOAuthError) Error() string { return "ChatGPT authorization failed; run reagent login." }

type chatGPTTransportError struct{ status int }

func (e *chatGPTTransportError) Error() string {
	if e.status != 0 {
		return fmt.Sprintf("ChatGPT authorization service temporarily unavailable (HTTP %d)", e.status)
	}
	return "could not contact ChatGPT authorization service"
}

func terminalChatGPTRefreshCode(code string) bool {
	return code == "invalid_grant" || code == "invalid_refresh_token" || code == "token_expired" || strings.HasPrefix(code, "refresh_token_")
}

// Authentication responses can contain credentials, so neither bodies nor URL errors are quoted.
func chatGPTForm(ctx context.Context, client *http.Client, endpoint string, form url.Values, out any) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, strings.NewReader(form.Encode()))
	if err != nil {
		return fmt.Errorf("invalid ChatGPT authorization endpoint")
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	var acquiring, connected atomic.Bool
	req = req.WithContext(httptrace.WithClientTrace(req.Context(), &httptrace.ClientTrace{
		GetConn: func(string) { acquiring.Store(true) },
		GotConn: func(httptrace.GotConnInfo) { connected.Store(true) },
	}))
	// An injected client must not forward token-bearing forms through a redirect.
	authClient := *client
	authClient.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	response, err := authClient.Do(req)
	if err != nil {
		// Connection acquisition precedes sending; WroteRequest alone can race a lost response.
		// Untraced injected transports must still prove a failed dial.
		var network *net.OpError
		if acquiring.Load() && !connected.Load() || errors.As(err, &network) && network.Op == "dial" {
			return &chatGPTTransportError{}
		}
		return &chatGPTOAuthError{code: chatGPTRefreshOutcomeUnknown}
	}
	defer response.Body.Close()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		var body struct {
			Error json.RawMessage `json:"error"`
		}
		json.NewDecoder(io.LimitReader(response.Body, 1<<20)).Decode(&body)
		var code string
		if json.Unmarshal(body.Error, &code) != nil {
			var nested struct {
				Code string `json:"code"`
			}
			json.Unmarshal(body.Error, &nested)
			code = nested.Code
		}
		if retryable(response.StatusCode) && !terminalChatGPTRefreshCode(code) {
			return &chatGPTTransportError{status: response.StatusCode}
		}
		return &chatGPTOAuthError{code: code}
	}
	if out == nil {
		return nil
	}
	if err := json.NewDecoder(io.LimitReader(response.Body, 1<<20)).Decode(out); err != nil {
		return &chatGPTOAuthError{code: chatGPTRefreshOutcomeUnknown}
	}
	return nil
}

func applyChatGPTTokens(c *chatGPTCredentials, tokens chatGPTTokenSet) error {
	if tokens.AccessToken == "" || tokens.RefreshToken == "" || !strings.EqualFold(tokens.TokenType, "Bearer") || tokens.ExpiresIn <= 0 {
		return fmt.Errorf("incomplete ChatGPT token response; run reagent login")
	}
	c.AccessToken, c.RefreshToken = tokens.AccessToken, tokens.RefreshToken
	if tokens.IDToken != "" {
		c.IDToken = tokens.IDToken
	}
	c.ExpiresAt = time.Now().Unix() + tokens.ExpiresIn
	c.EarliestRefreshAt = tokens.EarliestRefreshAt
	if tokens.Scope != "" {
		c.Scope = tokens.Scope
	}
	c.NeedsLogin = false
	return nil
}

type chatGPTTokenSource struct {
	mu           sync.Mutex
	client       *http.Client
	secretTokens []string
}

func newChatGPTTokenSource(client *http.Client) (*chatGPTTokenSource, error) {
	c, err := loadChatGPTCredentials()
	if err != nil {
		return nil, err
	}
	if !c.planGranted() || c.NeedsLogin || c.AccessToken == "" || c.RefreshToken == "" || c.ClientID == "" {
		return nil, fmt.Errorf("%s", chatGPTReconnect)
	}
	if client == nil {
		client = NewHTTPClient()
	}
	source := &chatGPTTokenSource{client: client}
	source.rememberSecrets(c)
	return source, nil
}

// An empty token with no error means a refresh is needed.
func cachedChatGPTToken(c chatGPTCredentials, force bool, rejected string) (string, error) {
	if c.NeedsLogin || !c.planGranted() || c.RefreshToken == "" || c.AccessToken == "" || c.ClientID == "" {
		return "", fmt.Errorf("%s", chatGPTReconnect)
	}
	now := time.Now().Unix()
	if force && rejected != "" && c.AccessToken != rejected && c.ExpiresAt > now {
		return c.AccessToken, nil
	}
	if !force && c.ExpiresAt-now > 120 {
		return c.AccessToken, nil
	}
	if now < c.EarliestRefreshAt {
		if !force && c.ExpiresAt > now {
			return c.AccessToken, nil
		}
		return "", fmt.Errorf("%s", chatGPTReconnect)
	}
	return "", nil
}

func (s *chatGPTTokenSource) Token(ctx context.Context, force bool, rejected string) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	// Atomic file replacement lets valid credentials avoid a file-lock round trip.
	c, err := loadChatGPTCredentials()
	if err != nil {
		return "", err
	}
	s.rememberSecrets(c)
	if token, err := cachedChatGPTToken(c, force, rejected); token != "" || err != nil {
		return token, err
	}
	lock, err := lockChatGPTCredentials(ctx)
	if err != nil {
		return "", err
	}
	defer unlockChatGPTCredentials(lock)
	// Another process may have rotated the refresh token while we waited.
	c, err = loadChatGPTCredentials()
	if err != nil {
		return "", err
	}
	s.rememberSecrets(c)
	if token, err := cachedChatGPTToken(c, force, rejected); token != "" || err != nil {
		return token, err
	}
	if err := ctx.Err(); err != nil {
		return "", boundaryError{"refresh ChatGPT credentials", err}
	}
	// Once consumed, a rotating token's replacement must survive caller cancellation.
	refreshCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), 30*time.Second)
	defer cancel()
	var tokens chatGPTTokenSet
	err = chatGPTForm(refreshCtx, s.client, chatGPTIssuer()+"/api/accounts/oauth/token", url.Values{
		"grant_type": {"refresh_token"}, "client_id": {c.ClientID}, "refresh_token": {c.RefreshToken}, "resource": {chatGPTResource},
	}, &tokens)
	if err == nil {
		if err = applyChatGPTTokens(&c, tokens); err != nil {
			err = &chatGPTOAuthError{code: chatGPTRefreshOutcomeUnknown}
		}
	}
	if oauth, ok := err.(*chatGPTOAuthError); ok && (terminalChatGPTRefreshCode(oauth.code) || oauth.code == chatGPTRefreshOutcomeUnknown) {
		c.NeedsLogin = true
		// Uncertainty forbids reuse, but does not confirm these credentials were revoked.
		if oauth.code != chatGPTRefreshOutcomeUnknown {
			c.AccessToken, c.RefreshToken = "", ""
		}
		if err := saveChatGPTCredentials(c); err != nil {
			return "", err
		}
		return "", fmt.Errorf("%s", chatGPTReconnect)
	}
	if err != nil {
		return "", err
	}
	s.rememberSecrets(c)
	if err := saveChatGPTCredentials(c); err != nil {
		return "", err
	}
	if !c.planGranted() {
		return "", fmt.Errorf("ChatGPT plan usage was not granted; run reagent login.")
	}
	return c.AccessToken, nil
}

// Retired secrets remain sensitive even after another process rotates the file.
func (s *chatGPTTokenSource) rememberSecrets(c chatGPTCredentials) {
	for _, secret := range []string{c.AccessToken, c.RefreshToken, c.IDToken} {
		rememberToken(&s.secretTokens, secret)
	}
}

func (s *chatGPTTokenSource) secrets() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	if c, err := loadChatGPTCredentials(); err == nil {
		s.rememberSecrets(c)
	}
	return append([]string(nil), s.secretTokens...)
}
