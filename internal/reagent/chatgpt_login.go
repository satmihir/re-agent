package reagent

import (
	"bufio"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"os/signal"
	"runtime"
	"strings"
	"sync"
	"time"
)

func chatGPTRandom() (string, error) {
	var bytes [32]byte
	if _, err := rand.Read(bytes[:]); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(bytes[:]), nil
}

func chatGPTHostID() (string, error) {
	var bytes [16]byte
	if _, err := rand.Read(bytes[:]); err != nil {
		return "", err
	}
	bytes[6] = bytes[6]&0x0f | 0x40
	bytes[8] = bytes[8]&0x3f | 0x80
	return fmt.Sprintf("urn:uuid:%x-%x-%x-%x-%x", bytes[:4], bytes[4:6], bytes[6:8], bytes[8:10], bytes[10:]), nil
}

func openChatGPTBrowser(ctx context.Context, address string) error {
	name := "xdg-open"
	if runtime.GOOS == "darwin" {
		name = "open"
	}
	ctx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	return exec.CommandContext(ctx, name, address).Run()
}

type chatGPTDiscovery struct {
	Issuer    string `json:"issuer"`
	Authorize string `json:"authorization_endpoint"`
	Token     string `json:"token_endpoint"`
}

func discoverChatGPT(ctx context.Context, client *http.Client) (chatGPTDiscovery, error) {
	var d chatGPTDiscovery
	issuer := chatGPTIssuer()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, issuer+"/.well-known/openid-configuration", nil)
	if err != nil {
		return d, fmt.Errorf("invalid ChatGPT authorization issuer")
	}
	response, err := client.Do(req)
	if err != nil {
		return d, fmt.Errorf("could not discover ChatGPT authorization service")
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK || json.NewDecoder(io.LimitReader(response.Body, 1<<20)).Decode(&d) != nil {
		return d, fmt.Errorf("invalid ChatGPT authorization discovery response")
	}
	if d.Issuer == "" {
		d.Issuer = issuer
	}
	base, err := url.Parse(issuer)
	if err != nil || base.Host == "" || (base.Scheme != "https" && base.Scheme != "http") {
		return d, fmt.Errorf("invalid ChatGPT authorization issuer")
	}
	for _, endpoint := range []string{d.Issuer, d.Authorize, d.Token} {
		u, err := url.Parse(endpoint)
		if err != nil || u.Host != base.Host || u.Scheme != base.Scheme || u.User != nil {
			return d, fmt.Errorf("ChatGPT discovery endpoint does not match the configured issuer")
		}
	}
	return d, nil
}

func validateChatGPTIDToken(token, issuer, clientID, nonce, subject string) (string, string, error) {
	// OIDC Core §3.1.3.7 permits skipping signature verification for an ID token
	// obtained directly from the token endpoint over TLS. Claims still bind this login.
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return "", "", fmt.Errorf("invalid ChatGPT ID token")
	}
	body, err := base64.RawURLEncoding.DecodeString(parts[1])
	var claims struct {
		Issuer   string          `json:"iss"`
		Audience json.RawMessage `json:"aud"`
		Expiry   int64           `json:"exp"`
		Nonce    string          `json:"nonce"`
		Subject  string          `json:"sub"`
		Email    string          `json:"email"`
	}
	if err != nil || json.Unmarshal(body, &claims) != nil {
		return "", "", fmt.Errorf("invalid ChatGPT ID token claims")
	}
	var audience string
	var audiences []string
	if json.Unmarshal(claims.Audience, &audience) == nil {
		audiences = []string{audience}
	} else {
		json.Unmarshal(claims.Audience, &audiences)
	}
	matches := false
	for _, aud := range audiences {
		matches = matches || aud == clientID
	}
	if claims.Issuer != issuer || !matches || claims.Expiry <= time.Now().Unix() || subtle.ConstantTimeCompare([]byte(claims.Nonce), []byte(nonce)) != 1 || claims.Subject == "" {
		return "", "", fmt.Errorf("ChatGPT ID token claims do not match this login")
	}
	if subject != "" && claims.Subject != subject {
		path, err := chatGPTCredentialPath()
		if err != nil {
			return "", "", fmt.Errorf("ChatGPT login must use the same account; use the saved ChatGPT account or OPENAI_API_KEY; locate saved registration: %w", err)
		}
		return "", "", fmt.Errorf("ChatGPT login must use the same account; use the saved ChatGPT account or OPENAI_API_KEY (saved registration: %s)", path)
	}
	return claims.Subject, claims.Email, nil
}

