package reagent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestTransport_NeutralFailureTextKeepsDiagnosticsOnlyInTrace(t *testing.T) {
	for _, retry := range []bool{false, true} {
		t.Run(map[bool]string{false: "single", true: "retry_exhausted"}[retry], func(t *testing.T) {
			endpoint := "http://user:private@example.invalid/v1/responses"
			tracePath := filepath.Join(t.TempDir(), "trace.jsonl")
			trace := OpenTrace(tracePath, NewID(), NewID(), io.Discard)
			defer trace.Close()
			client := &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
				return nil, &net.OpError{Op: "dial", Net: "tcp", Err: errors.New("implementation-specific diagnostic " + endpoint)}
			})}
			tr := &transport{endpoint: endpoint, client: client, trace: trace}
			ctx := context.Background()
			if retry {
				ctx = withModelRetry(ctx, time.Millisecond, nil)
			}
			_, _, err := tr.call(ctx, 1, []byte(`{}`), nil)
			if err == nil || !strings.Contains(err.Error(), "connection or response read failed") || strings.Contains(err.Error(), "diagnostic") || strings.Contains(err.Error(), "private") {
				t.Fatalf("unstable error: %v", err)
			}
			if retry && !strings.Contains(err.Error(), "retry window exhausted") {
				t.Fatalf("did not exercise retry exhaustion: %v", err)
			}
			trace.Close()
			raw, err := os.ReadFile(tracePath)
			if err != nil {
				t.Fatal(err)
			}
			if !strings.Contains(string(raw), "implementation-specific diagnostic") || strings.Contains(string(raw), "private") {
				t.Fatalf("trace lost diagnostics or leaked password: %s", raw)
			}
		})
	}
}

func TestChatGPT_MissingCredentialDirectoryIsNotAnError(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("XDG_CONFIG_HOME", filepath.Join(t.TempDir(), "absent"))
	if _, err := loadChatGPTCredentials(); err != nil {
		t.Fatal(err)
	}
}

func TestChatGPT_CredentialFilesystemErrorsHaveOwnedWording(t *testing.T) {
	t.Setenv("HOME", t.TempDir())
	t.Setenv("XDG_CONFIG_HOME", t.TempDir())
	path, err := chatGPTCredentialPath()
	if err != nil {
		t.Fatal(err)
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink("missing", path); err != nil {
		t.Fatal(err)
	}
	_, err = loadChatGPTCredentials()
	if err == nil || err.Error() != "open ChatGPT credentials: too many symbolic links" {
		t.Fatalf("got %v", err)
	}
}

func TestJev_ResponseReadErrorsHaveOwnedWording(t *testing.T) {
	client := &http.Client{Transport: roundTripFunc(func(*http.Request) (*http.Response, error) {
		return &http.Response{StatusCode: 200, Body: io.NopCloser(errorReader{})}, nil
	})}
	_, err := newJevClient("key", "https://example.invalid/v1/systemone", client).decide(context.Background(), "task", jevTestRoutes())
	if err == nil || err.Error() != "read Jev response: unexpected end of input" {
		t.Fatalf("got %v", err)
	}
}

type errorReader struct{}

func (errorReader) Read([]byte) (int, error) { return 0, io.ErrUnexpectedEOF }

func TestTransport_ShortEndpointPasswordDoesNotAlterProviderContent(t *testing.T) {
	api := newFakeAPI(t, okReply(textReply))
	endpoint := strings.Replace(api.server.URL, "http://", "http://user:a@", 1)
	model := NewOpenAIModel("key", endpoint, NewHTTPClient(), NewTrace(io.Discard))
	response, err := model.Generate(context.Background(), ModelRequest{Model: "m"})
	if err != nil || len(response.Blocks) != 1 || response.Blocks[0].Text != "The marker is marker." {
		t.Fatalf("response=%+v err=%v", response, err)
	}
}

func TestTransport_EscapedEndpointPasswordsAreMasked(t *testing.T) {
	for _, escape := range []string{"slashes", "unicode", "encoded-password", "different-path"} {
		t.Run(escape, func(t *testing.T) {
			var endpoint string
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				echo := endpoint
				if escape == "different-path" {
					echo = strings.TrimSuffix(endpoint, "/v1/messages") + "/other"
				}
				raw, _ := json.Marshal(echo)
				text := string(raw)
				if escape == "slashes" {
					text = strings.ReplaceAll(text, "/", `\/`)
				} else if escape == "unicode" {
					text = strings.ReplaceAll(text, "private", `\u0070rivate`)
				}
				w.WriteHeader(http.StatusBadRequest)
				fmt.Fprintf(w, `{"error":{"message":%s}}`, text)
			}))
			defer server.Close()
			endpoint = strings.Replace(server.URL, "http://", "http://user:private&pass@", 1) + "/v1/messages"
			if escape == "encoded-password" {
				endpoint = strings.ReplaceAll(endpoint, "private", "pr%69vate")
			}
			tracePath := filepath.Join(t.TempDir(), "trace.jsonl")
			trace := OpenTrace(tracePath, NewID(), NewID(), io.Discard)
			defer trace.Close()
			body, _ := json.Marshal(map[string]string{"echo": endpoint})
			tr := transport{endpoint: endpoint, client: NewHTTPClient(), trace: trace}
			status, response, err := tr.call(context.Background(), 1, body, nil)
			if err != nil {
				t.Fatal(err)
			}
			err = providerError(status, response)
			if err == nil || strings.Contains(err.Error(), "private") || !strings.Contains(err.Error(), "xxxxx") {
				t.Fatalf("unmasked or missing error: %v", err)
			}
			trace.Close()
			recorded, err := os.ReadFile(tracePath)
			if err != nil {
				t.Fatal(err)
			}
			for _, line := range strings.Split(strings.TrimSpace(string(recorded)), "\n") {
				var event struct {
					Data struct {
						RequestBody  string `json:"request_body"`
						ResponseBody string `json:"response_body"`
					} `json:"data"`
				}
				if err := json.Unmarshal([]byte(line), &event); err != nil {
					t.Fatal(err)
				}
				for _, raw := range []string{event.Data.RequestBody, event.Data.ResponseBody} {
					if raw == "" {
						continue
					}
					var value any
					if err := json.Unmarshal([]byte(raw), &value); err != nil {
						t.Fatal(err)
					}
					if text := fmt.Sprint(value); strings.Contains(text, "private") || !strings.Contains(text, "xxxxx") {
						t.Fatalf("unmasked trace data: %s", raw)
					}
				}
			}
		})
	}
}
