package reagent

import (
	"context"
	"crypto/tls"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func testChatGPTCredentials(t *testing.T) chatGPTCredentials {
	t.Helper()
	c := chatGPTCredentials{HostID: "urn:uuid:test", ClientID: "oaiapp_test", Subject: "user", Email: "user@example.com", IDToken: "secret-id", AccessToken: "old-access", RefreshToken: "old-refresh", ExpiresAt: time.Now().Unix() + 30, Scope: "openid chatgpt.tokens.use.direct"}
	if err := saveChatGPTCredentials(c); err != nil {
		t.Fatal(err)
	}
	return c
}

func TestChatGPTAuth_RefreshAdmissionAndRotation(t *testing.T) {
	for _, scenario := range []struct {
		name              string
		expires, earliest int64
		wantRefresh       bool
	}{
		{"near_expiry", 30, -1, true},
		{"not_earliest", 30, 20, false},
		{"valid", 3600, -1, false},
	} {
		t.Run(scenario.name, func(t *testing.T) {
			setTestHome(t, t.TempDir())
			c := testChatGPTCredentials(t)
			c.ExpiresAt = time.Now().Unix() + scenario.expires
			c.EarliestRefreshAt = time.Now().Unix() + scenario.earliest
			if err := saveChatGPTCredentials(c); err != nil {
				t.Fatal(err)
			}
			var calls atomic.Int32
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				calls.Add(1)
				r.ParseForm()
				if r.URL.Path != "/api/accounts/oauth/token" || r.Form.Get("grant_type") != "refresh_token" || r.Form.Get("client_id") != c.ClientID || r.Form.Get("refresh_token") != c.RefreshToken || r.Form.Get("resource") != chatGPTResource {
					t.Errorf("unexpected refresh form: %v", r.Form)
				}
				json.NewEncoder(w).Encode(chatGPTTokenSet{AccessToken: "new-access", RefreshToken: "rotated-refresh", TokenType: "Bearer", ExpiresIn: 3600, EarliestRefreshAt: time.Now().Unix() + 3000, Scope: c.Scope})
			}))
			defer server.Close()
			t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", server.URL)
			source, err := newChatGPTTokenSource(server.Client())
			if err != nil {
				t.Fatal(err)
			}
			token, err := source.Token(context.Background(), false, "")
			if err != nil {
				t.Fatal(err)
			}
			want := "old-access"
			if scenario.wantRefresh {
				want = "new-access"
			}
			if token != want || (calls.Load() == 1) != scenario.wantRefresh {
				t.Fatalf("token %s, refresh count %d", token, calls.Load())
			}
			stored, err := loadChatGPTCredentials()
			if err != nil {
				t.Fatal(err)
			}
			if scenario.wantRefresh && (stored.RefreshToken != "rotated-refresh" || stored.ExpiresAt <= c.ExpiresAt || stored.EarliestRefreshAt == 0) {
				t.Fatalf("rotation was not persisted")
			}
			path, _ := chatGPTCredentialPath()
			info, err := os.Stat(path)
			if err != nil || info.Mode().Perm() != 0600 {
				t.Fatalf("credential permissions: %v, %v", info, err)
			}
		})
	}
}

func TestChatGPTAuth_TwoGoroutinesRefreshOnce(t *testing.T) {
	setTestHome(t, t.TempDir())
	c := testChatGPTCredentials(t)
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		time.Sleep(50 * time.Millisecond)
		json.NewEncoder(w).Encode(chatGPTTokenSet{AccessToken: "new-access", RefreshToken: "rotated-refresh", TokenType: "Bearer", ExpiresIn: 3600, Scope: c.Scope})
	}))
	defer server.Close()
	t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", server.URL)
	source, err := newChatGPTTokenSource(server.Client())
	if err != nil {
		t.Fatal(err)
	}
	var group sync.WaitGroup
	for i := 0; i < 2; i++ {
		group.Add(1)
		go func() {
			defer group.Done()
			token, err := source.Token(context.Background(), false, "")
			if err != nil || token != "new-access" {
				t.Errorf("Token: %q, %v", token, err)
			}
		}()
	}
	group.Wait()
	if calls.Load() != 1 {
		t.Fatalf("refreshes: %d", calls.Load())
	}
	// A concurrent 401 for the old token must not rotate the replacement again.
	token, err := source.Token(context.Background(), true, "old-access")
	if err != nil || token != "new-access" || calls.Load() != 1 {
		t.Fatalf("rejected-token reread: %s, %v, calls %d", token, err, calls.Load())
	}
}

