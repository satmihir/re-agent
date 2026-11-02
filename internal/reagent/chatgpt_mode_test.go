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
	"testing"
	"time"
)

func modeCredentials(t *testing.T, scope string) chatGPTCredentials {
	t.Helper()
	setTestHome(t, t.TempDir())
	t.Setenv("XDG_CONFIG_HOME", filepath.Join(os.Getenv("HOME"), ".config"))
	t.Setenv("OPENAI_API_KEY", "")
	t.Setenv("REAGENT_OPENAI_AUTH", "")
	t.Setenv("REAGENT_MODEL", "")
	t.Setenv("API_PROXY_URL", "")
	t.Setenv("API_PROXY_PROVIDER", "")
	c := chatGPTCredentials{ClientID: "oaiapp_test", HostID: "urn:uuid:test", Subject: "account", Email: "test@example.com", AccessToken: "access-mode-secret", RefreshToken: "refresh-mode-secret", IDToken: "id-mode-secret", Scope: scope, ExpiresAt: time.Now().Add(time.Hour).Unix()}
	if err := saveChatGPTCredentials(c); err != nil {
		t.Fatal(err)
	}
	return c
}

func TestChatGPTMode_SelectionAtCLI(t *testing.T) {
	cases := []struct {
		name, key, env, flag, scope, proxy, want string
		code                                     int
	}{
		{"auto plan", "", "", "", "chatgpt.tokens.use.direct", "", "ChatGPT plan", exitOK},
		{"key precedes plan", "key", "", "", "chatgpt.tokens.use.direct", "", "API key", exitOK},
		{"force plan", "key", "key", "chatgpt", "chatgpt.tokens.use.direct", "", "ChatGPT plan", exitOK},
		{"force key", "key", "chatgpt", "key", "chatgpt.tokens.use.direct", "", "API key", exitOK},
		{"environment plan", "key", "chatgpt", "", "chatgpt.tokens.use.direct", "", "ChatGPT plan", exitOK},
		{"declined grant", "", "", "", "openid profile", "", "run reagent login", exitUsage},
		{"forced key missing", "", "", "key", "chatgpt.tokens.use.direct", "", "OPENAI_API_KEY", exitUsage},
		{"forced plan missing scope", "", "", "chatgpt", "openid", "", "grant plan usage", exitUsage},
		{"existing proxy wins", "key", "", "", "chatgpt.tokens.use.direct", "http://127.0.0.1:1/responses", "API proxy", exitOK},
		{"proxy without key", "", "", "", "chatgpt.tokens.use.direct", "http://127.0.0.1:1/responses", "API proxy", exitOK},
		{"explicit key stays proxy", "key", "", "key", "chatgpt.tokens.use.direct", "http://127.0.0.1:1/responses", "API proxy", exitOK},
		{"explicit key proxy needs no key", "", "", "key", "chatgpt.tokens.use.direct", "http://127.0.0.1:1/responses", "API proxy", exitOK},
		{"plan proxy conflict", "key", "", "chatgpt", "chatgpt.tokens.use.direct", "http://127.0.0.1:1/responses", "cannot be combined", exitUsage},
		{"bad mode", "key", "", "bad", "", "", "auto, key or chatgpt", exitUsage},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			modeCredentials(t, test.scope)
			t.Setenv("OPENAI_API_KEY", test.key)
			t.Setenv("REAGENT_OPENAI_AUTH", test.env)
			if test.proxy != "" {
				t.Setenv("API_PROXY_URL", test.proxy)
				t.Setenv("API_PROXY_PROVIDER", "openai")
			}
			args := []string{"chat", "--workspace", t.TempDir()}
			if test.flag != "" {
				args = append(args, "--openai-auth", test.flag)
			}
			var output bytes.Buffer
			code := Main(context.Background(), args, strings.NewReader("/status\n/exit\n"), io.Discard, &output)
			if code != test.code || !strings.Contains(output.String(), test.want) {
				t.Fatalf("code %d, expected %d; output %q, expected %q", code, test.code, output.String(), test.want)
			}
		})
	}
}

