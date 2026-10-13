package reagent

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func transportChatGPTSource(t *testing.T) *chatGPTTokenSource {
	t.Helper()
	home := t.TempDir()
	setTestHome(t, home)
	t.Setenv("XDG_CONFIG_HOME", filepath.Join(home, ".config"))
	if err := saveChatGPTCredentials(chatGPTCredentials{
		ClientID: "oaiapp_test", Subject: "subscriber", Email: "user@example.com",
		AccessToken: "access-secret", RefreshToken: "refresh-secret", IDToken: "id-secret",
		ExpiresAt: time.Now().Add(time.Hour).Unix(), Scope: "openid chatgpt.tokens.use.direct",
	}); err != nil {
		t.Fatal(err)
	}
	source, err := newChatGPTTokenSource(NewHTTPClient())
	if err != nil {
		t.Fatal(err)
	}
	return source
}

func TestChatGPT_ConversationUsesStableUUIDAndStreamEncoding(t *testing.T) {
	source := transportChatGPTSource(t)
	first := sse(itemDone(0, `{"type":"function_call","id":"fc_1","status":"completed","call_id":"call_1","name":"echo","arguments":"{\"text\":\"marker\"}"}`), completed(`[]`))
	api := newFakeAPI(t, okReply(first), okReply(streamedTextReply()))
	api.replies[1].header = map[string]string{"x-oai-request-id": "oai-request", "x-request-id": "wrong-request"}
	cfg := testConfig(t)
	cfg.Provider, cfg.Model = openaiName, "test-model"
	trace := NewTrace(io.Discard)
	path := filepath.Join(t.TempDir(), "events.jsonl")
	_, result := oneTurn(t, context.Background(), cfg, newChatGPTModel(source, api.server.URL, api.server.Client(), trace), trace, path, "find marker")
	if result.Status != StatusCompleted || result.ToolCalls != 1 || result.Reply != "The marker is marker." {
		t.Fatalf("round trip: %+v", result)
	}
	headers := api.receivedHeaders()
	if len(headers) != 2 || headers[0].Get("Authorization") != "Bearer access-secret" || headers[1].Get("Authorization") != "Bearer access-secret" {
		t.Fatal("missing bearer token")
	}
	id := headers[0].Get("session_id")
	if !regexp.MustCompile(`^[0-9a-f]{8}-[0-9a-f]{4}-5[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$`).MatchString(id) || headers[1].Get("session_id") != id || headers[0].Get("session-id") != "" {
		t.Fatalf("session UUID: %q / %q", id, headers[1].Get("session_id"))
	}
	for _, body := range api.received() {
		if bytes.Contains(body, []byte(`"truncation"`)) || !bytes.Contains(body, []byte(`"stream":true`)) || !bytes.Contains(body, []byte(`"store":false`)) {
			t.Fatalf("wrong encoding: %s", body)
		}
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Contains(raw, []byte(`"request_id":"oai-request"`)) || bytes.Contains(raw, []byte("wrong-request")) || bytes.Contains(raw, []byte("access-secret")) {
		t.Fatal("trace request ID or credential redaction failed")
	}
}

func TestChatGPT_HTTPAndStreamErrorsHaveTheSameRecovery(t *testing.T) {
	cases := []struct {
		code    string
		status  int
		message string
		retry   bool
	}{
		{"subscription_sharing_usage_limit_exceeded", 429, "ChatGPT plan usage limit reached. Manage usage: https://chatgpt.com/settings/usage (or rerun with --openai-auth key)", false},
		{"subscription_sharing_usage_unavailable", 503, "temporarily unavailable", true},
		{"subscription_sharing_user_unavailable", 503, "temporarily unavailable", true},
		{"subscription_sharing_unsupported_capability", 400, "service_tier", false},
		{"subscription_sharing_user_not_eligible", 403, "account, workspace, or policy", false},
		{"subscription_sharing_invalid_user", 401, "subscriber identity", false},
		{"subscription_sharing_route_not_supported", 403, "POST /v1/responses", false},
		{"chatpass_v2_scope_not_authorized", 403, "permission context", false},
		{"chatpass_v2_invalid_authorization_context", 403, "permission context", false},
		{"chatpass_v2_unknown_future_code", 403, "permission context", false},
	}
	for _, tc := range cases {
		for _, shape := range []string{"http", "failed", "error", "nested_error"} {
			t.Run(tc.code+"/"+shape, func(t *testing.T) {
				source := transportChatGPTSource(t)
				errorBody := fmt.Sprintf(`{"code":%q,"message":"provider diagnostic","param":"service_tier"}`, tc.code)
				status, raw := tc.status, `{"error":`+errorBody+`}`
				switch shape {
				case "failed":
					status, raw = 200, "data: {\"type\":\"response.failed\",\"response\":{\"error\":"+errorBody+"}}\n\n"
				case "error":
					status, raw = 200, "data: "+strings.Replace(errorBody, "{", `{"type":"error",`, 1)+"\n\n"
				case "nested_error":
					status, raw = 200, "data: {\"type\":\"error\",\"error\":"+errorBody+"}\n\n"
				}
				api := newFakeAPI(t, apiReply{status: status, body: raw}, okReply(streamedTextReply()))
				model := newChatGPTModel(source, api.server.URL, api.server.Client(), NewTrace(io.Discard))
				model.transport.waitRetry = func(context.Context, time.Duration) error { return nil }
				cfg := testConfig(t)
				cfg.Provider, cfg.Model = openaiName, "test-model"
				req := BuildContext(cfg, RequestScope{Step: 1}, []Entry{{Kind: EntryUser, User: &UserTurn{Text: "hello"}}})
				_, err := model.Generate(context.Background(), req)
				if tc.retry {
					if err != nil || len(api.received()) != 2 {
						t.Fatalf("bounded retry: %v, requests=%d", err, len(api.received()))
					}
					return
				}
				if err == nil || !strings.Contains(err.Error(), tc.message) || len(api.received()) != 1 {
					t.Fatalf("terminal error: %v, requests=%d", err, len(api.received()))
				}
			})
		}
	}
}

func TestChatGPT_UnauthorizedRefreshesAndRetriesSameRequest(t *testing.T) {
	source := transportChatGPTSource(t)
	var refreshes atomic.Int32
	issuer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		refreshes.Add(1)
		r.ParseForm()
		if r.Form.Get("refresh_token") != "refresh-secret" || r.Form.Get("client_id") != "oaiapp_test" {
			t.Error("wrong refresh form")
		}
		io.WriteString(w, `{"access_token":"new-access-secret","refresh_token":"new-refresh-secret","token_type":"Bearer","expires_in":3600,"scope":"chatgpt.tokens.use.direct"}`)
	}))
	defer issuer.Close()
	t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", issuer.URL)
	api := newFakeAPI(t, apiReply{status: 401, body: `{"detail":"expired identity"}`}, okReply(streamedTextReply()))
	cfg := testConfig(t)
	cfg.Provider, cfg.Model = openaiName, "test-model"
	trace := NewTrace(io.Discard)
	_, result := oneTurn(t, context.Background(), cfg, newChatGPTModel(source, api.server.URL, api.server.Client(), trace), trace, filepath.Join(t.TempDir(), "events.jsonl"), "hello")
	if result.Status != StatusCompleted || refreshes.Load() != 1 {
		t.Fatalf("401 recovery: %+v, refreshes=%d", result, refreshes.Load())
	}
	requests := api.received()
	headers := api.receivedHeaders()
	if len(requests) != 2 || !bytes.Equal(requests[0], requests[1]) || headers[1].Get("Authorization") != "Bearer new-access-secret" {
		t.Fatal("401 retry changed body or used old token")
	}
	c, err := loadChatGPTCredentials()
	if err != nil || c.RefreshToken != "new-refresh-secret" {
		t.Fatal("rotation was not persisted")
	}
}