func TestChatGPTAuth_TwoProcessesRefreshOnce(t *testing.T) {
	home := t.TempDir()
	setTestHome(t, home)
	c := testChatGPTCredentials(t)
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		r.ParseForm()
		if r.Form.Get("refresh_token") != "old-refresh" {
			t.Errorf("reused rotating token: %v", r.Form)
		}
		time.Sleep(100 * time.Millisecond)
		json.NewEncoder(w).Encode(chatGPTTokenSet{AccessToken: "new-access", RefreshToken: "rotated-refresh", TokenType: "Bearer", ExpiresIn: 3600, Scope: c.Scope})
	}))
	defer server.Close()
	commands := make([]*exec.Cmd, 2)
	for i := range commands {
		cmd := exec.Command(os.Args[0], "-test.run=^TestChatGPTAuth_ProcessHelper$", "-test.count=1")
		bootstrap := t.TempDir()
		cmd.Env = append(os.Environ(), "HOME="+bootstrap, "XDG_CONFIG_HOME="+filepath.Join(bootstrap, ".config"), "REAGENT_OAUTH_HELPER_HOME="+home, "REAGENT_OPENAI_AUTH_ISSUER="+server.URL)
		cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
		if err := cmd.Start(); err != nil {
			t.Fatal(err)
		}
		commands[i] = cmd
	}
	for _, cmd := range commands {
		if err := cmd.Wait(); err != nil {
			t.Fatal(err)
		}
	}
	if calls.Load() != 1 {
		t.Fatalf("process refreshes: %d", calls.Load())
	}
	stored, _ := loadChatGPTCredentials()
	if stored.RefreshToken != "rotated-refresh" {
		t.Fatal("process rotation not persisted")
	}
}

func TestChatGPTAuth_ProcessHelper(t *testing.T) {
	home := os.Getenv("REAGENT_OAUTH_HELPER_HOME")
	if home == "" {
		return
	}
	setTestHome(t, home)
	source, err := newChatGPTTokenSource(nil)
	if err != nil {
		t.Fatal(err)
	}
	token, err := source.Token(context.Background(), false, "")
	if err != nil || token != "new-access" {
		t.Fatalf("process token: %q, %v", token, err)
	}
}

func TestChatGPTAuth_InvalidRefreshRequiresLogin(t *testing.T) {
	for _, code := range []string{"invalid_grant", "invalid_refresh_token", "token_expired", "refresh_token_expired", "refresh_token_invalid", "refresh_token_invalidated", "refresh_token_reused"} {
		t.Run(code, func(t *testing.T) {
			setTestHome(t, t.TempDir())
			testChatGPTCredentials(t)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.WriteHeader(400)
				fmt.Fprintf(w, `{"error":%q,"error_description":"old-refresh secret-id old-access"}`, code)
			}))
			defer server.Close()
			t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", server.URL)
			source, err := newChatGPTTokenSource(server.Client())
			if err != nil {
				t.Fatal(err)
			}
			_, err = source.Token(context.Background(), false, "")
			if err == nil || err.Error() != chatGPTReconnect {
				t.Fatalf("error: %v", err)
			}
			stored, _ := loadChatGPTCredentials()
			if !stored.NeedsLogin || stored.AccessToken != "" || stored.RefreshToken != "" {
				t.Fatal("reauthentication not persisted")
			}
			if _, err := newChatGPTTokenSource(nil); err == nil {
				t.Fatal("accepted invalid login")
			}
		})
	}
}

func TestChatGPTAuth_DeclinedScopeAndPrivateStorage(t *testing.T) {
	setTestHome(t, t.TempDir())
	c := testChatGPTCredentials(t)
	c.Scope = "openid profile email"
	if err := saveChatGPTCredentials(c); err != nil {
		t.Fatal(err)
	}
	if _, err := newChatGPTTokenSource(nil); err == nil {
		t.Fatal("accepted declined plan grant")
	}
	path, _ := chatGPTCredentialPath()
	if err := os.Chmod(path, 0644); err != nil {
		t.Fatal(err)
	}
	if _, err := loadChatGPTCredentials(); err == nil {
		t.Fatal("read public credentials")
	}
	if err := os.Remove(path); err != nil {
		t.Fatal(err)
	}
	target := filepath.Join(t.TempDir(), "target")
	if err := os.WriteFile(target, []byte(`{"access_token":"secret"}`), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, path); err != nil {
		t.Fatal(err)
	}
	if _, err := loadChatGPTCredentials(); err == nil || strings.Contains(err.Error(), "secret") {
		t.Fatalf("symlink read: %v", err)
	}
}

