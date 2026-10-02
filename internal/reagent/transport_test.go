package reagent

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

// drippingServer sends its reply in chunks with a pause before each, flushing
// every chunk, the way a streamed reply arrives.
func drippingServer(t *testing.T, pause time.Duration, chunks ...string) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		io.ReadAll(r.Body)
		w.WriteHeader(http.StatusOK)
		w.(http.Flusher).Flush()
		for _, chunk := range chunks {
			select {
			case <-time.After(pause):
			case <-r.Context().Done():
				return
			}
			io.WriteString(w, chunk)
			w.(http.Flusher).Flush()
		}
	}))
	t.Cleanup(server.Close)
	return server
}

// A reply that keeps arriving is read to the end, however long it takes in
// total: here six times the idle bound.
func TestTransport_SteadyReplyOutlastsTheIdleBound(t *testing.T) {
	chunks := strings.Split("a,b,c,d,e,f,g,h,i,j,k,l", ",")
	server := drippingServer(t, 25*time.Millisecond, chunks...)
	tr := &transport{endpoint: server.URL, client: newHTTPClient(time.Second, 50*time.Millisecond), trace: NewTrace(io.Discard)}

	started := time.Now()
	status, raw, err := tr.call(context.Background(), 1, []byte("{}"), nil)
	if err != nil || status != http.StatusOK || string(raw) != strings.Join(chunks, "") {
		t.Fatalf("status %d, body %q, err %v", status, raw, err)
	}
	if elapsed := time.Since(started); elapsed < 6*50*time.Millisecond {
		t.Fatalf("finished in %s; the test did not outlast the idle bound", elapsed)
	}
}

// A reply that stops arriving ends the attempt with a reason that says so, and
// is not retried.
func TestTransport_StalledReplyEndsTheAttempt(t *testing.T) {
	var attempts atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		attempts.Add(1)
		io.ReadAll(r.Body)
		w.WriteHeader(http.StatusOK)
		io.WriteString(w, "event: response.created\n\n")
		w.(http.Flusher).Flush()
		<-r.Context().Done()
	}))
	t.Cleanup(server.Close)
	tr := &transport{endpoint: server.URL, client: newHTTPClient(time.Second, 50*time.Millisecond), trace: NewTrace(io.Discard)}

	_, _, err := tr.call(context.Background(), 1, []byte("{}"), nil)
	me, ok := err.(*ModelError)
	if !ok || me.Status != StatusProviderError || !strings.Contains(me.Message, "the provider sent nothing for 50ms") {
		t.Fatalf("got %v", err)
	}
	if n := attempts.Load(); n != 1 {
		t.Fatalf("made %d attempts, want 1", n)
	}
}

// Cancelling still ends a slow reply at once, and says it was cancelled.
func TestTransport_CancelDuringAReply(t *testing.T) {
	server := drippingServer(t, time.Second, "never")
	tr := &transport{endpoint: server.URL, client: NewHTTPClient(), trace: NewTrace(io.Discard)}
	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()

	_, _, err := tr.call(ctx, 1, []byte("{}"), nil)
	if me, ok := err.(*ModelError); !ok || me.Status != StatusCancelled {
		t.Fatalf("got %v", err)
	}
}
