package reagent

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"strings"
	"sync/atomic"
	"syscall"
	"testing"
	"time"
)

func testChatGPTIDToken(issuer, nonce, subject string, audience any, expiry int64) string {
	body, _ := json.Marshal(map[string]any{"iss": issuer, "nonce": nonce, "sub": subject, "aud": audience, "exp": expiry, "email": "user@example.com"})
	return "e30." + base64.RawURLEncoding.EncodeToString(body) + ".signature"
}

func TestChatGPTLogin_CallbackAndPaste(t *testing.T) {
	for _, scenario := range []struct {
		name                                                                                                                      string
		relogin, paste, badState, badClient, denied, declined, exchangeFails, reconsent, missingScope, loggedOut, terminalRefresh bool
		client, subject, wantError                                                                                                string
	}{
		{name: "first_callback", client: "oaiapp_test", subject: "user"},
		{name: "paste", paste: true, client: "oaiapp_test", subject: "user"},
		{name: "relogin", relogin: true, subject: "user"},
		{name: "relogin_paste", relogin: true, paste: true, subject: "user"},
		{name: "reconsent_declined_plan", relogin: true, reconsent: true, subject: "user"},
		{name: "decline_logout_login", relogin: true, reconsent: true, loggedOut: true, subject: "user"},
		{name: "relogin_missing_scope", relogin: true, missingScope: true, subject: "user"},
		{name: "bad_state_callback_then_valid", badState: true, client: "oaiapp_test", subject: "user"},
		{name: "bad_state_paste_then_valid", badState: true, paste: true, client: "oaiapp_test", subject: "user"},
		{name: "access_denied", denied: true, wantError: "not approved"},
		{name: "different_client_then_valid", relogin: true, badClient: true, client: "oaiapp_other", subject: "user"},
		{name: "different_client_paste_then_valid", relogin: true, paste: true, badClient: true, client: "oaiapp_other", subject: "user"},
		{name: "missing_registration_then_valid", badClient: true, subject: "user"},
		{name: "missing_registration_paste_then_valid", paste: true, badClient: true, subject: "user"},
		{name: "invalid_registration_then_valid", badClient: true, client: "invalid", subject: "user"},
		{name: "invalid_registration_paste_then_valid", paste: true, badClient: true, client: "oaiapp_", subject: "user"},
		{name: "terminal_refresh_during_wait_then_reauthorized", relogin: true, terminalRefresh: true, subject: "user"},
		{name: "different_subject", relogin: true, subject: "other", wantError: "same account"},
		{name: "declined_plan", declined: true, client: "oaiapp_test", subject: "user"},
		{name: "exchange_fails", exchangeFails: true, client: "oaiapp_test", subject: "user", wantError: "authorization failed"},
	} {
		t.Run(scenario.name, func(t *testing.T) {
			setTestHome(t, t.TempDir())
			var previous chatGPTCredentials
			if scenario.relogin {
				previous = testChatGPTCredentials(t)
				if scenario.reconsent {
					previous.Scope = "openid profile email"
					if err := saveChatGPTCredentials(previous); err != nil {
						t.Fatal(err)
					}
				}
			}
			var authorize url.Values
			var exchanges atomic.Int32
			var server *httptest.Server
			server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				switch r.URL.Path {
				case "/.well-known/openid-configuration":
					json.NewEncoder(w).Encode(chatGPTDiscovery{Issuer: server.URL, Authorize: server.URL + "/api/accounts/authorize", Token: server.URL + "/api/accounts/oauth/token"})
				case "/api/accounts/oauth/revoke":
					w.WriteHeader(http.StatusOK)
				case "/api/accounts/oauth/token":
					r.ParseForm()
					if r.Form.Get("grant_type") == "refresh_token" && scenario.terminalRefresh {
						w.WriteHeader(http.StatusBadRequest)
						fmt.Fprint(w, `{"error":"invalid_grant"}`)
						return
					}
					exchanges.Add(1)
					stored, err := loadChatGPTCredentials()
					if err != nil || stored.ClientID != "oaiapp_test" {
						t.Errorf("client ID not persisted before exchange: %q, %v", stored.ClientID, err)
					}
					challenge := sha256.Sum256([]byte(r.Form.Get("code_verifier")))
					if r.Form.Get("grant_type") != "authorization_code" || r.Form.Get("client_id") != "oaiapp_test" || r.Form.Get("redirect_uri") != authorize.Get("redirect_uri") || r.Form.Get("resource") != chatGPTResource || base64.RawURLEncoding.EncodeToString(challenge[:]) != authorize.Get("code_challenge") {
						t.Errorf("incorrect code exchange form")
					}
					if scenario.exchangeFails {
						w.WriteHeader(400)
						fmt.Fprint(w, `{"error":"invalid_grant","error_description":"new-access secret-id"}`)
						return
					}
					scope := "openid profile email chatgpt.tokens.use.direct"
					if scenario.declined {
						scope = "openid profile email"
					}
					if scenario.missingScope {
						scope = ""
					}
					json.NewEncoder(w).Encode(chatGPTTokenSet{AccessToken: "new-access", RefreshToken: "new-refresh", IDToken: testChatGPTIDToken(server.URL, authorize.Get("nonce"), scenario.subject, []string{"other", "oaiapp_test"}, time.Now().Unix()+3600), TokenType: "Bearer", ExpiresIn: 3600, Scope: scope})
				default:
					t.Errorf("unexpected request: %s", r.URL.Path)
					w.WriteHeader(404)
				}
			}))
			defer server.Close()
			t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", server.URL)
			if scenario.loggedOut {
				if err := logoutChatGPT(context.Background(), server.Client(), io.Discard); err != nil {
					t.Fatal(err)
				}
				var err error
				previous, err = loadChatGPTCredentials()
				if err != nil {
					t.Fatal(err)
				}
			}
			reader, writer := io.Pipe()
			defer reader.Close()
			defer writer.Close()
			open := func(ctx context.Context, address string) error {
				u, err := url.Parse(address)
				if err != nil {
					return err
				}
				authorize = u.Query()
				if (authorize.Get("prompt") == "consent") != scenario.reconsent || authorize.Has("force_reconsent") {
					t.Errorf("consent prompt should be requested only after a previous decline")
				}
				if authorize.Get("response_type") != "code" || authorize.Get("resource") != chatGPTResource || authorize.Get("code_challenge_method") != "S256" || authorize.Get("state") == "" || authorize.Get("nonce") == "" || !strings.Contains(authorize.Get("scope"), "chatgpt.tokens.use.direct") {
					t.Errorf("incomplete authorization parameters")
				}
				if scenario.relogin {
					if authorize.Get("client_id") != previous.ClientID || authorize.Get("id_token_hint") != previous.IDToken || authorize.Get("login_hint") != previous.Email || authorize.Has("agent_name_hint") || authorize.Get("ext_agent_host_id") != previous.HostID {
						t.Errorf("incorrect relogin hints")
					}
				} else if authorize.Get("client_id") != "dynamic_agent_client" || authorize.Get("agent_name_hint") != "re:agent" || !strings.HasPrefix(authorize.Get("ext_agent_host_id"), "urn:uuid:") {
					t.Errorf("incorrect registration parameters")
				}
				if scenario.terminalRefresh {
					source, err := newChatGPTTokenSource(server.Client())
					if err != nil {
						return err
					}
					if _, err := source.Token(ctx, true, previous.AccessToken); err == nil {
						t.Error("terminal refresh unexpectedly succeeded")
					}
					current, err := loadChatGPTCredentials()
					if err != nil || current.RefreshToken != "" || current.LogoutGeneration != previous.LogoutGeneration || !current.NeedsLogin {
						t.Fatalf("terminal refresh did not clear tokens without logout: %v", err)
					}
				}
				callback := url.Values{"code": {"test-code"}, "state": {authorize.Get("state")}, "scope": {"openid"}}
				if scenario.client != "" {
					callback.Set("client_id", scenario.client)
				}
				if scenario.badState || scenario.badClient {
					if scenario.badState {
						callback.Set("state", "wrong")
					}
					bad := authorize.Get("redirect_uri") + "?" + callback.Encode()
					if scenario.paste {
						if _, err := fmt.Fprintln(writer, bad); err != nil {
							return err
						}
					} else {
						response, err := server.Client().Get(bad)
						if err != nil {
							return err
						}
						response.Body.Close()
						if response.StatusCode != http.StatusBadRequest {
							t.Errorf("bad state callback status: %d", response.StatusCode)
						}
					}
					callback.Set("state", authorize.Get("state"))
					if scenario.badClient {
						callback.Set("client_id", "oaiapp_test")
					}
				}
				if scenario.denied {
					callback.Set("error", "access_denied")
				}
				address = authorize.Get("redirect_uri") + "?" + callback.Encode()
				if scenario.paste {
					_, err = fmt.Fprintln(writer, address)
					return err
				}
				response, err := server.Client().Get(address)
				if err != nil {
					return err
				}
				defer response.Body.Close()
				body, _ := io.ReadAll(response.Body)
				if string(body) != "Return to re:agent — you can close this tab.\n" {
					t.Errorf("callback page: %q", body)
				}
				return nil
			}
			var out bytes.Buffer
			c, err := loginChatGPT(context.Background(), server.Client(), reader, &out, open)
			if scenario.wantError != "" {
				if err == nil || !strings.Contains(err.Error(), scenario.wantError) {
					t.Fatalf("want %q, got %v", scenario.wantError, err)
				}
				if scenario.wantError == "same account" {
					path, pathErr := chatGPTCredentialPath()
					if pathErr != nil || !strings.Contains(err.Error(), path) || !strings.Contains(err.Error(), "use the saved ChatGPT account or OPENAI_API_KEY") {
						t.Fatalf("missing account recovery/path: %v", err)
					}
					for _, secret := range []string{previous.AccessToken, previous.RefreshToken, previous.IDToken, "access-secret", "refresh-secret", "code-secret", "user@example.com"} {
						if secret != "" && strings.Contains(err.Error(), secret) {
							t.Fatal("account mismatch diagnostic disclosed a secret")
						}
					}
					if strings.Contains(err.Error(), "delete") || strings.Contains(err.Error(), "remove") {
						t.Fatalf("account mismatch suggests identity removal: %v", err)
					}
				}
				if scenario.wantError != "same account" && !scenario.exchangeFails && exchanges.Load() != 0 {
					t.Fatal("exchanged rejected callback")
				}
				if scenario.relogin {
					stored, err := loadChatGPTCredentials()
					if err != nil || stored.AccessToken != previous.AccessToken || stored.RefreshToken != previous.RefreshToken || stored.Subject != previous.Subject {
						t.Fatalf("rejected relogin changed active credentials: %v", err)
					}
				}
				if scenario.exchangeFails {
					stored, err := loadChatGPTCredentials()
					if err != nil || stored.ClientID != "oaiapp_test" || stored.AccessToken != "" {
						t.Fatalf("registration lost after failed exchange: %v", err)
					}
				}
			} else {
				if err != nil {
					t.Fatal(err)
				}
				if c.planGranted() == (scenario.declined || scenario.missingScope) {
					t.Fatalf("plan grant: %v", c.planGranted())
				}
				path, _ := chatGPTCredentialPath()
				info, err := os.Stat(path)
				if err != nil || info.Mode().Perm() != 0600 {
					t.Fatalf("login credential permissions: %v, %v", info, err)
				}
				stored, err := loadChatGPTCredentials()
				if err != nil || stored.AccessToken != "new-access" || stored.Subject != "user" {
					t.Fatalf("saved login: %v", err)
				}
				if exchanges.Load() != 1 {
					t.Fatalf("exchanges: %d", exchanges.Load())
				}
			}
			for _, line := range strings.Split(out.String(), "\n") {
				if strings.HasPrefix(line, server.URL) {
					printed, err := url.Parse(line)
					if err != nil || printed.Query().Has("id_token_hint") {
						t.Fatal("public fallback URL contains an identity hint")
					}
				}
			}
			if scenario.relogin && previous.IDToken != "" && !strings.Contains(out.String(), "may show the account picker") {
				t.Fatal("missing account-picker fallback guidance")
			}
			for _, secret := range []string{"secret-id", "old-access", "old-refresh", "new-access", "new-refresh"} {
				if strings.Contains(out.String(), secret) || (err != nil && strings.Contains(err.Error(), secret)) {
					t.Fatalf("secret leaked: %s", secret)
				}
			}
			if !strings.Contains(out.String(), "ssh -L") || !strings.Contains(out.String(), "paste") {
				t.Fatalf("missing SSH fallback guidance: %s", out.String())
			}
		})
	}
}