func TestChatGPTAuth_DirectorySymlinkRejected(t *testing.T) {
	setTestHome(t, t.TempDir())
	path, _ := chatGPTCredentialPath()
	if err := os.MkdirAll(filepath.Dir(filepath.Dir(path)), 0700); err != nil {
		t.Fatal(err)
	}
	target := t.TempDir()
	if err := os.WriteFile(filepath.Join(target, "chatgpt.json"), []byte(`{"access_token":"secret"}`), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(target, filepath.Dir(path)); err != nil {
		t.Fatal(err)
	}
	if _, err := loadChatGPTCredentials(); err == nil {
		t.Fatal("read symlink credential directory")
	}
	if err := saveChatGPTCredentials(chatGPTCredentials{}); err == nil {
		t.Fatal("wrote symlink credential directory")
	}
	if lock, err := lockChatGPTCredentials(context.Background()); err == nil {
		unlockChatGPTCredentials(lock)
		t.Fatal("locked symlink credential directory")
	}
}

func TestChatGPTAuth_TokenRedirectRefused(t *testing.T) {
	var forwarded atomic.Int32
	target := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { forwarded.Add(1) }))
	defer target.Close()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, target.URL+"?secret-token", http.StatusTemporaryRedirect)
	}))
	defer server.Close()
	err := chatGPTForm(context.Background(), server.Client(), server.URL, url.Values{"refresh_token": {"secret-token"}}, nil)
	if err == nil || strings.Contains(err.Error(), "secret-token") || forwarded.Load() != 0 {
		t.Fatalf("token redirect: %v, forwarded %d", err, forwarded.Load())
	}
}

func TestChatGPTAuth_TooEarlyForcedRefreshPreservesLogin(t *testing.T) {
	setTestHome(t, t.TempDir())
	c := testChatGPTCredentials(t)
	c.EarliestRefreshAt = time.Now().Unix() + 20
	if err := saveChatGPTCredentials(c); err != nil {
		t.Fatal(err)
	}
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { calls.Add(1) }))
	defer server.Close()
	t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", server.URL)
	source, err := newChatGPTTokenSource(server.Client())
	if err != nil {
		t.Fatal(err)
	}
	if _, err := source.Token(context.Background(), true, c.AccessToken); err == nil || err.Error() != chatGPTReconnect {
		t.Fatalf("early forced refresh: %v", err)
	}
	stored, err := loadChatGPTCredentials()
	if err != nil || calls.Load() != 0 || stored.NeedsLogin || stored.AccessToken != c.AccessToken || stored.RefreshToken != c.RefreshToken {
		t.Fatalf("early refresh changed credentials: %v", err)
	}
}

func TestChatGPTAuth_SecretSnapshotReadsCrossProcessRotation(t *testing.T) {
	setTestHome(t, t.TempDir())
	c := testChatGPTCredentials(t)
	source, err := newChatGPTTokenSource(nil)
	if err != nil {
		t.Fatal(err)
	}
	c.AccessToken, c.RefreshToken, c.IDToken = "other-access", "other-refresh", "other-id"
	if err := saveChatGPTCredentials(c); err != nil {
		t.Fatal(err)
	}
	known := source.secrets()
	for _, secret := range []string{"old-access", "old-refresh", "secret-id", "other-access", "other-refresh", "other-id"} {
		found := false
		for _, saved := range known {
			found = found || saved == secret
		}
		if !found {
			t.Errorf("snapshot missed a current or retired secret")
		}
	}
	known[0] = "modified"
	if source.secrets()[0] == "modified" {
		t.Fatal("snapshot aliases mutable source state")
	}
}

