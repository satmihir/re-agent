package reagent

import (
	"bytes"
	"context"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func clearHTTPEnvironment(t *testing.T) {
	t.Helper()
	for _, name := range []string{"REAGENT_OPENAI_BASE_URL", "REAGENT_OPENAI_AUTH_ISSUER", "REAGENT_ANTHROPIC_BASE_URL", "REAGENT_JEV_BASE_URL", "REAGENT_RESPONSE_HEADER_TIMEOUT", "REAGENT_RESPONSE_IDLE_TIMEOUT"} {
		t.Setenv(name, "")
	}
}

func TestEndpoints_InvalidConfigurationFailsAtStartupWithoutQuotingURL(t *testing.T) {
	clearHTTPEnvironment(t)
	for _, name := range []string{"REAGENT_OPENAI_BASE_URL", "REAGENT_OPENAI_AUTH_ISSUER", "REAGENT_ANTHROPIC_BASE_URL", "REAGENT_JEV_BASE_URL"} {
		for _, value := range []string{"ftp://user:private@example.invalid", "http://user:private@", "http://user:private@%gh", "relative", "https://example.invalid/v1?secret", "https://example.invalid/v1#fragment"} {
			t.Run(name+"/"+value, func(t *testing.T) {
				t.Setenv(name, value)
				for _, args := range [][]string{{"run", "--show-context", "hello"}, {"auth", "status"}} {
					var out, stderr bytes.Buffer
					code := Main(context.Background(), args, strings.NewReader(""), &out, &stderr)
					if code != exitUsage || !strings.Contains(stderr.String(), name+" must be a full http or https URL") || strings.Contains(stderr.String(), "private") || out.Len() != 0 {
						t.Fatalf("code=%d stdout=%s stderr=%s", code, &out, &stderr)
					}
				}
			})
		}
	}
}

func TestEndpoints_OverridesRouteOnlyTheirOwnCredentials(t *testing.T) {
	clearHTTPEnvironment(t)
	tracePath := filepath.Join(t.TempDir(), "trace.jsonl")
	trace := OpenTrace(tracePath, NewID(), NewID(), io.Discard)
	defer trace.Close()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/openai/v1/responses":
			if r.Header.Get("Authorization") != "Bearer openai-key" || r.Header.Get("x-api-key") != "" {
				t.Error("wrong OpenAI credentials")
			}
			io.WriteString(w, textReply)
		case "/anthropic/v1/messages":
			if r.Header.Get("x-api-key") != "anthropic-key" || r.Header.Get("Authorization") != "Basic dXNlcjpwYXNzd29yZA==" {
				t.Error("wrong Anthropic credentials")
			}
			io.WriteString(w, anthropicTextReply)
		case "/jev/v1/systemone":
			if r.Header.Get("Authorization") != "Bearer jev-key" || r.Header.Get("x-api-key") != "" {
				t.Error("wrong Jev credentials")
			}
			io.WriteString(w, jevReply)
		case "/issuer/.well-known/openid-configuration":
			if r.Header.Get("Authorization") != "Basic dXNlcjpwYXNzd29yZA==" || r.Header.Get("x-api-key") != "" {
				t.Error("wrong issuer credentials")
			}
			io.WriteString(w, `{"issuer":"`+"http://"+r.Host+`/issuer","authorization_endpoint":"`+"http://"+r.Host+`/authorize","token_endpoint":"`+"http://"+r.Host+`/token"}`)
		default:
			t.Errorf("unexpected endpoint %s", r.URL.Path)
		}
	}))
	defer server.Close()
	withPassword := strings.Replace(server.URL, "http://", "http://user:password@", 1)
	t.Setenv("REAGENT_OPENAI_BASE_URL", withPassword+"/openai/v1///")
	t.Setenv("REAGENT_ANTHROPIC_BASE_URL", withPassword+"/anthropic/v1/")
	t.Setenv("REAGENT_JEV_BASE_URL", withPassword+"/jev/v1/")
	t.Setenv("REAGENT_OPENAI_AUTH_ISSUER", withPassword+"/issuer/")
	if err := validateHTTPEnvironment(); err != nil {
		t.Fatal(err)
	}
	req := ModelRequest{Model: "test-model", Instructions: "system", History: []Entry{{Kind: EntryUser, User: &UserTurn{Text: "hello"}}}}
	for _, provider := range []string{openaiName, anthropicName} {
		key := "openai-key"
		if provider == anthropicName {
			key = "anthropic-key"
		}
		if _, err := newLiveModel(provider, key, apiProxy{}, NewHTTPClient(), trace).Generate(context.Background(), req); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := newJevClient("jev-key", "", nil).decide(context.Background(), "task", jevTestRoutes()); err != nil {
		t.Fatal(err)
	}
	if _, err := discoverChatGPT(context.Background(), NewHTTPClient()); err != nil {
		t.Fatal(err)
	}
	trace.Close()
	traced, err := os.ReadFile(tracePath)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(traced), "password") || !strings.Contains(string(traced), "xxxxx") {
		t.Fatalf("unsafe trace: %s", traced)
	}
	if shown := chatGPTVisible(chatGPTIssuer()); strings.Contains(shown, "password") || !strings.Contains(shown, "xxxxx") {
		t.Fatal(shown)
	}
}

