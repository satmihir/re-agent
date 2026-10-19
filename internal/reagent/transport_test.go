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
	// Leave scheduling headroom under -race while still exceeding the idle bound.
	const idle = 200 * time.Millisecond
	chunks := strings.Split(strings.Repeat("abcdefghijkl", 4), "")
	server := drippingServer(t, 25*time.Millisecond, chunks...)
	tr := &transport{endpoint: server.URL, client: newHTTPClient(time.Second, idle), trace: NewTrace(io.Discard)}

	started := time.Now()
	status, raw, err := tr.call(context.Background(), 1, []byte("{}"), nil)
	if err != nil || status != http.StatusOK || string(raw) != strings.Join(chunks, "") {
		t.Fatalf("status %d, body %q, err %v", status, raw, err)
	}
	if elapsed := time.Since(started); elapsed < 6*idle {
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

func TestTransport_OvernightRetryWindowExpiresWithoutAnExtraAttempt(t *testing.T) {
	var attempts atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		attempts.Add(1)
		w.Header().Set("Retry-After", "60")
		w.WriteHeader(http.StatusTooManyRequests)
		io.WriteString(w, `{"error":{"message":"quota temporarily unavailable"}}`)
	}))
	t.Cleanup(server.Close)
	tr := &transport{endpoint: server.URL, client: server.Client(), trace: NewTrace(io.Discard)}
	start := time.Now()
	ctx := withModelRetry(context.Background(), 80*time.Millisecond, nil)
	_, _, err := tr.call(ctx, 1, []byte(`{}`), nil)
	me, ok := err.(*ModelError)
	if !ok || me.Status != StatusProviderError || !strings.Contains(me.Message, "after 1 attempts") || !strings.Contains(me.Message, "quota temporarily unavailable") || attempts.Load() != 1 {
		t.Fatalf("attempts %d, error %v", attempts.Load(), err)
	}
	if time.Since(start) > time.Second {
		t.Fatal("Retry-After outlived the retry window")
	}
}

func TestTransport_OvernightRetryCancellationDuringWaitAndRequest(t *testing.T) {
	for _, tc := range []struct {
		name    string
		handler http.HandlerFunc
	}{
		{"wait", func(w http.ResponseWriter, _ *http.Request) { w.WriteHeader(http.StatusServiceUnavailable) }},
		{"request", func(w http.ResponseWriter, r *http.Request) {
			io.Copy(io.Discard, r.Body)
			w.WriteHeader(http.StatusOK)
			w.(http.Flusher).Flush()
			<-r.Context().Done()
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			server := httptest.NewServer(tc.handler)
			t.Cleanup(server.Close)
			tr := &transport{endpoint: server.URL, client: server.Client(), trace: NewTrace(io.Discard)}
			ctx, cancel := context.WithTimeout(withModelRetry(context.Background(), 4*time.Second, nil), 70*time.Millisecond)
			defer cancel()
			start := time.Now()
			_, _, err := tr.call(ctx, 1, []byte(`{}`), nil)
			if me, ok := err.(*ModelError); !ok || me.Status != StatusCancelled || time.Since(start) > time.Second {
				t.Fatalf("elapsed %s, error %v", time.Since(start), err)
			}
		})
	}
}

func TestTransport_OvernightRetryConnectionFailureIsRetried(t *testing.T) {
	var attempts atomic.Int32
	client := &http.Client{Transport: roundTripFunc(func(r *http.Request) (*http.Response, error) {
		if attempts.Add(1) == 1 {
			return nil, io.ErrUnexpectedEOF
		}
		return &http.Response{StatusCode: 200, Body: io.NopCloser(strings.NewReader("ok")), Header: make(http.Header)}, nil
	})}
	tr := &transport{endpoint: "http://localhost/model", client: client, trace: NewTrace(io.Discard), waitRetry: recordRetryWait(t, true, time.Second)}
	status, raw, err := tr.call(withModelRetry(context.Background(), 3*time.Second, nil), 1, []byte(`{}`), nil)
	if err != nil || status != 200 || string(raw) != "ok" || attempts.Load() != 2 {
		t.Fatalf("status %d body %s attempts %d error %v", status, raw, attempts.Load(), err)
	}
}