func TestChatGPTAuth_CancelledCallerPreservesRotation(t *testing.T) {
	setTestHome(t, t.TempDir())
	c := testChatGPTCredentials(t)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		call := calls.Add(1)
		if err := r.ParseForm(); err != nil {
			t.Error(err)
		}
		wantRefresh := "old-refresh"
		access, refresh := "new-access", "rotated-refresh"
		if call == 2 {
			wantRefresh = "rotated-refresh"
			access, refresh = "newest-access", "newest-refresh"
		}
		if call > 2 || r.Form.Get("refresh_token") != wantRefresh {
			t.Errorf("refresh %d reused a consumed token", call)
			w.WriteHeader(http.StatusBadRequest)
			fmt.Fprint(w, `{"error":"refresh_token_reused"}`)
			return
		}
		// The server has consumed the token before cancellation, but has not replied.
		cancel()
		<-ctx.Done()
		json.NewEncoder(w).Encode(chatGPTTokenSet{AccessToken: access, RefreshToken: refresh, TokenType: "Bearer", ExpiresIn: 3600, Scope: c.Scope})
	}))
	defer server.Close()
	t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", server.URL)
	source, err := newChatGPTTokenSource(server.Client())
	if err != nil {
		t.Fatal(err)
	}
	if token, err := source.Token(ctx, false, ""); err != nil || token != "new-access" {
		t.Fatalf("cancelled refresh: %q, %v", token, err)
	}
	stored, err := loadChatGPTCredentials()
	if err != nil || stored.NeedsLogin || stored.RefreshToken != "rotated-refresh" {
		t.Fatalf("replacement not persisted: %v", err)
	}
	if token, err := source.Token(context.Background(), false, ""); err != nil || token != "new-access" || calls.Load() != 1 {
		t.Fatalf("next token: %q, %v, refreshes %d", token, err, calls.Load())
	}
	if token, err := source.Token(context.Background(), true, "new-access"); err != nil || token != "newest-access" || calls.Load() != 2 {
		t.Fatalf("next rotation: %q, %v, refreshes %d", token, err, calls.Load())
	}
}

func TestChatGPTAuth_CancelledCallerDoesNotStartRefresh(t *testing.T) {
	setTestHome(t, t.TempDir())
	c := testChatGPTCredentials(t)
	var calls atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { calls.Add(1) }))
	defer server.Close()
	t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", server.URL)
	source, err := newChatGPTTokenSource(server.Client())
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, err := source.Token(ctx, false, ""); !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled refresh: %v", err)
	}
	stored, err := loadChatGPTCredentials()
	if err != nil || calls.Load() != 0 || stored != c {
		t.Fatalf("cancelled caller changed credentials: %v, refreshes %d", err, calls.Load())
	}
}

func TestChatGPTAuth_OnlyRefreshWaitsForCredentialLock(t *testing.T) {
	for _, scenario := range []struct {
		name              string
		expires, earliest int64
		wantLock          bool
	}{
		{"valid", 3600, -1, false},
		{"earliest_gate", 30, 3600, false},
		{"near_expiry", 30, -1, true},
		{"expired", -1, -1, true},
	} {
		t.Run(scenario.name, func(t *testing.T) {
			setTestHome(t, t.TempDir())
			c := testChatGPTCredentials(t)
			c.ExpiresAt = time.Now().Unix() + scenario.expires
			c.EarliestRefreshAt = time.Now().Unix() + scenario.earliest
			if err := saveChatGPTCredentials(c); err != nil {
				t.Fatal(err)
			}
			source, err := newChatGPTTokenSource(nil)
			if err != nil {
				t.Fatal(err)
			}
			lock, err := lockChatGPTCredentials(context.Background())
			if err != nil {
				t.Fatal(err)
			}
			defer unlockChatGPTCredentials(lock)
			ctx, cancel := context.WithTimeout(context.Background(), 200*time.Millisecond)
			defer cancel()
			token, err := source.Token(ctx, false, "")
			if scenario.wantLock {
				if token != "" || !errors.Is(err, context.DeadlineExceeded) {
					t.Fatalf("refresh bypassed lock: %q, %v", token, err)
				}
			} else if err != nil || token != c.AccessToken || ctx.Err() != nil {
				t.Fatalf("valid token waited for lock: %q, %v", token, err)
			}
		})
	}
}