func TestChatGPT_DetailErrorsRedactAllCredentialTokens(t *testing.T) {
	source := transportChatGPTSource(t)
	api := newFakeAPI(t, apiReply{status: 403, body: `{"detail":"policy denied access-secret refresh-secret id-secret"}`, header: map[string]string{"x-oai-request-id": "access-secret refresh-secret id-secret"}})
	cfg := testConfig(t)
	cfg.Provider, cfg.Model = openaiName, "test-model"
	trace := NewTrace(io.Discard)
	path := filepath.Join(t.TempDir(), "events.jsonl")
	_, result := oneTurn(t, context.Background(), cfg, newChatGPTModel(source, api.server.URL, api.server.Client(), trace), trace, path, "hello")
	if !strings.Contains(result.Reason, "policy denied") || strings.Contains(result.Reason, "access-secret") {
		t.Fatalf("detail: %s", result.Reason)
	}
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	for _, secret := range []string{"access-secret", "refresh-secret", "id-secret"} {
		if strings.Contains(result.Reason, secret) || bytes.Contains(raw, []byte(secret)) {
			t.Fatal("credential token reached the error or trace")
		}
	}
}

func TestChatGPT_UsageLimitCheckpointRemainsResumable(t *testing.T) {
	store := testSessionStore(t, NewID())
	source := transportChatGPTSource(t)
	api := newFakeAPI(t, apiReply{status: 429, body: `{"error":{"code":"subscription_sharing_usage_limit_exceeded"}}`}, okReply(streamedTextReply()))
	cfg := testConfig(t)
	cfg.Provider, cfg.Model, cfg.WorkspacePath = openaiName, "test-model", t.TempDir()
	trace := NewTrace(io.Discard)
	model := newChatGPTModel(source, api.server.URL, api.server.Client(), trace)
	session := NewSession(cfg, model, trace, io.Discard)
	session.ID, session.store = filepath.Base(store.dir), store
	result, err := session.Turn(context.Background(), "hello", NewID(), filepath.Join(t.TempDir(), "first.jsonl"))
	if err != nil || result.Status != StatusProviderError || !strings.Contains(result.Reason, "ChatGPT plan usage limit reached") || session.blocked != "" || len(api.received()) != 1 {
		t.Fatalf("usage limit: %+v, %v, blocked=%q", result, err, session.blocked)
	}
	cp, err := store.load(session.ID)
	if err != nil {
		t.Fatal(err)
	}
	restored := NewSession(cfg, model, trace, io.Discard)
	restored.restore(cp)
	restored.store = store
	result, err = restored.Turn(context.Background(), "continue", NewID(), filepath.Join(t.TempDir(), "second.jsonl"))
	if err != nil || result.Status != StatusCompleted {
		t.Fatalf("resume after usage limit: %+v %v", result, err)
	}
}