func TestChatGPTMode_PreviewAndTraceContainNoCredentials(t *testing.T) {
	credentials := modeCredentials(t, "chatgpt.tokens.use.direct")
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/responses" || r.Header.Get("Authorization") != "Bearer "+credentials.AccessToken {
			t.Errorf("unexpected request path or credential")
		}
		fmt.Fprint(w, streamedTextReply())
	}))
	defer server.Close()
	t.Setenv("REAGENT_OPENAI_BASE_URL", server.URL)
	workspace := t.TempDir()
	var output, progress bytes.Buffer
	if code := Main(context.Background(), []string{"run", "--workspace", workspace, "--show-context", "task"}, strings.NewReader(""), &output, &progress); code != exitOK {
		t.Fatalf("preview %d: %s", code, progress.String())
	}
	if strings.Contains(output.String(), `"truncation"`) || !strings.Contains(output.String(), `"stream":true`) {
		t.Fatalf("preview is not the ChatGPT encoding: %s", output.String())
	}
	preview := output.String()
	output.Reset()
	progress.Reset()
	path := filepath.Join(t.TempDir(), "trace.jsonl")
	if code := Main(context.Background(), []string{"run", "--workspace", workspace, "--trace-file", path, "task"}, strings.NewReader(""), &output, &progress); code != exitOK {
		t.Fatalf("run %d: %s", code, progress.String())
	}
	trace, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(trace), `"auth_mode":"chatgpt"`) {
		t.Fatalf("trace omitted auth mode: %s", trace)
	}
	for _, secret := range []string{credentials.AccessToken, credentials.RefreshToken, credentials.IDToken, "Authorization"} {
		if strings.Contains(preview+output.String()+progress.String()+string(trace), secret) {
			t.Fatalf("credential %q leaked", secret)
		}
	}
}

func TestChatGPTMode_UsageLimitCanResumeWithKey(t *testing.T) {
	modeCredentials(t, "chatgpt.tokens.use.direct")
	attempts := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		attempts++
		if attempts == 1 {
			w.WriteHeader(http.StatusTooManyRequests)
			fmt.Fprint(w, `{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"usage"}}`)
			return
		}
		if r.Header.Get("Authorization") != "Bearer api-key" {
			t.Errorf("resume did not select API key")
		}
		fmt.Fprint(w, `{"id":"r","model":"test","status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"resumed"}]}]}`)
	}))
	defer server.Close()
	t.Setenv("REAGENT_OPENAI_BASE_URL", server.URL)
	var output, progress bytes.Buffer
	args := []string{"chat", "--workspace", t.TempDir()}
	if code := Main(context.Background(), args, strings.NewReader("first\n/exit\n"), &output, &progress); code != exitOK {
		t.Fatalf("chat %d: %s", code, progress.String())
	}
	if attempts != 1 || !strings.Contains(progress.String(), "ChatGPT plan usage limit reached") || strings.Contains(progress.String(), "session blocked") {
		t.Fatalf("usage failure attempts %d: %s", attempts, progress.String())
	}
	id := regexp.MustCompile(`session ID: ([0-9a-f]{16})`).FindStringSubmatch(progress.String())
	if len(id) != 2 {
		t.Fatalf("missing session: %s", progress.String())
	}
	store, err := openSessionStore(id[1], false)
	if err != nil {
		t.Fatal(err)
	}
	saved, err := store.load(id[1])
	store.close()
	if err != nil || saved.Blocked != "" || len(saved.History) != 1 || saved.History[0].User.Text != "first" {
		t.Fatalf("checkpoint lost pending history or became blocked: %+v, %v", saved, err)
	}
	t.Setenv("OPENAI_API_KEY", "api-key")
	output.Reset()
	progress.Reset()
	args = []string{"chat", "--resume", id[1], "--openai-auth", "key"}
	if code := Main(context.Background(), args, strings.NewReader("continue\n/exit\n"), &output, &progress); code != exitOK {
		t.Fatalf("resume %d: %s", code, progress.String())
	}
	if attempts != 2 || !strings.Contains(output.String(), "resumed") || !strings.Contains(progress.String(), "API key") {
		t.Fatalf("resume attempts %d output %s progress %s", attempts, output.String(), progress.String())
	}
}