func TestChatGPTAuth_TransientTransportErrorsAreTypedAndSanitized(t *testing.T) {
	for _, scenario := range []struct {
		name      string
		status    int
		code      string
		transient bool
	}{
		{"rate_limit", 429, "rate_limited", true},
		{"server_fault", 503, "server_error", true},
		{"unauthorized", 401, "unauthorized", false},
		{"invalid_grant", 400, "invalid_grant", false},
		{"terminal_server_fault", 503, "refresh_token_reused", false},
	} {
		t.Run(scenario.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.WriteHeader(scenario.status)
				fmt.Fprintf(w, `{"error":%q,"error_description":"secret-access secret-refresh secret-id"}`, scenario.code)
			}))
			defer server.Close()
			err := chatGPTForm(context.Background(), server.Client(), server.URL+"?secret-id", url.Values{"refresh_token": {"secret-refresh"}}, nil)
			var transient *chatGPTTransportError
			if err == nil || errors.As(err, &transient) != scenario.transient {
				t.Fatalf("transport classification: %v", err)
			}
			if transient != nil && transient.status != scenario.status {
				t.Fatalf("transient status: %d", transient.status)
			}
			if strings.Contains(err.Error(), "secret-") || strings.Contains(err.Error(), server.URL) {
				t.Fatalf("authorization error leaked credentials: %v", err)
			}
		})
	}
	server := httptest.NewServer(http.NotFoundHandler())
	server.Close()
	err := chatGPTForm(context.Background(), server.Client(), server.URL+"?secret-refresh", url.Values{"refresh_token": {"secret-refresh"}}, nil)
	var transient *chatGPTTransportError
	if !errors.As(err, &transient) || transient.status != 0 || strings.Contains(err.Error(), "secret-refresh") || strings.Contains(err.Error(), server.URL) {
		t.Fatalf("network error: %v", err)
	}
}

func TestChatGPTAuth_UncertainRotationNeverReplaysRefresh(t *testing.T) {
	for _, drop := range []string{"headers", "body", "malformed", "incomplete"} {
		t.Run(drop, func(t *testing.T) {
			setTestHome(t, t.TempDir())
			c := testChatGPTCredentials(t)
			var refreshes, rotations, modelCalls atomic.Int32
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.URL.Path != "/api/accounts/oauth/token" {
					modelCalls.Add(1)
					w.WriteHeader(http.StatusInternalServerError)
					return
				}
				if err := r.ParseForm(); err != nil {
					t.Error(err)
				}
				if refreshes.Add(1) != 1 || r.Form.Get("refresh_token") != c.RefreshToken {
					t.Error("replayed a consumed refresh token")
					w.WriteHeader(http.StatusBadRequest)
					fmt.Fprint(w, `{"error":"refresh_token_reused"}`)
					return
				}
				rotations.Add(1)
				if drop == "malformed" {
					fmt.Fprint(w, `{"access_token":`)
					return
				}
				if drop == "incomplete" {
					fmt.Fprint(w, `{"access_token":"replacement","token_type":"Bearer","expires_in":3600}`)
					return
				}
				if drop == "body" {
					w.Header().Set("Content-Length", "200")
					w.WriteHeader(http.StatusOK)
					w.(http.Flusher).Flush()
				}
				conn, _, err := w.(http.Hijacker).Hijack()
				if err != nil {
					t.Error(err)
					return
				}
				conn.Close()
			}))
			defer server.Close()
			t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", server.URL)
			source, err := newChatGPTTokenSource(server.Client())
			if err != nil {
				t.Fatal(err)
			}
			otherSource, err := newChatGPTTokenSource(server.Client())
			if err != nil {
				t.Fatal(err)
			}
			waits := 0
			transport := transport{
				endpoint: server.URL + "/v1/responses", client: server.Client(),
				tokenSource: source, trace: NewTrace(io.Discard),
				waitRetry: func(context.Context, time.Duration) error {
					waits++
					return context.Canceled
				},
			}
			ctx := withModelRetry(context.Background(), time.Hour, nil)
			_, _, err = transport.call(ctx, 1, []byte(`{}`), nil)
			var modelErr *ModelError
			if !errors.As(err, &modelErr) || modelErr.Status != StatusProviderError || waits != 0 || refreshes.Load() != 1 || rotations.Load() != 1 || modelCalls.Load() != 0 {
				t.Fatalf("ambiguous rotation: err=%v, waits=%d, refreshes=%d, rotations=%d, model calls=%d", err, waits, refreshes.Load(), rotations.Load(), modelCalls.Load())
			}
			stored, loadErr := loadChatGPTCredentials()
			c.NeedsLogin = true
			if loadErr != nil || stored != c {
				t.Fatalf("uncertain rotation must require login and retain credentials: %v", loadErr)
			}
			for _, laterSource := range []*chatGPTTokenSource{source, otherSource} {
				for _, force := range []bool{false, true} {
					if _, laterErr := laterSource.Token(context.Background(), force, c.AccessToken); laterErr == nil || laterErr.Error() != chatGPTReconnect || refreshes.Load() != 1 {
						t.Fatalf("later token replayed uncertain rotation: %v, refreshes=%d", laterErr, refreshes.Load())
					}
				}
			}
			if _, laterErr := newChatGPTTokenSource(server.Client()); laterErr == nil || laterErr.Error() != chatGPTReconnect {
				t.Fatalf("new source accepted uncertain credentials: %v", laterErr)
			}
			for _, secret := range []string{c.AccessToken, c.RefreshToken, c.IDToken, server.URL} {
				if strings.Contains(err.Error(), secret) {
					t.Fatal("ambiguous rotation error leaked authorization data")
				}
			}
		})
	}
}

