package reagent

import (
	"bytes"
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestReadAPIProxy_BothVariablesOrNeither(t *testing.T) {
	cases := map[string]struct{ url, provider, wantErr string }{
		"neither":          {"", "", ""},
		"local http":       {"http://localhost:8080/v1/responses", "openai", ""},
		"remote https":     {"https://gateway.example/openai/v1/responses", "openai", ""},
		"url only":         {"http://localhost:8080/v1/responses", "", "API_PROXY_PROVIDER is not"},
		"provider only":    {"", "openai", "API_PROXY_URL is not"},
		"other provider":   {"http://localhost:8080/v1/messages", "anthropic", "only openai"},
		"no scheme":        {"localhost:8080/v1/responses", "openai", "full http or https URL"},
		"other scheme":     {"ftp://localhost/v1/responses", "openai", "full http or https URL"},
		"no host":          {"http:///v1/responses", "openai", "full http or https URL"},
		"password is kept": {"https://user:secret@gateway.example/v1/responses", "openai", ""},
	}
	for name, c := range cases {
		t.Run(name, func(t *testing.T) {
			proxy, err := readAPIProxy(c.url, c.provider)
			if c.wantErr == "" {
				if err != nil || proxy.endpoint != c.url {
					t.Fatalf("got %+v, %v", proxy, err)
				}
				return
			}
			if err == nil || !strings.Contains(err.Error(), c.wantErr) {
				t.Fatalf("got %v, want an error containing %q", err, c.wantErr)
			}
		})
	}
	if proxy, _ := readAPIProxy("https://user:secret@gateway.example/v1/responses", "openai"); strings.Contains(proxy.shown(), "secret") {
		t.Fatalf("the shown endpoint exposes the password: %s", proxy.shown())
	}
}

// proxiedRun runs one live OpenAI turn through Main with the proxy set to
// proxyURL, and returns what the run printed and traced.
func proxiedRun(t *testing.T, proxyURL, openAIKey string) (code int, stdout, stderr, trace string) {
	t.Helper()
	t.Setenv("API_PROXY_URL", proxyURL)
	t.Setenv("API_PROXY_PROVIDER", "openai")
	t.Setenv("OPENAI_API_KEY", openAIKey)
	tracePath := filepath.Join(t.TempDir(), "events.jsonl")
	var out, errs bytes.Buffer
	code = Main(context.Background(), []string{"run", "--workspace", t.TempDir(), "--model", "gpt-6-luna",
		"--trace-file", tracePath, "where is the marker?"}, strings.NewReader(""), &out, &errs)
	raw, _ := os.ReadFile(tracePath)
	return code, out.String(), errs.String(), string(raw)
}

// The proxy authenticates on its own: a run needs no OpenAI key, and a key
// that happens to be set is never sent to the proxy.
func TestMain_ProxyReceivesOpenAIRequestsWithoutAKey(t *testing.T) {
	for name, key := range map[string]string{"no key": "", "key set": "sk-real-openai-key"} {
		t.Run(name, func(t *testing.T) {
			api := newFakeAPI(t, okReply(streamedTextReply()))
			code, stdout, stderr, trace := proxiedRun(t, api.server.URL+"/v1/responses", key)
			if code != exitOK || strings.TrimSpace(stdout) != "The marker is marker." {
				t.Fatalf("exit %d, stdout %q, stderr %s", code, stdout, stderr)
			}
			headers := api.receivedHeaders()
			if len(headers) != 1 || headers[0].Get("Authorization") != "" {
				t.Fatalf("the proxy received %d requests, Authorization %q", len(headers), headers[0].Get("Authorization"))
			}
			if !strings.Contains(stderr, "requests go to "+api.server.URL+"/v1/responses (API_PROXY_URL)") {
				t.Fatalf("the header does not name the proxy: %s", stderr)
			}
			if !strings.Contains(trace, `"endpoint":"`+api.server.URL+`/v1/responses"`) {
				t.Fatal("the trace does not record the proxy endpoint")
			}
			if key != "" && strings.Contains(trace, key) {
				t.Fatal("the OpenAI key reached the trace")
			}
		})
	}
}

func TestMain_ProxyPasswordNeverReachesTheTrace(t *testing.T) {
	api := newFakeAPI(t, okReply(streamedTextReply()))
	withPassword := strings.Replace(api.server.URL, "http://", "http://user:hunter2@", 1) + "/v1/responses"
	code, _, stderr, trace := proxiedRun(t, withPassword, "")
	if code != exitOK {
		t.Fatalf("exit %d: %s", code, stderr)
	}
	for where, text := range map[string]string{"trace": trace, "stderr": stderr} {
		if strings.Contains(text, "hunter2") {
			t.Fatalf("the proxy password reached the %s", where)
		}
	}
}

func TestMain_ProxyDoesNotServeAnthropic(t *testing.T) {
	t.Setenv("API_PROXY_URL", "http://localhost:1/v1/responses")
	t.Setenv("API_PROXY_PROVIDER", "openai")
	t.Setenv("ANTHROPIC_API_KEY", "")
	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"run", "--workspace", t.TempDir(), "--model", "claude-haiku-4-5", "x"},
		strings.NewReader(""), &stdout, &stderr)
	if code != exitUsage || !strings.Contains(stderr.String(), "ANTHROPIC_API_KEY is not set") {
		t.Fatalf("exit %d: %s", code, stderr.String())
	}
}