// A file read polls cancellation without closing stdin or reading ahead into a later chat.
type chatGPTPasteReader struct {
	ctx    context.Context
	reader pollingReader
}

func (r chatGPTPasteReader) Read(p []byte) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	p = p[:1]
	for {
		if r.ctx.Err() != nil {
			return 0, io.EOF
		}
		n, err := r.reader.Read(p)
		if n > 0 || err != nil {
			return n, err
		}
	}
}

// Both the callback and pasted URLs enter the same single-winner, state-checked path.
func waitChatGPTCallback(ctx context.Context, listener net.Listener, redirect, state, clientID string, input io.Reader, out io.Writer, open func(context.Context, string) error, authorize string) (url.Values, error) {
	var mu sync.Mutex
	var result url.Values
	var failure error
	done := false
	submit := func(address string) error {
		mu.Lock()
		defer mu.Unlock()
		if done {
			return nil
		}
		u, err := url.Parse(address)
		if err != nil || u.Scheme+"://"+u.Host+u.Path != redirect || u.User != nil || u.Fragment != "" {
			return fmt.Errorf("paste the complete loopback callback URL from this login")
		}
		values, err := url.ParseQuery(u.RawQuery)
		if err != nil || len(values["state"]) != 1 || subtle.ConstantTimeCompare([]byte(values.Get("state")), []byte(state)) != 1 {
			return fmt.Errorf("ChatGPT login state did not match this attempt")
		}
		if len(values["error"]) > 1 {
			return fmt.Errorf("incomplete ChatGPT login callback")
		}
		if values.Get("error") != "" {
			done = true
			failure = fmt.Errorf("ChatGPT sign-in was not approved; run reagent login again")
			return nil
		}
		if len(values["code"]) != 1 || values.Get("code") == "" || len(values["client_id"]) > 1 {
			return fmt.Errorf("incomplete ChatGPT login callback")
		}
		issued := values.Get("client_id")
		if clientID == "" && (!strings.HasPrefix(issued, "oaiapp_") || len(issued) == len("oaiapp_")) {
			return fmt.Errorf("ChatGPT callback did not issue a valid client ID")
		}
		if clientID != "" && issued != "" && issued != clientID {
			return fmt.Errorf("ChatGPT callback returned a different client ID")
		}
		result = values
		done = true
		return nil
	}
	server := &http.Server{ReadHeaderTimeout: 5 * time.Second, Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet {
			http.Error(w, "invalid ChatGPT login callback", http.StatusBadRequest)
			return
		}
		if err := submit("http://" + r.Host + r.URL.RequestURI()); err != nil {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		fmt.Fprintln(w, "Return to re:agent — you can close this tab.")
	})}
	defer server.Close()
	go server.Serve(listener)
	if input != nil {
		inputCtx, cancelInput := context.WithCancel(ctx)
		var inputDone sync.WaitGroup
		var closeInput func()
		if file, ok := input.(*os.File); ok {
			input = chatGPTPasteReader{ctx: inputCtx, reader: pollingReader{fd: int(file.Fd())}}
		} else if closer, ok := input.(io.Closer); ok {
			// Injected blocking readers must be closable; cleanup owns their close.
			closeInput = func() { closer.Close() }
		}
		defer func() {
			cancelInput()
			if closeInput != nil {
				closeInput()
			}
			inputDone.Wait()
		}()
		inputDone.Add(1)
		go func() {
			defer inputDone.Done()
			scanner := bufio.NewScanner(input)
			scanner.Buffer(make([]byte, 4096), 1<<20)
			for scanner.Scan() {
				line := strings.TrimSpace(scanner.Text())
				if line != "" {
					if err := submit(line); err != nil {
						mu.Lock()
						fmt.Fprintf(out, "Invalid callback: %s. Still waiting for approval; paste the final callback URL or press Ctrl-C to cancel.\n", err)
						mu.Unlock()
						continue
					}
					return
				}
			}
		}()
	}
	shown, _ := url.Parse(authorize)
	query := shown.Query()
	hinted := query.Has("id_token_hint")
	// The printed fallback still works, but never discloses a saved identity token.
	query.Del("id_token_hint")
	shown.RawQuery = query.Encode()
	port := strings.TrimPrefix(listener.Addr().String(), "127.0.0.1:")
	mu.Lock()
	fmt.Fprintf(out, "Opening your browser for ChatGPT sign-in.\n%s\nWaiting for approval (Ctrl-C cancels). If the callback cannot reach this machine, paste the final %s?... URL here, or use ssh -L %s:127.0.0.1:%s.\n", shown.String(), redirect, port, port)
	if hinted {
		fmt.Fprintln(out, "The printed URL omits the saved identity token and may show the account picker.")
	}
	mu.Unlock()
	if err := open(ctx, authorize); err != nil {
		mu.Lock()
		fmt.Fprintln(out, "Could not open the browser; open the URL above yourself.")
		mu.Unlock()
	}
	for {
		mu.Lock()
		finished, values, err := done, result, failure
		mu.Unlock()
		if finished {
			return values, err
		}
		if err := waitBeforeRetry(ctx, 25*time.Millisecond); err != nil {
			return nil, fmt.Errorf("ChatGPT sign-in cancelled or timed out")
		}
	}
}