func TestChatGPTAuth_ConnectionAcquisitionFailurePreservesLogin(t *testing.T) {
	for _, failure := range []string{"untrusted_certificate", "handshake_timeout", "proxy_connect"} {
		t.Run(failure, func(t *testing.T) {
			setTestHome(t, t.TempDir())
			original := testChatGPTCredentials(t)
			var requests atomic.Int32
			issuer := httptest.NewUnstartedServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requests.Add(1)
				if err := r.ParseForm(); err != nil {
					t.Error(err)
				}
				if r.Form.Get("refresh_token") != original.RefreshToken {
					t.Error("pre-send failure changed the refresh token")
				}
				json.NewEncoder(w).Encode(chatGPTTokenSet{AccessToken: "new-access", RefreshToken: "new-refresh", TokenType: "Bearer", ExpiresIn: 3600, Scope: original.Scope})
			}))
			issuer.Config.ErrorLog = log.New(io.Discard, "", 0)
			if failure == "handshake_timeout" {
				issuer.TLS = &tls.Config{GetConfigForClient: func(*tls.ClientHelloInfo) (*tls.Config, error) {
					time.Sleep(100 * time.Millisecond)
					return nil, nil
				}}
			}
			issuer.StartTLS()
			defer issuer.Close()
			t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", issuer.URL)
			client := NewHTTPClient()
			switch failure {
			case "handshake_timeout":
				transport := issuer.Client().Transport.(*http.Transport).Clone()
				transport.TLSHandshakeTimeout = 20 * time.Millisecond
				defer transport.CloseIdleConnections()
				client = &http.Client{Transport: transport}
			case "proxy_connect":
				proxy := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
					if r.Method != http.MethodConnect {
						t.Error("proxy received a token-bearing request instead of CONNECT")
					}
					w.WriteHeader(http.StatusBadGateway)
				}))
				defer proxy.Close()
				address, err := url.Parse(proxy.URL)
				if err != nil {
					t.Fatal(err)
				}
				transport := &http.Transport{Proxy: http.ProxyURL(address)}
				defer transport.CloseIdleConnections()
				client = &http.Client{Transport: transport}
			}
			source, err := newChatGPTTokenSource(client)
			if err != nil {
				t.Fatal(err)
			}
			_, err = source.Token(context.Background(), false, "")
			var transient *chatGPTTransportError
			if !errors.As(err, &transient) || transient.status != 0 || requests.Load() != 0 {
				t.Fatalf("unsent refresh was not transient: %v, requests=%d", err, requests.Load())
			}
			stored, loadErr := loadChatGPTCredentials()
			if loadErr != nil || stored != original {
				t.Fatalf("unsent refresh changed saved login: %v", loadErr)
			}
			for _, secret := range []string{original.AccessToken, original.RefreshToken, original.IDToken, issuer.URL} {
				if strings.Contains(err.Error(), secret) {
					t.Fatal("pre-send error leaked authorization data")
				}
			}
			// The next attempt can still rotate the original, unconsumed token.
			source.client = issuer.Client()
			token, err := source.Token(context.Background(), false, "")
			stored, loadErr = loadChatGPTCredentials()
			if err != nil || loadErr != nil || token != "new-access" || stored.RefreshToken != "new-refresh" || stored.NeedsLogin || requests.Load() != 1 {
				t.Fatalf("recovery lost login: %v, %v, requests=%d", err, loadErr, requests.Load())
			}
		})
	}
}