func TestChatGPTLogin_IDTokenClaims(t *testing.T) {
	for _, scenario := range []struct {
		name, issuer, nonce, subject string
		aud                          any
		expiry                       int64
		valid                        bool
	}{
		{"valid", "issuer", "nonce", "user", "client", time.Now().Unix() + 3600, true},
		{"issuer", "wrong", "nonce", "user", "client", time.Now().Unix() + 3600, false},
		{"nonce", "issuer", "wrong", "user", "client", time.Now().Unix() + 3600, false},
		{"subject", "issuer", "nonce", "", "client", time.Now().Unix() + 3600, false},
		{"audience", "issuer", "nonce", "user", []string{"other"}, time.Now().Unix() + 3600, false},
		{"expired", "issuer", "nonce", "user", "client", time.Now().Unix() - 1, false},
	} {
		t.Run(scenario.name, func(t *testing.T) {
			token := testChatGPTIDToken(scenario.issuer, scenario.nonce, scenario.subject, scenario.aud, scenario.expiry)
			_, _, err := validateChatGPTIDToken(token, "issuer", "client", "nonce", "")
			if (err == nil) != scenario.valid {
				t.Fatalf("claims validation: %v", err)
			}
		})
	}
}

func TestChatGPTLogin_LogoutAndStatus(t *testing.T) {
	for _, revokeFails := range []bool{false, true} {
		t.Run(fmt.Sprint(revokeFails), func(t *testing.T) {
			setTestHome(t, t.TempDir())
			c := testChatGPTCredentials(t)
			c.ExpiresAt = time.Now().Unix() + 3600
			if err := saveChatGPTCredentials(c); err != nil {
				t.Fatal(err)
			}
			var revokes atomic.Int32
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.URL.Path == "/models" {
					if r.Header.Get("Authorization") != "Bearer old-access" {
						t.Error("model listing bearer token missing")
					}
					fmt.Fprint(w, `{"models":[{"slug":"visible old-access","display_name":"Visible Model old-refresh secret-id\u001b[2J\nforged","visibility":"list"},{"slug":"hidden","visibility":"hide"}]}`)
					return
				}
				revokes.Add(1)
				r.ParseForm()
				if r.URL.Path != "/api/accounts/oauth/revoke" || r.Form.Get("token") != c.RefreshToken || r.Form.Get("token_type_hint") != "refresh_token" || r.Form.Get("client_id") != c.ClientID {
					t.Errorf("revoke form wrong")
				}
				if revokeFails {
					w.WriteHeader(503)
					fmt.Fprint(w, `{"detail":"old-refresh"}`)
				}
			}))
			defer server.Close()
			t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", server.URL)
			t.Setenv("REAGENT_OPENAI_BASE_URL", server.URL)
			var out, stderr bytes.Buffer
			if code := chatGPTAuthCommand(context.Background(), []string{"auth", "status"}, nil, &out, &stderr); code != exitOK {
				t.Fatalf("status: %d %s", code, stderr.String())
			}
			if !strings.Contains(out.String(), c.Email) || !strings.Contains(out.String(), "visible") || strings.Contains(out.String(), "hidden") || strings.Contains(out.String(), "old-access") {
				t.Fatalf("status output: %s", out.String())
			}
			if strings.Contains(out.String(), "old-refresh") || strings.Contains(out.String(), "secret-id") || strings.Contains(out.String(), "\x1b") || strings.Contains(out.String(), "\nforged") || !strings.Contains(out.String(), "[redacted]") {
				t.Fatalf("unsafe model-list output: %s", out.String())
			}
			out.Reset()
			if err := logoutChatGPT(context.Background(), server.Client(), &out); err != nil {
				t.Fatal(err)
			}
			stored, err := loadChatGPTCredentials()
			if err != nil || stored.ClientID != c.ClientID || stored.HostID != c.HostID || stored.Email != c.Email || stored.Subject != c.Subject || stored.AccessToken != "" || stored.RefreshToken != "" || stored.IDToken != "" {
				t.Fatalf("logout credentials not cleared: %v", err)
			}
			if revokes.Load() != 1 || strings.Contains(out.String(), c.RefreshToken) || strings.Contains(out.String(), "Could not confirm") != revokeFails {
				t.Fatalf("logout output: %s", out.String())
			}
			out.Reset()
			if code := chatGPTAuthCommand(context.Background(), []string{"auth", "status"}, nil, &out, &stderr); code != exitOK {
				t.Fatalf("logged-out status: %d %s", code, stderr.String())
			}
			if !strings.Contains(out.String(), "Account: "+c.Email) || !strings.Contains(out.String(), "ChatGPT plan usage: disabled") || !strings.Contains(out.String(), "Access token expires: none") || strings.Contains(out.String(), "Available models:") {
				t.Fatalf("logged-out status output: %s", out.String())
			}
		})
	}
}