func loginChatGPT(ctx context.Context, client *http.Client, input io.Reader, out io.Writer, open func(context.Context, string) error) (chatGPTCredentials, error) {
	ctx, cancel := context.WithTimeout(ctx, 5*time.Minute)
	defer cancel()
	lock, err := lockChatGPTCredentials(ctx)
	if err != nil {
		return chatGPTCredentials{}, err
	}
	c, err := func() (chatGPTCredentials, error) {
		defer unlockChatGPTCredentials(lock)
		c, err := loadChatGPTCredentials()
		if err != nil {
			return c, err
		}
		if c.HostID == "" {
			c.HostID, err = chatGPTHostID()
			if err != nil {
				return c, err
			}
			return c, saveChatGPTCredentials(c)
		}
		return c, nil
	}()
	if err != nil {
		return c, err
	}
	discovery, err := discoverChatGPT(ctx, client)
	if err != nil {
		return c, err
	}
	listener, err := net.Listen("tcp", "127.0.0.1:1455")
	if err != nil {
		listener, err = net.Listen("tcp", "127.0.0.1:0")
	}
	if err != nil {
		return c, fmt.Errorf("could not listen for ChatGPT callback: %w", err)
	}
	defer listener.Close()
	redirect := "http://" + listener.Addr().String() + "/auth/callback"
	state, err := chatGPTRandom()
	if err != nil {
		return c, err
	}
	nonce, err := chatGPTRandom()
	if err != nil {
		return c, err
	}
	verifier, err := chatGPTRandom()
	if err != nil {
		return c, err
	}
	challenge := sha256.Sum256([]byte(verifier))
	clientID := c.ClientID
	if clientID == "" {
		clientID = "dynamic_agent_client"
	}
	authorize, _ := url.Parse(discovery.Authorize)
	query := url.Values{
		"client_id": {clientID}, "response_type": {"code"}, "redirect_uri": {redirect},
		"scope":    {"openid profile email offline_access resource.invoke chatgpt.tokens.use.direct"},
		"resource": {chatGPTResource}, "state": {state}, "nonce": {nonce},
		"code_challenge": {base64.RawURLEncoding.EncodeToString(challenge[:])}, "code_challenge_method": {"S256"}, "ext_agent_host_id": {c.HostID},
	}
	if c.ClientID == "" {
		query.Set("agent_name_hint", "re:agent")
	} else {
		if c.IDToken != "" {
			query.Set("id_token_hint", c.IDToken)
		}
		if !c.planGranted() {
			query.Set("prompt", "consent")
		}
		if c.Email != "" {
			query.Set("login_hint", c.Email)
		}
	}
	authorize.RawQuery = query.Encode()
	callback, err := waitChatGPTCallback(ctx, listener, redirect, state, c.ClientID, input, out, open, authorize.String())
	if err != nil {
		return c, err
	}
	lock, err = lockChatGPTCredentials(ctx)
	if err != nil {
		return c, err
	}
	defer unlockChatGPTCredentials(lock)
	current, err := loadChatGPTCredentials()
	if err != nil {
		return c, err
	}
	// Refreshes may rotate tokens while the browser is open; identity changes
	// and a concurrent sign-out must not be undone by this pending login.
	if current.HostID != c.HostID || current.ClientID != c.ClientID || current.Subject != c.Subject || current.LogoutGeneration != c.LogoutGeneration {
		return current, fmt.Errorf("ChatGPT credentials changed during sign-in; run reagent login again")
	}
	c = current
	issued := callback.Get("client_id")
	if c.ClientID == "" {
		c.ClientID = issued
		// Registration survives even if the subsequent exchange fails.
		if err := saveChatGPTCredentials(c); err != nil {
			return c, err
		}
	}
	var tokens chatGPTTokenSet
	if err := chatGPTForm(ctx, client, discovery.Token, url.Values{
		"grant_type": {"authorization_code"}, "client_id": {c.ClientID}, "code": {callback.Get("code")},
		"code_verifier": {verifier}, "redirect_uri": {redirect}, "resource": {chatGPTResource},
	}, &tokens); err != nil {
		return c, err
	}
	subject, email, err := validateChatGPTIDToken(tokens.IDToken, discovery.Issuer, c.ClientID, nonce, c.Subject)
	if err != nil {
		return c, err
	}
	// A new authorization must not inherit plan permission from the previous grant.
	c.Scope = ""
	if err := applyChatGPTTokens(&c, tokens); err != nil {
		return c, err
	}
	c.Subject, c.Email = subject, email
	if err := saveChatGPTCredentials(c); err != nil {
		return c, err
	}
	return c, nil
}

