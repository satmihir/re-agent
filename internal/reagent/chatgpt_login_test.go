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
		name                                                                                          string
		relogin, paste, badState, denied, declined, exchangeFails, reconsent, missingScope, loggedOut bool
		client, subject, wantError                                                                    string
	}{
		{name: "first_callback", client: "oaiapp_test", subject: "user"},
		{name: "paste", paste: true, client: "oaiapp_test", subject: "user"},
		{name: "relogin", relogin: true, subject: "user"},
		{name: "relogin_paste", relogin: true, paste: true, subject: "user"},
		{name: "reconsent_declined_plan", relogin: true, reconsent: true, subject: "user"},
		{name: "decline_logout_login", relogin: true, reconsent: true, loggedOut: true, subject: "user"},
		{name: "relogin_missing_scope", relogin: true, missingScope: true, subject: "user"},
		{name: "bad_state_callback", badState: true, client: "oaiapp_test", wantError: "state"},
		{name: "bad_state_paste", badState: true, paste: true, client: "oaiapp_test", wantError: "state"},
		{name: "access_denied", denied: true, wantError: "not approved"},
		{name: "different_client", relogin: true, client: "oaiapp_other", wantError: "different client"},
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
					exchanges.Add(1)
					r.ParseForm()
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
				callback := url.Values{"code": {"test-code"}, "state": {authorize.Get("state")}, "scope": {"openid"}}
				if scenario.client != "" {
					callback.Set("client_id", scenario.client)
				}
				if scenario.badState {
					callback.Set("state", "wrong")
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
	if _, err := waitChatGPTCallback(ctx, listener, redirect, "test-state", reader, &out, open, "https://auth.example/authorize"); err != nil {
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