func TestChatGPTLogin_Cancellation(t *testing.T) {
	setTestHome(t, t.TempDir())
	var server *httptest.Server
	server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		json.NewEncoder(w).Encode(chatGPTDiscovery{Issuer: server.URL, Authorize: server.URL + "/authorize", Token: server.URL + "/token"})
	}))
	defer server.Close()
	t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", server.URL)
	ctx, cancel := context.WithCancel(context.Background())
	open := func(context.Context, string) error { cancel(); return nil }
	var out bytes.Buffer
	_, err := loginChatGPT(ctx, server.Client(), strings.NewReader(""), &out, open)
	if err == nil || !strings.Contains(err.Error(), "cancelled") {
		t.Fatalf("cancellation: %v", err)
	}
	stored, err := loadChatGPTCredentials()
	if err != nil || stored.HostID == "" || stored.AccessToken != "" {
		t.Fatalf("cancelled login storage: %v", err)
	}
}

func TestChatGPTLogin_CallbackDoesNotLeaveFileReader(t *testing.T) {
	reader, writer, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	defer reader.Close()
	defer writer.Close()
	before, _, errno := syscall.Syscall(syscall.SYS_FCNTL, reader.Fd(), syscall.F_GETFL, 0)
	if errno != 0 {
		t.Fatal(errno)
	}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	redirect := "http://" + listener.Addr().String() + "/auth/callback"
	open := func(context.Context, string) error {
		response, err := NewHTTPClient().Get(redirect + "?state=test-state&code=test-code")
		if err != nil {
			return err
		}
		response.Body.Close()
		return nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	var out bytes.Buffer
	if _, err := waitChatGPTCallback(ctx, listener, redirect, "test-state", "oaiapp_test", reader, &out, open, "https://auth.example/authorize"); err != nil {
		t.Fatal(err)
	}
	after, _, errno := syscall.Syscall(syscall.SYS_FCNTL, reader.Fd(), syscall.F_GETFL, 0)
	if errno != 0 || before != after {
		t.Fatalf("stdin flags changed: %d to %d (%v)", before, after, errno)
	}
	if _, err := fmt.Fprint(writer, "next chat input\n"); err != nil {
		t.Fatal(err)
	}
	data := make([]byte, len("next chat input\n"))
	if _, err := io.ReadFull(reader, data); err != nil || string(data) != "next chat input\n" {
		t.Fatalf("login stole later input: %q %v", data, err)
	}
}

func TestChatGPTLogin_PasteReaderWaitsUntilCancellation(t *testing.T) {
	reader, writer, err := os.Pipe()
	if err != nil {
		t.Fatal(err)
	}
	defer reader.Close()
	defer writer.Close()
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()
	paste := chatGPTPasteReader{ctx: ctx, reader: pollingReader{fd: int(reader.Fd())}}
	var data [1]byte
	if n, err := paste.Read(data[:]); n != 0 || err != io.EOF || ctx.Err() == nil {
		t.Fatalf("empty polls should not stop the scanner before cancellation: %d, %v", n, err)
	}
}

func TestChatGPTLogin_StatusRedactsRetiredAndRotatedSecrets(t *testing.T) {
	setTestHome(t, t.TempDir())
	c := testChatGPTCredentials(t)
	secrets := []string{c.AccessToken, c.RefreshToken, c.IDToken, "new-access", "new-refresh", "new-id"}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/api/accounts/oauth/token" {
			json.NewEncoder(w).Encode(chatGPTTokenSet{AccessToken: "new-access", RefreshToken: "new-refresh", IDToken: "new-id", TokenType: "Bearer", ExpiresIn: 3600, Scope: c.Scope})
			return
		}
		if r.URL.Path != "/models" || r.Header.Get("Authorization") != "Bearer new-access" {
			t.Error("model listing should use rotated token")
		}
		json.NewEncoder(w).Encode(map[string]any{"models": []map[string]string{{"slug": strings.Join(secrets, " "), "display_name": "model", "visibility": "list"}}})
	}))
	defer server.Close()
	t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", server.URL)
	t.Setenv("REAGENT_OPENAI_BASE_URL", server.URL)
	var out, stderr bytes.Buffer
	if code := chatGPTAuthCommand(context.Background(), []string{"auth", "status"}, nil, &out, &stderr); code != exitOK {
		t.Fatalf("status: %d %s", code, stderr.String())
	}
	for _, secret := range secrets {
		if strings.Contains(out.String(), secret) || strings.Contains(stderr.String(), secret) {
			t.Fatal("status leaked a current or retired credential")
		}
	}
}