func logoutChatGPT(ctx context.Context, client *http.Client, out io.Writer) error {
	lock, err := lockChatGPTCredentials(ctx)
	if err != nil {
		return err
	}
	defer unlockChatGPTCredentials(lock)
	c, err := loadChatGPTCredentials()
	if err != nil {
		return err
	}
	if c.RefreshToken != "" {
		if err := chatGPTForm(ctx, client, chatGPTIssuer()+"/api/accounts/oauth/revoke", url.Values{
			"token": {c.RefreshToken}, "token_type_hint": {"refresh_token"}, "client_id": {c.ClientID},
		}, nil); err != nil {
			fmt.Fprintln(out, "Could not confirm revocation. Disconnect re:agent in https://chatgpt.com/settings before signing in again.")
		}
	}
	c.IDToken, c.AccessToken, c.RefreshToken, c.Scope = "", "", "", ""
	c.ExpiresAt, c.EarliestRefreshAt, c.NeedsLogin = 0, 0, false
	c.LogoutGeneration++
	if err := saveChatGPTCredentials(c); err != nil {
		return err
	}
	fmt.Fprintln(out, "Signed out of ChatGPT.")
	return nil
}

func chatGPTVisible(text string, secrets ...string) string {
	for _, secret := range secrets {
		if secret != "" {
			text = strings.ReplaceAll(text, secret, "[redacted]")
		}
	}
	return sanitize(strings.NewReplacer("\n", `\n`, "\t", `\t`).Replace(text))
}