func TestEndpoints_ProxyWinsAndNeverReceivesProviderKey(t *testing.T) {
	clearHTTPEnvironment(t)
	t.Setenv("REAGENT_OPENAI_BASE_URL", "http://127.0.0.1:1/unused")
	api := newFakeAPI(t, okReply(streamedTextReply()))
	model := newLiveModel(openaiName, "secret-key", apiProxy{provider: openaiName, endpoint: api.server.URL}, NewHTTPClient(), NewTrace(io.Discard))
	if _, err := model.Generate(context.Background(), ModelRequest{Model: "m"}); err != nil {
		t.Fatal(err)
	}
	if api.receivedHeaders()[0].Get("Authorization") != "" {
		t.Fatal("proxy received provider key")
	}
}

func TestEndpoints_DefaultsAndRequestBytesMatchOriginMain(t *testing.T) {
	clearHTTPEnvironment(t)
	if NewOpenAIModel("", "", nil, nil).transport.endpoint != openAIEndpoint || NewAnthropicModel("", "", nil, nil).transport.endpoint != anthropicEndpoint || newJevClient("", "", nil).endpoint != jevEndpoint || chatGPTIssuer() != "https://auth.openai.com" {
		t.Fatal("default endpoint changed")
	}
	// Golden from origin/main 941e8b6; no endpoint setting changes body encoding.
	want := `{"model":"test-model","max_tokens":16000,"system":[{"type":"text","text":"system","cache_control":{"type":"ephemeral"}}],"messages":[{"role":"user","content":[{"type":"text","text":"hello","cache_control":{"type":"ephemeral"}}]}],"tool_choice":{"type":"auto","disable_parallel_tool_use":false}}`
	api := newFakeAPI(t, okReply(anthropicTextReply))
	model := NewAnthropicModel("key", api.server.URL, NewHTTPClient(), NewTrace(io.Discard))
	if _, err := model.Generate(context.Background(), ModelRequest{Model: "test-model", Instructions: "system", History: []Entry{{Kind: EntryUser, User: &UserTurn{Text: "hello"}}}}); err != nil {
		t.Fatal(err)
	}
	if string(api.received()[0]) != want {
		t.Fatalf("request bytes changed:\n%s\nwant:\n%s", api.received()[0], want)
	}
	jev := newFakeAPI(t, okReply(jevReply))
	questions := map[string]jevQuestion{"route": {Type: "choice", Instructions: "choose", Criteria: map[string]string{"fast": "fast", "capable": "capable"}}}
	if _, _, err := newJevClient("key", jev.server.URL, nil).choose(context.Background(), "task", questions); err != nil {
		t.Fatal(err)
	}
	wantJev := `{"state":"task","model":"jev-1.13.0","questions":{"route":{"type":"choice","instructions":"choose","criteria":{"capable":"capable","fast":"fast"}}}}`
	if string(jev.received()[0]) != wantJev {
		t.Fatalf("Jev request bytes changed: %s", jev.received()[0])
	}
}