func TestChatGPTLogin_InvalidCallbacksKeepWaiting(t *testing.T) {
	for _, query := range []string{
		"", "code=secret-code", "state=wrong&code=secret-code",
		"state=test-state&state=test-state&code=secret-code",
		"state=test-state&code=%zz", "state=test-state",
		"state=test-state&code=", "state=test-state&code=a&code=b",
		"state=test-state&code=a&client_id=a&client_id=b",
		"state=test-state&error=access_denied&error=other",
	} {
		t.Run(query, func(t *testing.T) {
			listener, err := net.Listen("tcp", "127.0.0.1:0")
			if err != nil {
				t.Fatal(err)
			}
			defer listener.Close()
			redirect := "http://" + listener.Addr().String() + "/auth/callback"
			open := func(context.Context, string) error {
				for _, address := range []string{redirect + "?" + query, "http://" + listener.Addr().String() + "/favicon.ico"} {
					response, err := NewHTTPClient().Get(address)
					if err != nil {
						return err
					}
					body, _ := io.ReadAll(response.Body)
					response.Body.Close()
					if response.StatusCode != http.StatusBadRequest || strings.Contains(string(body), "secret-code") {
						t.Errorf("invalid callback response: %d %q", response.StatusCode, body)
					}
				}
				for _, method := range []string{http.MethodGet, http.MethodPost} {
					req, _ := http.NewRequest(method, redirect+"?state=test-state&code=wrong-address", nil)
					if method == http.MethodGet {
						req.Host = "wrong.example"
					}
					response, err := NewHTTPClient().Do(req)
					if err != nil {
						return err
					}
					response.Body.Close()
					if response.StatusCode != http.StatusBadRequest {
						t.Errorf("invalid method/address callback status: %d", response.StatusCode)
					}
				}
				response, err := NewHTTPClient().Get(redirect + "?state=test-state&code=valid-code")
				if err == nil {
					response.Body.Close()
				}
				return err
			}
			ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
			defer cancel()
			values, err := waitChatGPTCallback(ctx, listener, redirect, "test-state", "oaiapp_test", nil, io.Discard, open, "https://auth.example/authorize")
			if err != nil || values.Get("code") != "valid-code" || values.Get("state") != "test-state" {
				t.Fatalf("valid callback after stray request: %v %v", values, err)
			}
		})
	}
}