func TestOpenAI_BaseURLOverrideKeepsExplicitProxyEndpoint(t *testing.T) {
	t.Setenv("REAGENT_OPENAI_BASE_URL", "http://127.0.0.1:1234/v1/")
	model := NewOpenAIModel("key", "", nil, nil)
	if model.transport.endpoint != "http://127.0.0.1:1234/v1/responses" {
		t.Fatal(model.transport.endpoint)
	}
	proxy := NewOpenAIModel("", "http://127.0.0.1:5678/proxy", nil, nil)
	if proxy.transport.endpoint != "http://127.0.0.1:5678/proxy" {
		t.Fatal("override changed explicit endpoint")
	}
	if err := providerError(403, []byte(`{"detail":"readable admission error"}`)); !strings.Contains(err.Error(), "readable admission error") {
		t.Fatal(err)
	}
}

func TestOpenAI_KeyAndProxyRequestBytesMatchOriginMain(t *testing.T) {
	// Golden body from origin/main's encoder (eb2d662), not today's encoder.
	keyBody := `{"model":"test-model","instructions":"system","input":[{"role":"user","content":[{"type":"input_text","text":"hello"}]}],"tools":[],"tool_choice":"auto","parallel_tool_calls":true,"store":false,"include":["reasoning.encrypted_content"],"prompt_cache_key":"9d4e3aeded7e30909522740e28a7fa7d","truncation":"disabled","stream":false}`
	proxyBody := strings.Replace(keyBody, `,"truncation":"disabled","stream":false`, `,"stream":true`, 1)
	for _, mode := range []string{"key", "proxy"} {
		t.Run(mode, func(t *testing.T) {
			response, expected, key := textReply, keyBody, "api-key"
			if mode == "proxy" {
				response, expected, key = streamedTextReply(), proxyBody, ""
			}
			api := newFakeAPI(t, okReply(response))
			model := NewOpenAIModel(key, api.server.URL, api.server.Client(), NewTrace(io.Discard))
			model.proxied = mode == "proxy"
			req := ModelRequest{Model: "test-model", Instructions: "system", History: []Entry{{Kind: EntryUser, User: &UserTurn{Text: "hello"}}}}
			if _, err := model.Generate(context.Background(), req); err != nil {
				t.Fatal(err)
			}
			if string(api.received()[0]) != expected {
				t.Fatalf("request bytes changed from origin/main:\n%s\nwant:\n%s", api.received()[0], expected)
			}
			if mode == "proxy" && (api.receivedHeaders()[0].Get("session-id") != openAICacheKey(req) || api.receivedHeaders()[0].Get("session_id") != "") {
				t.Fatal("legacy proxy session-id header changed")
			}
		})
	}
}