func TestHTTPTimeouts_WholeSecondsBoundsAndStartupErrors(t *testing.T) {
	clearHTTPEnvironment(t)
	defaults := NewHTTPClient().Transport.(idleTransport)
	if defaults.idle != 5*time.Minute || defaults.base.(*http.Transport).ResponseHeaderTimeout != 10*time.Minute {
		t.Fatal("default timeouts changed")
	}
	for _, variable := range []string{"REAGENT_RESPONSE_HEADER_TIMEOUT", "REAGENT_RESPONSE_IDLE_TIMEOUT"} {
		for _, value := range []string{"0", "3601", "-1", "+1", "1s", "1.5", " 1", "999999999999999999999999"} {
			t.Run(variable+"/"+value, func(t *testing.T) {
				t.Setenv(variable, value)
				var out, stderr bytes.Buffer
				if code := Main(context.Background(), []string{"auth", "status"}, strings.NewReader(""), &out, &stderr); code != exitUsage || !strings.Contains(stderr.String(), variable+" must be an integer number of seconds from 1 to 3600") {
					t.Fatalf("code=%d stderr=%s", code, &stderr)
				}
			})
		}
	}
	for _, seconds := range []string{"1", "3600"} {
		t.Setenv("REAGENT_RESPONSE_HEADER_TIMEOUT", seconds)
		t.Setenv("REAGENT_RESPONSE_IDLE_TIMEOUT", seconds)
		if err := validateHTTPEnvironment(); err != nil {
			t.Fatal(err)
		}
		transport := NewHTTPClient().Transport.(idleTransport)
		want := time.Second
		if seconds == "3600" {
			want = time.Hour
		}
		if transport.idle != want || transport.base.(*http.Transport).ResponseHeaderTimeout != want {
			t.Fatal("timeouts not applied")
		}
	}
}

func TestEndpoints_InjectedClientsCannotFollowCredentialRedirects(t *testing.T) {
	var reached atomic.Bool
	target := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) { reached.Store(true) }))
	defer target.Close()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, target.URL, http.StatusTemporaryRedirect)
	}))
	defer server.Close()
	for _, model := range []Model{NewOpenAIModel("secret", server.URL, server.Client(), NewTrace(io.Discard)), NewAnthropicModel("secret", server.URL, server.Client(), NewTrace(io.Discard))} {
		if _, err := model.Generate(context.Background(), ModelRequest{Model: "test-model"}); err == nil {
			t.Fatal("redirect accepted")
		}
	}
	if reached.Load() {
		t.Fatal("redirect forwarded credentials")
	}
}

func TestHTTPTimeouts_AuthRequestsUseConfiguredHeaderAndIdleLimits(t *testing.T) {
	clearHTTPEnvironment(t)
	t.Setenv("REAGENT_RESPONSE_HEADER_TIMEOUT", "1")
	t.Setenv("REAGENT_RESPONSE_IDLE_TIMEOUT", "1")
	for _, phase := range []string{"header", "idle"} {
		t.Run(phase, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				io.ReadAll(r.Body)
				if phase == "idle" {
					w.WriteHeader(http.StatusOK)
					w.(http.Flusher).Flush()
				}
				<-r.Context().Done()
			}))
			defer server.Close()
			started := time.Now()
			var result chatGPTTokenSet
			err := chatGPTForm(context.Background(), NewHTTPClient(), server.URL, nil, &result)
			if elapsed := time.Since(started); err == nil || elapsed < 900*time.Millisecond || elapsed > 5*time.Second {
				t.Fatalf("elapsed=%s err=%v", elapsed, err)
			}
		})
	}
}

func TestEndpoints_SignInURLMasksIssuerPassword(t *testing.T) {
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer listener.Close()
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	var out bytes.Buffer
	_, _ = waitChatGPTCallback(ctx, listener, "http://localhost/callback", "state", "client", strings.NewReader(""), &out, func(_ context.Context, address string) error {
		if !strings.Contains(address, "password") {
			t.Error("browser did not receive configured endpoint")
		}
		return nil
	}, "https://user:password@example.invalid/authorize")
	if strings.Contains(out.String(), "password") || !strings.Contains(out.String(), "xxxxx") {
		t.Fatalf("unsafe sign-in URL: %s", &out)
	}
}