type roundTripFunc func(*http.Request) (*http.Response, error)

func (f roundTripFunc) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

func TestTransport_OvernightWindowDoesNotCutOffInFlightAttempt(t *testing.T) {
	for _, tc := range []struct {
		name       string
		window     time.Duration
		replyDelay time.Duration
		firstFault bool
		want       int32
	}{
		{"first attempt", 60 * time.Millisecond, 120 * time.Millisecond, false, 1},
		{"retry outlasts window", 200 * time.Millisecond, 250 * time.Millisecond, true, 2},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var attempts atomic.Int32
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				io.Copy(io.Discard, r.Body)
				if attempts.Add(1) == 1 && tc.firstFault {
					w.WriteHeader(http.StatusServiceUnavailable)
					return
				}
				time.Sleep(tc.replyDelay)
				io.WriteString(w, "ok")
			}))
			t.Cleanup(server.Close)
			tr := &transport{endpoint: server.URL, client: server.Client(), trace: NewTrace(io.Discard)}
			var delays []time.Duration
			if tc.firstFault {
				tr.waitRetry = func(ctx context.Context, delay time.Duration) error {
					delays = append(delays, delay)
					if delay <= 0 || delay > tc.window {
						t.Fatalf("delay %s, want within %s window", delay, tc.window)
					}
					return waitBeforeRetry(ctx, 50*time.Millisecond)
				}
			}
			start := time.Now()
			status, raw, err := tr.call(withModelRetry(context.Background(), tc.window, nil), 1, []byte(`{}`), nil)
			if len(delays) != int(tc.want)-1 {
				t.Fatalf("retry delays %v, want %d waits", delays, tc.want-1)
			}
			if err != nil || status != http.StatusOK || string(raw) != "ok" || attempts.Load() != tc.want || time.Since(start) <= tc.window {
				t.Fatalf("after %s: status %d, body %q, attempts %d, error %v", time.Since(start), status, raw, attempts.Load(), err)
			}
		})
	}
}

// recordRetryWait skips only the wait; requests still cross the HTTP boundary.
func recordRetryWait(t *testing.T, jitter bool, want ...time.Duration) func(context.Context, time.Duration) error {
	t.Helper()
	var delays []time.Duration
	t.Cleanup(func() {
		if len(delays) != len(want) {
			t.Errorf("retry delays %v, want %d waits", delays, len(want))
		}
	})
	return func(ctx context.Context, delay time.Duration) error {
		t.Helper()
		index := len(delays)
		delays = append(delays, delay)
		if index >= len(want) {
			t.Fatalf("unexpected retry delay %s", delay)
		}
		low, high := want[index], want[index]
		if jitter {
			low, high = low*8/10, high*12/10
		}
		if delay < low || delay > high {
			t.Fatalf("retry %d delay %s, want [%s, %s]", index+1, delay, low, high)
		}
		return ctx.Err()
	}
}

func TestTransport_DefaultRetryWaitThenSucceeds(t *testing.T) {
	api := newFakeAPI(t, apiReply{status: 503, body: "busy"}, apiReply{status: 200, body: "ok"})
	tr := &transport{endpoint: api.server.URL, client: api.server.Client(), trace: NewTrace(io.Discard)}
	start := time.Now()
	status, raw, err := tr.call(context.Background(), 1, []byte(`{}`), nil)
	if err != nil || status != 200 || string(raw) != "ok" || len(api.received()) != 2 || time.Since(start) < 500*time.Millisecond {
		t.Fatalf("after %s: status %d, body %q, attempts %d, error %v", time.Since(start), status, raw, len(api.received()), err)
	}
}