func printChatGPTModels(ctx context.Context, client *http.Client, token string, out io.Writer, secrets ...string) error {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, openAIInferenceBaseURL()+"/models", nil)
	if err != nil {
		return fmt.Errorf("invalid OpenAI inference base URL")
	}
	req.Header.Set("Authorization", "Bearer "+token)
	response, err := client.Do(req)
	if err != nil {
		return fmt.Errorf("could not list ChatGPT models")
	}
	defer response.Body.Close()
	var body struct {
		Models []struct {
			Slug        string `json:"slug"`
			DisplayName string `json:"display_name"`
			Visibility  string `json:"visibility"`
		} `json:"models"`
	}
	if response.StatusCode != http.StatusOK || json.NewDecoder(io.LimitReader(response.Body, 1<<20)).Decode(&body) != nil {
		return fmt.Errorf("could not list ChatGPT models (HTTP %d)", response.StatusCode)
	}
	fmt.Fprintln(out, "Available models:")
	for _, model := range body.Models {
		if model.Visibility == "list" {
			tokens := append([]string{token}, secrets...)
			fmt.Fprintf(out, "  %s (%s)\n", chatGPTVisible(model.Slug, tokens...), chatGPTVisible(model.DisplayName, tokens...))
		}
	}
	return nil
}

func chatGPTAuthCommand(ctx context.Context, args []string, stdin io.Reader, stdout, stderr io.Writer) int {
	ctx, stop := signal.NotifyContext(ctx, os.Interrupt)
	defer stop()
	client := NewHTTPClient()
	ctx, cancel := context.WithTimeout(ctx, 5*time.Minute)
	defer cancel()
	if len(args) == 1 && args[0] == "logout" {
		if err := logoutChatGPT(ctx, client, stdout); err != nil {
			return startupError(stderr, err.Error())
		}
		return exitOK
	}
	if len(args) == 1 && args[0] == "login" {
		previous, err := loadChatGPTCredentials()
		if err != nil {
			return startupError(stderr, err.Error())
		}
		secrets := []string{previous.AccessToken, previous.RefreshToken, previous.IDToken}
		c, err := loginChatGPT(ctx, client, stdin, stderr, openChatGPTBrowser)
		if err != nil {
			return startupError(stderr, err.Error())
		}
		if !c.planGranted() {
			fmt.Fprintln(stdout, "Signed in, but ChatGPT plan usage was not granted. Run reagent login again and allow it, or set OPENAI_API_KEY.")
			return exitOK
		}
		secrets = append(secrets, c.AccessToken, c.RefreshToken, c.IDToken)
		fmt.Fprintf(stdout, "Signed in as %s. ChatGPT plan usage: enabled.\n", chatGPTVisible(c.Email, secrets...))
		if err := printChatGPTModels(ctx, client, c.AccessToken, stdout, secrets...); err != nil {
			fmt.Fprintln(stderr, err)
		}
		return exitOK
	}
	if len(args) == 2 && args[0] == "auth" && args[1] == "status" {
		c, err := loadChatGPTCredentials()
		if err != nil {
			return startupError(stderr, err.Error())
		}
		if c.AccessToken == "" || c.NeedsLogin {
			account := "none"
			if c.Email != "" {
				account = chatGPTVisible(c.Email, c.AccessToken, c.RefreshToken, c.IDToken)
			}
			fmt.Fprintf(stdout, "Account: %s\nChatGPT plan usage: disabled\nAccess token expires: none\nNot signed in to ChatGPT. Run reagent login.\n", account)
			return exitOK
		}
		secrets := []string{c.AccessToken, c.RefreshToken, c.IDToken}
		grant := "not granted"
		if c.planGranted() {
			grant = "enabled"
			source, err := newChatGPTTokenSource(client)
			if err != nil {
				return startupError(stderr, err.Error())
			}
			if _, err := source.Token(ctx, false, ""); err != nil {
				return startupError(stderr, err.Error())
			}
			secrets = append(secrets, source.secrets()...)
			c, err = loadChatGPTCredentials()
			if err != nil {
				return startupError(stderr, err.Error())
			}
		}
		secrets = append(secrets, c.AccessToken, c.RefreshToken, c.IDToken)
		fmt.Fprintf(stdout, "Account: %s\nChatGPT plan usage: %s\nAccess token expires: %s\n", chatGPTVisible(c.Email, secrets...), grant, time.Unix(c.ExpiresAt, 0).UTC().Format(time.RFC3339))
		if err := printChatGPTModels(ctx, client, c.AccessToken, stdout, secrets...); err != nil {
			return startupError(stderr, err.Error())
		}
		return exitOK
	}
	fmt.Fprintln(stderr, "Usage: reagent login | reagent logout | reagent auth status")
	return exitUsage
}