func TestChatGPTLogin_InvalidPasteThenValidAndSingleWinner(t *testing.T) {
	for _, useCallback := range []bool{false, true} {
		t.Run(fmt.Sprint(useCallback), func(t *testing.T) {
			listener, err := net.Listen("tcp", "127.0.0.1:0")
			if err != nil {
				t.Fatal(err)
			}
			defer listener.Close()
			redirect := "http://" + listener.Addr().String() + "/auth/callback"
			input := strings.NewReader("https://wrong.example/?code=secret-code\n" + redirect + "?state=test-state&code=pasted\n")
			open := func(context.Context, string) error {
				if !useCallback {
					return fmt.Errorf("browser unavailable")
				}
				response, err := NewHTTPClient().Get(redirect + "?state=test-state&code=callback")
				if err == nil {
					response.Body.Close()
				}
				return err
			}
			ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
			defer cancel()
			var out bytes.Buffer
			values, err := waitChatGPTCallback(ctx, listener, redirect, "test-state", "oaiapp_test", input, &out, open, "https://auth.example/authorize")
			if err != nil || (values.Get("code") != "pasted" && (!useCallback || values.Get("code") != "callback")) {
				t.Fatalf("callback/paste winner: %v %v", values, err)
			}
			if strings.Contains(out.String(), "secret-code") || (!useCallback && !strings.Contains(out.String(), "Still waiting for approval")) {
				t.Fatalf("invalid paste diagnostic: %s", out.String())
			}
		})
	}
}

