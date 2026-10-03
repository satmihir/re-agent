package reagent

import (
	"bytes"
	"context"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"regexp"
	"strings"
	"testing"
)

func TestChat_ResumeRestoresNativeHistoryAndID(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("OPENAI_API_KEY", "")
	t.Setenv("API_PROXY_PROVIDER", "openai")
	var requests []string
	replies := []string{
		sse(itemDone(0, streamReasoning), itemDone(1, `{"type":"function_call","id":"fc_1","status":"completed","call_id":"call_1","name":"list_files","arguments":"{\"path\":\".\"}"}`), completed(`[]`)),
		streamedTextReply(), streamedTextReply(),
	}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		requests = append(requests, string(body))
		w.Header().Set("Content-Type", "text/event-stream")
		if len(requests) > len(replies) {
			http.Error(w, "unexpected", 500)
			return
		}
		fmt.Fprint(w, replies[len(requests)-1])
	}))
	defer server.Close()
	t.Setenv("API_PROXY_URL", server.URL)
	root := t.TempDir()
	var out, errOut bytes.Buffer
	args := []string{"chat", "--workspace", root}
	if code := Main(context.Background(), args, strings.NewReader("before\n/exit\n"), &out, &errOut); code != exitOK {
		t.Fatalf("first %d: %s", code, errOut.String())
	}
	id := regexp.MustCompile(`session ID: ([0-9a-f]{16})`).FindStringSubmatch(errOut.String())
	if len(id) != 2 || strings.Contains(out.String(), "session ID") {
		t.Fatalf("welcome: stdout %q stderr %q", out.String(), errOut.String())
	}
	out.Reset()
	errOut.Reset()
	if code := Main(context.Background(), []string{"chat", "--resume", id[1]}, strings.NewReader("after\n/exit\n"), &out, &errOut); code != exitOK {
		t.Fatalf("resume %d: %s", code, errOut.String())
	}
	if len(requests) != 3 || !strings.Contains(errOut.String(), "session ID: "+id[1]) {
		t.Fatalf("requests %d; welcome %q", len(requests), errOut.String())
	}
	body := requests[2]
	at := 0
	for _, item := range []string{"before", "opaque-blob", "call_1", "list_files", "after"} {
		next := strings.Index(body[at:], item)
		if next < 0 {
			t.Fatalf("missing %q in order: %s", item, body)
		}
		at += next + len(item)
	}
	if !strings.Contains(out.String(), "The marker is marker.") {
		t.Fatalf("reply: %q", out.String())
	}
}

func TestChat_ResumeRejectsConflictsAndMissingCredentials(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("OPENAI_API_KEY", "test")
	root := t.TempDir()
	var out, errOut bytes.Buffer
	if code := Main(context.Background(), []string{"chat", "--workspace", root}, strings.NewReader("/exit\n"), &out, &errOut); code != exitOK {
		t.Fatalf("create %d: %s", code, errOut.String())
	}
	id := regexp.MustCompile(`session ID: ([0-9a-f]{16})`).FindStringSubmatch(errOut.String())[1]
	for _, flag := range []string{"--model=gpt-6-sol", "--provider=openai", "--auto", "--workspace=" + root, "--plan", "--scripted=x", "--read-only"} {
		errOut.Reset()
		if code := Main(context.Background(), []string{"chat", "--resume", id, flag}, strings.NewReader(""), &out, &errOut); code != exitUsage || !strings.Contains(errOut.String(), "--resume cannot be combined") {
			t.Fatalf("%s: %d %s", flag, code, errOut.String())
		}
	}
	t.Setenv("OPENAI_API_KEY", "")
	errOut.Reset()
	if code := Main(context.Background(), []string{"chat", "--resume", id}, strings.NewReader(""), &out, &errOut); code != exitUsage || !strings.Contains(errOut.String(), "OPENAI_API_KEY") {
		t.Fatalf("missing credential %d %s", code, errOut.String())
	}
}

func TestChat_ResumeStartsAtLaunchNotLastActiveWorkspace(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("OPENAI_API_KEY", "test")
	launch, elsewhere := t.TempDir(), t.TempDir()
	var out, errOut bytes.Buffer
	if code := Main(context.Background(), []string{"chat", "--workspace", launch}, strings.NewReader("/exit\n"), &out, &errOut); code != exitOK {
		t.Fatalf("create %d: %s", code, errOut.String())
	}
	id := regexp.MustCompile(`session ID: ([0-9a-f]{16})`).FindStringSubmatch(errOut.String())[1]
	st, err := openSessionStore(id, false)
	if err != nil {
		t.Fatal(err)
	}
	cp, err := st.load(id)
	if err != nil {
		t.Fatal(err)
	}
	launch = cp.Launch
	cp.Active = elsewhere
	if err := st.save(cp); err != nil {
		t.Fatal(err)
	}
	st.close()
	out.Reset()
	errOut.Reset()
	if code := Main(context.Background(), []string{"chat", "--resume", id}, strings.NewReader("/exit\n"), &out, &errOut); code != exitOK || !strings.Contains(errOut.String(), "last active workspace was "+elsewhere) || !strings.Contains(errOut.String(), "workspace "+launch) {
		t.Fatalf("resume %d: %s", code, errOut.String())
	}
}