func TestChatGPT_RotationKeepsRejectedTokensRedactedAcrossRetries(t *testing.T) {
	for _, shape := range []string{"http", "stream", "later_retry", "automatic"} {
		t.Run(shape, func(t *testing.T) {
			source := transportChatGPTSource(t)
			c, err := loadChatGPTCredentials()
			if err != nil {
				t.Fatal(err)
			}
			c.AccessToken = "rejected-bearer-123"
			c.RefreshToken, c.IDToken = "original-refresh-789", "original-id-987"
			if shape == "automatic" {
				c.ExpiresAt = time.Now().Add(30 * time.Second).Unix()
			}
			if err := saveChatGPTCredentials(c); err != nil {
				t.Fatal(err)
			}
			var refreshes atomic.Int32
			issuer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				refreshes.Add(1)
				io.WriteString(w, `{"access_token":"replacement-bearer-456","refresh_token":"rotated-refresh-654","id_token":"rotated-id-321","token_type":"Bearer","expires_in":3600,"scope":"chatgpt.tokens.use.direct"}`)
			}))
			defer issuer.Close()
			t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", issuer.URL)
			secrets := []string{"rejected-bearer-123", "replacement-bearer-456", "original-refresh-789", "rotated-refresh-654", "original-id-987", "rotated-id-321"}
			echo := strings.Join(secrets, " ")
			final := apiReply{status: 403, body: fmt.Sprintf(`{"detail":"policy denied %s"}`, echo), header: map[string]string{"x-oai-request-id": "request-" + echo}}
			if shape == "stream" {
				final.status = 200
				final.body = sse(fmt.Sprintf(`{"type":"error","code":"subscription_sharing_unsupported_capability","param":"%s"}`, echo))
			}
			var replies []apiReply
			if shape != "automatic" {
				replies = append(replies, apiReply{status: 401, body: `{"detail":"identity expired"}`})
			}
			if shape == "later_retry" {
				replies = append(replies, apiReply{status: 503, body: `{"detail":"temporarily unavailable"}`})
			}
			replies = append(replies, final)
			api := newFakeAPI(t, replies...)
			cfg := testConfig(t)
			cfg.Provider, cfg.Model = openaiName, "test-model"
			trace := NewTrace(io.Discard)
			path := filepath.Join(t.TempDir(), "events.jsonl")
			model := newChatGPTModel(source, api.server.URL, api.server.Client(), trace)
			model.transport.waitRetry = func(context.Context, time.Duration) error { return nil }
			_, result := oneTurn(t, context.Background(), cfg, model, trace, path, "hello")
			if result.Status != StatusProviderError || refreshes.Load() != 1 || len(api.received()) != len(replies) {
				t.Fatalf("rotation: %+v, refreshes=%d, requests=%d", result, refreshes.Load(), len(api.received()))
			}
			raw, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			for _, secret := range secrets {
				if strings.Contains(result.Reason, secret) || bytes.Contains(raw, []byte(secret)) {
					t.Fatal("credential token reached the error or trace")
				}
			}
			if !strings.Contains(result.Reason, "[redacted]") || !bytes.Contains(raw, []byte(`"request_id":"request-`+strings.TrimSpace(strings.Repeat("[redacted] ", len(secrets)))+`"`)) {
				t.Fatal("redaction discarded diagnostics")
			}
		})
	}
}