func TestChatGPTMode_SourceSurvivesAutoAndModelSwitches(t *testing.T) {
	modeCredentials(t, "chatgpt.tokens.use.direct")
	source, err := newChatGPTTokenSource(NewHTTPClient())
	if err != nil {
		t.Fatal(err)
	}
	cfg := testConfig(t)
	cfg.Provider, cfg.Model, cfg.ReasoningEffort = openaiName, "gpt-6.1-sol", "low"
	cfg.chatGPTSource = source
	setConfigAuth(&cfg, apiProxy{})
	auto, err := newAutoRouting(cfg, "", nil, apiProxy{}, NewHTTPClient(), NewTrace(io.Discard))
	if err != nil {
		t.Fatal(err)
	}
	for _, model := range auto.models {
		if model.(*OpenAIModel).transport.tokenSource != source {
			t.Fatal("Auto built a separate token source")
		}
	}
	c := &conversation{session: NewSession(cfg, NewScriptedModel(), NewTrace(io.Discard), io.Discard), keys: map[string]string{anthropicName: "anthropic-key"}, trace: NewTrace(io.Discard), progress: io.Discard}
	if !c.available()[openaiName] {
		t.Fatal("ChatGPT is not available in model listing")
	}
	anthropic, _ := findModel(DefaultAnthropicModel)
	c.switchTo(anthropic)
	if c.session.cfg.AuthMode != "key" || c.session.cfg.chatGPTSource != source {
		t.Fatal("cross-provider switch lost source or billing mode")
	}
	openai, _ := findModel("gpt-6.1-luna")
	c.switchTo(openai)
	if c.session.cfg.AuthMode != "chatgpt" || c.session.model.(*OpenAIModel).transport.tokenSource != source {
		t.Fatal("switch back lost shared token source")
	}
	var status bytes.Buffer
	c.commandStatus(&status)
	if !strings.Contains(status.String(), "ChatGPT plan") || !strings.Contains(c.regionStatus().row(120, false), "ChatGPT plan") {
		t.Fatal("status omitted auth mode")
	}
}

func TestChatGPTMode_AgentsShareSourceAcrossProviders(t *testing.T) {
	s := agentFixture(t)
	source := &chatGPTTokenSource{}
	s.cfg.chatGPTSource = source
	setConfigAuth(&s.cfg, apiProxy{})
	seen := 0
	s.agentModel = func(cfg Config, trace *Trace) (Model, error) {
		seen++
		if cfg.chatGPTSource != source || cfg.AuthMode != "chatgpt" {
			t.Errorf("child lost shared source or auth mode")
		}
		model := newLiveModel(cfg.Provider, "", apiProxy{}, NewHTTPClient(), trace, cfg.chatGPTSource)
		if model.(*OpenAIModel).transport.tokenSource != source {
			t.Errorf("child factory did not share source")
		}
		return NewScriptedModel(turn(textBlock("done"))), nil
	}
	s.model = NewScriptedModel(turn(spawnBlock("spawn", "inspect", "one")), turn(callBlock("wait", "wait", `{"ids":["one"]}`)), turn(textBlock("complete")))
	agentTurn(t, s, "task")
	if seen != 1 {
		t.Fatalf("expected one child, got %d", seen)
	}
}

func TestChatGPTMode_OfflinePreviewDefaultsToKey(t *testing.T) {
	setTestHome(t, t.TempDir())
	for _, name := range []string{"OPENAI_API_KEY", "REAGENT_OPENAI_AUTH", "REAGENT_MODEL", "API_PROXY_URL", "API_PROXY_PROVIDER"} {
		t.Setenv(name, "")
	}
	root := t.TempDir()
	var keyPreview []byte
	for _, mode := range []string{"key", "", "auto", "chatgpt"} {
		var output, progress bytes.Buffer
		args := []string{"run", "--workspace", root, "--show-context"}
		if mode != "" {
			args = append(args, "--openai-auth", mode)
		}
		args = append(args, "task")
		code := Main(context.Background(), args, strings.NewReader(""), &output, &progress)
		if code != exitOK || strings.Contains(output.String(), `"stream":true`) != (mode == "chatgpt") {
			t.Fatalf("%s preview: %d %s %s", mode, code, output.String(), progress.String())
		}
		if mode == "key" {
			keyPreview = append([]byte(nil), output.Bytes()...)
		} else if mode != "chatgpt" && !bytes.Equal(output.Bytes(), keyPreview) {
			t.Fatalf("%s offline preview changed key-mode request bytes", mode)
		}
	}
}

