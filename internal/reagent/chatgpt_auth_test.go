package reagent

import (
	"context"
	"encoding/json"
	"fmt"
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
	if _, err := source.Token(context.Background(), true, c.AccessToken); err == nil || !strings.Contains(err.Error(), "cannot be refreshed yet") {
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