func TestChatGPTLogin_PendingAllowsTokenAndRefreshPreservesLatestOnFailure(t *testing.T) {
	setTestHome(t, t.TempDir())
	c := testChatGPTCredentials(t)
	c.ExpiresAt = time.Now().Unix() + 3600
	if err := saveChatGPTCredentials(c); err != nil {
		t.Fatal(err)
	}
	var refreshes, exchanges atomic.Int32
	var server *httptest.Server
	server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/.well-known/openid-configuration" {
			json.NewEncoder(w).Encode(chatGPTDiscovery{Issuer: server.URL, Authorize: server.URL + "/authorize", Token: server.URL + "/api/accounts/oauth/token"})
			return
		}
		r.ParseForm()
		if r.Form.Get("grant_type") == "refresh_token" {
			refreshes.Add(1)
			json.NewEncoder(w).Encode(chatGPTTokenSet{AccessToken: "refreshed-access", RefreshToken: "refreshed-refresh", IDToken: "refreshed-id", TokenType: "Bearer", ExpiresIn: 3600, Scope: c.Scope})
			return
		}
		exchanges.Add(1)
		w.WriteHeader(http.StatusBadRequest)
		fmt.Fprint(w, `{"error":"invalid_grant"}`)
	}))
	defer server.Close()
	t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", server.URL)
	source, err := newChatGPTTokenSource(server.Client())
	if err != nil {
		t.Fatal(err)
	}
	reader, writer := io.Pipe()
	defer reader.Close()
	defer writer.Close()
	var latest chatGPTCredentials
	open := func(ctx context.Context, address string) error {
		ctx, cancel := context.WithTimeout(ctx, time.Second)
		defer cancel()
		if token, err := source.Token(ctx, false, ""); err != nil || token != c.AccessToken {
			t.Errorf("pending login blocked valid token: %q %v", token, err)
			return fmt.Errorf("token failed")
		}
		if token, err := source.Token(ctx, true, c.AccessToken); err != nil || token != "refreshed-access" {
			t.Errorf("pending login blocked refresh: %q %v", token, err)
			return fmt.Errorf("refresh failed")
		}
		latest, err = loadChatGPTCredentials()
		if err != nil {
			return err
		}
		u, _ := url.Parse(address)
		_, err = fmt.Fprintln(writer, u.Query().Get("redirect_uri")+"?"+url.Values{"state": {u.Query().Get("state")}, "code": {"test-code"}}.Encode())
		return err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	returned, err := loginChatGPT(ctx, server.Client(), reader, io.Discard, open)
	if err == nil || !strings.Contains(err.Error(), "authorization failed") || returned != latest {
		t.Fatalf("rejected exchange: %v; current snapshot retained: %v", err, returned == latest)
	}
	stored, err := loadChatGPTCredentials()
	if err != nil || stored != latest || refreshes.Load() != 1 || exchanges.Load() != 1 {
		t.Fatalf("latest credentials not preserved: %v, refreshes %d exchanges %d", err, refreshes.Load(), exchanges.Load())
	}
}