func TestChatGPTMode_InvalidConnectionReportsReauthentication(t *testing.T) {
	credentials := modeCredentials(t, "chatgpt.tokens.use.direct")
	credentials.NeedsLogin = true
	if err := saveChatGPTCredentials(credentials); err != nil {
		t.Fatal(err)
	}
	var progress bytes.Buffer
	for _, mode := range []string{"auto", "chatgpt"} {
		progress.Reset()
		code := Main(context.Background(), []string{"chat", "--workspace", t.TempDir(), "--openai-auth", mode}, strings.NewReader("/exit\n"), io.Discard, &progress)
		if code != exitUsage || !strings.Contains(progress.String(), chatGPTReconnect) || strings.Contains(progress.String(), "OPENAI_API_KEY is not set") {
			t.Fatalf("%s startup %d: %s", mode, code, progress.String())
		}
	}
	t.Setenv("OPENAI_API_KEY", "key")
	progress.Reset()
	code := Main(context.Background(), []string{"chat", "--workspace", t.TempDir()}, strings.NewReader("/exit\n"), io.Discard, &progress)
	if code != exitOK || !strings.Contains(progress.String(), "API key") {
		t.Fatalf("API key should bypass invalid saved login: %d %s", code, progress.String())
	}
	t.Setenv("OPENAI_API_KEY", "")
	t.Setenv("ANTHROPIC_API_KEY", "key")
	progress.Reset()
	code = Main(context.Background(), []string{"chat", "--provider", "anthropic", "--workspace", t.TempDir()}, strings.NewReader("/exit\n"), io.Discard, &progress)
	if code != exitOK {
		t.Fatalf("unrelated provider blocked by stale login: %d %s", code, progress.String())
	}
}

func TestChatGPTMode_ExplicitKeyKeepsProxyKeyless(t *testing.T) {
	modeCredentials(t, "chatgpt.tokens.use.direct")
	t.Setenv("OPENAI_API_KEY", "must-not-reach-proxy")
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests++
		if r.Header.Get("Authorization") != "" || r.Header.Get("session-id") == "" || r.Header.Get("session_id") != "" {
			t.Error("explicit key altered keyless proxy headers")
		}
		fmt.Fprint(w, streamedTextReply())
	}))
	defer server.Close()
	t.Setenv("API_PROXY_URL", server.URL)
	t.Setenv("API_PROXY_PROVIDER", "openai")
	var progress bytes.Buffer
	code := Main(context.Background(), []string{"run", "--workspace", t.TempDir(), "--openai-auth", "key", "task"}, strings.NewReader(""), io.Discard, &progress)
	if code != exitOK || requests != 1 {
		t.Fatalf("proxy run %d with %d requests: %s", code, requests, progress.String())
	}
}

func TestChatGPTMode_AnthropicKeyVisibleAtStartupAndAfterSwitch(t *testing.T) {
	modeCredentials(t, "")
	t.Setenv("ANTHROPIC_API_KEY", "anthropic-test-key")
	t.Setenv("OPENAI_API_KEY", "openai-test-key")
	var output, progress bytes.Buffer
	input := "/status\n/model gpt-6.1-luna fresh\n/status\n/model " + DefaultAnthropicModel + " fresh\n/status\n/exit\n"
	code := Main(context.Background(), []string{"chat", "--provider", anthropicName, "--workspace", t.TempDir()}, strings.NewReader(input), &output, &progress)
	if code != exitOK {
		t.Fatalf("chat exit %d: %s", code, progress.String())
	}
	if !strings.Contains(progress.String(), "anthropic · API key") || strings.Count(progress.String(), "(anthropic · API key)") != 2 || !strings.Contains(progress.String(), "(openai · API key)") {
		t.Fatalf("startup/switch auth labels disagree: %s", progress.String())
	}
}