func TestMain_InvalidProxyIsAStartupError(t *testing.T) {
	t.Setenv("API_PROXY_URL", "http://localhost:8080/v1/responses")
	t.Setenv("API_PROXY_PROVIDER", "anthropic")
	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"run", "--workspace", t.TempDir(), "x"},
		strings.NewReader(""), &stdout, &stderr)
	if code != exitUsage || !strings.Contains(stderr.String(), "only openai") {
		t.Fatalf("exit %d: %s", code, stderr.String())
	}
}

func TestConversation_ProxyMakesOpenAIModelsUsable(t *testing.T) {
	c := newConversation(t, "claude-haiku-4-5", "", 0)
	c.keys = map[string]string{openaiName: "", anthropicName: "sk-anthropic"}
	if c.available()[openaiName] {
		t.Fatal("OpenAI was usable with neither a key nor a proxy")
	}
	c.proxy = apiProxy{provider: openaiName, endpoint: "http://localhost:8080/v1/responses"}
	if !c.available()[openaiName] {
		t.Fatal("the proxy did not make OpenAI usable")
	}

	var stderr bytes.Buffer
	c.commandModel("gpt-6-luna", &stderr)
	if !strings.Contains(stderr.String(), "switched to gpt-6-luna (openai via API_PROXY_URL)") {
		t.Fatalf("switch: %s", stderr.String())
	}
	stderr.Reset()
	c.commandStatus(&stderr)
	if !strings.Contains(stderr.String(), "endpoint   http://localhost:8080/v1/responses (API_PROXY_URL)") {
		t.Fatalf("status: %s", stderr.String())
	}
}

// A proxied conversation continues: the second request carries the first
// streamed reply's items back, encrypted reasoning included, in the proxy form.
func TestMain_ProxiedConversationContinuesAcrossSteps(t *testing.T) {
	api := newFakeAPI(t,
		okReply(sse(itemDone(0, streamReasoning), itemDone(1, streamCall), completed(`[]`))),
		okReply(streamedTextReply()))
	t.Setenv("API_PROXY_URL", api.server.URL+"/v1/responses")
	t.Setenv("API_PROXY_PROVIDER", "openai")
	t.Setenv("OPENAI_API_KEY", "")
	var stdout, stderr bytes.Buffer
	code := Main(context.Background(), []string{"run", "--workspace", t.TempDir(),
		"--trace-file", filepath.Join(t.TempDir(), "events.jsonl"), "find the marker"},
		strings.NewReader(""), &stdout, &stderr)
	if code != exitOK {
		t.Fatalf("exit %d: %s", code, stderr.String())
	}
	requests := api.received()
	if len(requests) != 2 {
		t.Fatalf("the proxy received %d requests; stderr: %s", len(requests), stderr.String())
	}
	for i, body := range requests {
		var sent map[string]any
		_ = json.Unmarshal(body, &sent)
		if sent["stream"] != true || sent["truncation"] != nil {
			t.Fatalf("request %d is not in the proxy form: stream %v, truncation %v", i+1, sent["stream"], sent["truncation"])
		}
	}
	if !strings.Contains(string(requests[1]), `"encrypted_content":"opaque-blob"`) || !strings.Contains(string(requests[1]), `"call_id":"call_1"`) {
		t.Fatalf("the second request lost the first reply's items: %s", requests[1])
	}
	if strings.TrimSpace(stdout.String()) != "The marker is marker." {
		t.Fatalf("stdout %q, stderr %s", stdout.String(), stderr.String())
	}
}

// --show-context prints what would really be sent, so with a proxy it prints
// the proxy form, and without one the direct form.
func TestMain_PreviewShowsTheProxyForm(t *testing.T) {
	preview := func() map[string]any {
		t.Helper()
		var stdout, stderr bytes.Buffer
		if code := Main(context.Background(), []string{"run", "--workspace", t.TempDir(), "--model", "gpt-6-luna", "--show-context", "x"},
			strings.NewReader(""), &stdout, &stderr); code != exitOK {
			t.Fatalf("exit %d: %s", code, stderr.String())
		}
		var sent map[string]any
		if err := json.Unmarshal(stdout.Bytes(), &sent); err != nil {
			t.Fatal(err)
		}
		return sent
	}
	t.Setenv("API_PROXY_URL", "")
	t.Setenv("API_PROXY_PROVIDER", "")
	if direct := preview(); direct["stream"] != false || direct["truncation"] != "disabled" {
		t.Fatalf("direct preview: %v %v", direct["stream"], direct["truncation"])
	}
	t.Setenv("API_PROXY_URL", "http://localhost:1/v1/responses")
	t.Setenv("API_PROXY_PROVIDER", "openai")
	if proxied := preview(); proxied["stream"] != true || proxied["truncation"] != nil {
		t.Fatalf("proxied preview: %v %v", proxied["stream"], proxied["truncation"])
	}
}