func TestChatGPTLogin_ConcurrentIdentityChangeAndLogoutRejectPending(t *testing.T) {
	for _, change := range []string{"client", "subject", "host", "registration", "logout", "first_login_logout", "logged_out_relogin_logout"} {
		t.Run(change, func(t *testing.T) {
			setTestHome(t, t.TempDir())
			if change != "registration" && change != "first_login_logout" {
				testChatGPTCredentials(t)

			}
			var concurrentNonce string
			var exchanges atomic.Int32
			var server *httptest.Server
			server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				switch r.URL.Path {
				case "/.well-known/openid-configuration":
					json.NewEncoder(w).Encode(chatGPTDiscovery{Issuer: server.URL, Authorize: server.URL + "/authorize", Token: server.URL + "/token"})
				case "/api/accounts/oauth/revoke":
					w.WriteHeader(http.StatusOK)
				default:
					exchanges.Add(1)
					if change == "registration" {
						json.NewEncoder(w).Encode(chatGPTTokenSet{AccessToken: "concurrent-access", RefreshToken: "concurrent-refresh", IDToken: testChatGPTIDToken(server.URL, concurrentNonce, "concurrent-user", "oaiapp_concurrent", time.Now().Unix()+3600), TokenType: "Bearer", ExpiresIn: 3600, Scope: "openid chatgpt.tokens.use.direct"})
						return
					}
					w.WriteHeader(http.StatusBadRequest)
				}
			}))
			defer server.Close()
			t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", server.URL)
			if change == "logged_out_relogin_logout" {
				if err := logoutChatGPT(context.Background(), server.Client(), io.Discard); err != nil {
					t.Fatal(err)
				}
			}
			reader, writer := io.Pipe()
			defer reader.Close()
			defer writer.Close()
			var latest chatGPTCredentials
			open := func(ctx context.Context, address string) error {
				ctx, cancel := context.WithTimeout(ctx, time.Second)
				defer cancel()
				if change == "registration" {
					concurrentOpen := func(_ context.Context, address string) error {
						u, _ := url.Parse(address)
						query := u.Query()
						concurrentNonce = query.Get("nonce")
						response, err := server.Client().Get(query.Get("redirect_uri") + "?" + url.Values{"state": {query.Get("state")}, "code": {"concurrent-code"}, "client_id": {"oaiapp_concurrent"}}.Encode())
						if err == nil {
							response.Body.Close()
						}
						return err
					}
					if _, err := loginChatGPT(ctx, server.Client(), nil, io.Discard, concurrentOpen); err != nil {
						return err
					}
				} else if change == "logout" || change == "first_login_logout" || change == "logged_out_relogin_logout" {
					if err := logoutChatGPT(ctx, server.Client(), io.Discard); err != nil {
						return err
					}
				} else {
					lock, err := lockChatGPTCredentials(ctx)
					if err != nil {
						return err
					}
					current, err := loadChatGPTCredentials()
					if err == nil {
						switch change {
						case "client":
							current.ClientID = "oaiapp_concurrent"
						case "subject":
							current.Subject = "concurrent-user"
						case "host":
							current.HostID = "urn:uuid:concurrent"
						}
						err = saveChatGPTCredentials(current)
					}
					unlockChatGPTCredentials(lock)
					if err != nil {
						return err
					}
				}
				var err error
				latest, err = loadChatGPTCredentials()
				if err != nil {
					return err
				}
				u, _ := url.Parse(address)
				_, err = fmt.Fprintln(writer, u.Query().Get("redirect_uri")+"?"+url.Values{"state": {u.Query().Get("state")}, "code": {"test-code"}, "client_id": {"oaiapp_test"}}.Encode())
				return err
			}
			ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
			defer cancel()
			_, err := loginChatGPT(ctx, server.Client(), reader, io.Discard, open)
			if err == nil || !strings.Contains(err.Error(), "credentials changed") {
				t.Fatalf("pending login after %s: %v", change, err)
			}
			stored, err := loadChatGPTCredentials()
			wantExchanges := int32(0)
			if change == "registration" {
				wantExchanges = 1
			}
			if err != nil || stored != latest || exchanges.Load() != wantExchanges {
				t.Fatalf("pending login overwrote %s: %v, exchanges %d", change, err, exchanges.Load())
			}
		})
	}
}