func TestTransport_RetryDelaysAndAttemptLimit(t *testing.T) {
	date := time.Now().Add(30 * time.Second).UTC().Truncate(time.Second)
	for _, tc := range []struct {
		name      string
		overnight bool
		header    string
		jitter    bool
		delays    []time.Duration
	}{
		{"ordinary stops at two", false, "60", false, []time.Duration{500 * time.Millisecond}},
		{"overnight backoff and cap", true, "", true, []time.Duration{time.Second, 2 * time.Second, 4 * time.Second, 8 * time.Second, 16 * time.Second, 32 * time.Second, 64 * time.Second, 2 * time.Minute, 2 * time.Minute}},
		{"integer Retry-After", true, "60", false, []time.Duration{time.Minute}},
		{"date Retry-After", true, date.Format(http.TimeFormat), true, []time.Duration{30 * time.Second}},
		{"padded Retry-After", true, " 60 ", false, []time.Duration{time.Minute}},
		// delay-seconds has no sign; an invalid header leaves the ordinary jittered backoff.
		{"signed Retry-After", true, "+60", true, []time.Duration{time.Second}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			replies := make([]apiReply, len(tc.delays)+1)
			for i := range replies {
				replies[i] = apiReply{status: 503, body: "busy", header: map[string]string{"Retry-After": tc.header}}
			}
			if tc.overnight {
				replies[len(replies)-1] = apiReply{status: 200, body: "ok"}
			}
			api := newFakeAPI(t, replies...)
			tr := &transport{endpoint: api.server.URL, client: api.server.Client(), trace: NewTrace(io.Discard), waitRetry: recordRetryWait(t, tc.jitter, tc.delays...)}
			if tc.name == "date Retry-After" {
				check := tr.waitRetry
				tr.waitRetry = func(ctx context.Context, delay time.Duration) error {
					if remaining := time.Until(date); delay < remaining || delay > remaining+time.Second {
						t.Fatalf("date delay %s, remaining %s", delay, remaining)
					}
					return check(ctx, delay)
				}
			}
			ctx := context.Background()
			wantStatus, wantBody, wantUsage := 503, "busy", Usage{Known: true}
			if tc.overnight {
				ctx = withModelRetry(ctx, time.Hour, nil)
				wantStatus, wantBody = 200, "ok"
				wantUsage.UnreportedAttempts = len(tc.delays)
			}
			status, raw, usage, err := tr.callModel(ctx, 1, []byte(`{}`), nil, nil)
			if err != nil || status != wantStatus || string(raw) != wantBody || usage != wantUsage || len(api.received()) != len(replies) {
				t.Fatalf("status %d body %q usage %+v attempts %d error %v", status, raw, usage, len(api.received()), err)
			}
		})
	}
}

func TestTransport_RetryWaitCancellationRetainsUsage(t *testing.T) {
	for _, tc := range []struct {
		name      string
		overnight bool
		stream    bool
	}{
		{"ordinary", false, false},
		{"overnight unreported", true, false},
		{"overnight stream", true, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			status := 503
			if tc.stream {
				status = 200
			}
			api := newFakeAPI(t, apiReply{status: status, body: "busy"}, apiReply{status: 200, body: "ok"})
			ctx, cancel := context.WithCancel(context.Background())
			defer cancel()
			wantUsage, wantErrorUsage := Usage{Known: true}, Usage{}
			wantDelay := 500 * time.Millisecond
			if tc.overnight {
				ctx = withModelRetry(ctx, time.Hour, nil)
				wantDelay = time.Second
				wantUsage.UnreportedAttempts = 1
				if tc.stream {
					wantUsage = Usage{Known: true, InputTokens: 10, OutputTokens: 5}
				}
				wantErrorUsage = wantUsage
			}
			check := recordRetryWait(t, tc.overnight, wantDelay)
			tr := &transport{endpoint: api.server.URL, client: api.server.Client(), trace: NewTrace(io.Discard), waitRetry: func(ctx context.Context, delay time.Duration) error {
				check(ctx, delay)
				cancel()
				return ctx.Err()
			}}
			_, _, usage, err := tr.callModel(ctx, 1, []byte(`{}`), nil, func([]byte) (string, Usage) { return "provider stream overload", wantUsage })
			me, ok := err.(*ModelError)
			if !ok || me.Status != StatusCancelled || me.Message != "cancelled before a retry" || me.Usage != wantErrorUsage || usage != wantUsage || len(api.received()) != 1 {
				t.Fatalf("usage %+v attempts %d error %+v", usage, len(api.received()), err)
			}
		})
	}
}
