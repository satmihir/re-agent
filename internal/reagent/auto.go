package reagent

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"sort"
	"strings"
)

// v0 §10 amendment (2026-10-02): sufficiency is experimental, not calibrated.
const (
	autoDwell       = 3
	autoSwitchLimit = 3
	autoSufficiency = 0.80
)

type autoRouting struct {
	enabled  bool
	jev      *jevClient
	fallback jevRoute
	routes   []jevRoute
	models   map[string]Model
	proxy    apiProxy
	usage    Usage
	attempts int
}

type autoCandidate struct {
	route   jevRoute
	cfg     Config
	handoff *modelHandoff
	bytes   int
}

func newAutoRouting(cfg Config, key string, keys map[string]string, proxy apiProxy, client *http.Client, trace *Trace) (*autoRouting, error) {
	info, known := findModel(cfg.Model)
	if !known || info.ContextWindow == 0 || info.Provider != cfg.Provider || (len(info.Efforts) == 0 && cfg.ReasoningEffort != "") || (len(info.Efforts) != 0 && !info.accepts(cfg.ReasoningEffort)) {
		return nil, fmt.Errorf("Auto fallback needs a catalog model with a known window and supported explicit effort")
	}
	if keys[info.Provider] == "" && !proxy.serves(info.Provider) {
		return nil, fmt.Errorf("Auto fallback provider is unavailable")
	}
	fallback := jevRoute{ID: "capable", Model: cfg.Model, Effort: cfg.ReasoningEffort, Description: autoRouteDescription(cfg.Model, cfg.ReasoningEffort)}
	a := &autoRouting{enabled: true, jev: newJevClient(key, "", client), fallback: fallback, proxy: proxy, usage: Usage{Known: true}, models: make(map[string]Model)}
	// v0 §10 amendment (2026-10-02): one Choice over at most eight joint pairs.
	a.routes = append(a.routes, fallback)
	for _, modelID := range []string{"gpt-6-luna", "gpt-6-sol", cfg.Model} {
		model, known := findModel(modelID)
		if !known || model.ContextWindow == 0 || (keys[model.Provider] == "" && !proxy.serves(model.Provider)) {
			continue
		}
		for _, effort := range []string{"low", "medium", "high"} {
			if !model.accepts(effort) || (modelID != "gpt-6-luna" && modelID != "gpt-6-sol" && effort != "high") {
				continue
			}
			duplicate := false
			for _, route := range a.routes {
				if route.Model == modelID && route.Effort == effort {
					duplicate = true
					break
				}
			}
			if duplicate {
				continue
			}
			a.routes = append(a.routes, jevRoute{ID: modelID + "/" + effort, Model: modelID, Effort: effort, Description: autoRouteDescription(modelID, effort)})
		}
	}
	for _, route := range a.routes {
		model, _ := findModel(route.Model)
		a.models[route.ID] = newLiveModel(model.Provider, keys[model.Provider], proxy, client, trace)
	}
	return a, nil
}

// v0 §10 amendment (2026-10-02): the configured pair uses the same experimental rubric.
func autoRouteDescription(model, effort string) string {
	description := "Use the configured model's explicit effort setting."
	switch effort {
	case "low":
		description = "Clear lookups, mechanical changes and interpreting straightforward tool results."
	case "medium":
		description = "A concrete multi-step task needing integration of several findings or a specific debugging path."
	case "high":
		description = "Evidence cheaper effort fell short: repeated failure, competing hypotheses after investigation, or an explicit request for deep analysis."
	}
	modelDescription := "Configured model."
	switch model {
	case "gpt-6-luna":
		modelDescription = "Designated faster model; do not assume high effort equals the stronger model."
	case "gpt-6-sol":
		modelDescription = "Stronger model for work that needs more capability than the faster model provides."
	}
	return fmt.Sprintf("%s / %s. %s %s", model, effort, modelDescription, description)
}

func autoDisclosure(w io.Writer, a *autoRouting) {
	fmt.Fprintf(w, "auto on: %s; TypeSafe receives summaries, user requests (earlier ones may be shortened or omitted), root project instructions and latest plan, plus selected assistant/tool/shell evidence including file contents and command output (up to 512-byte previews); content may contain secrets and is not secret-filtered; native reasoning omitted; routing credentials stay in the HTTP header\n", jevEndpoint)
	fmt.Fprintf(w, "auto fallback: %s / %s; %d allowed route(s); /auto off or manual model/effort selection pins the current route\n", a.fallback.Model, a.fallback.Effort, len(a.routes))
	fmt.Fprintln(w, "auto scope: joint model/effort at user-turn start; current-model effort only within a run; router failure keeps the current pair mid-run")
	if strings.TrimSpace(a.jev.key) == "" {
		fmt.Fprintln(w, "TYPESAFE_API_KEY is not set; Auto uses compatible fallback without routing calls")
	}
}

func stageAutoCandidate(ctx context.Context, s *Session, route jevRoute) (autoCandidate, error) {
	if err := ctx.Err(); err != nil {
		return autoCandidate{}, err
	}
	info, known := findModel(route.Model)
	if !known {
		return autoCandidate{}, fmt.Errorf("unknown destination model")
	}
	cfg := s.cfg
	cfg.Provider, cfg.Model, cfg.ReasoningEffort = info.Provider, route.Model, route.Effort
	cfg.Proxied = s.auto.proxy.serves(info.Provider)
	handoff := s.handoff
	var size int
	var err error
	textOnly := true
	for _, entry := range s.history {
		if entry.Kind == EntryAssistant || entry.Kind == EntryTool {
			textOnly = false
			break
		}
	}
	if cfg.Model == s.cfg.Model {
		size, err = validateDestination(cfg, s.requestHistory(), info.ContextWindow, s.admissionRate(cfg.Provider))
	} else if textOnly {
		// No native assistant state exists yet; preserve ordinary user snapshot/plan parts.
		handoff = nil
		size, err = validateDestination(cfg, s.history, info.ContextWindow, s.admissionRate(cfg.Provider))
	} else {
		handoff, size, err = stageModelSwitch(ctx, s.history, cfg, info.ContextWindow, s.admissionRate(cfg.Provider))
	}
	if err != nil {
		return autoCandidate{}, err
	}
	if err := ctx.Err(); err != nil {
		return autoCandidate{}, err
	}
	return autoCandidate{route: route, cfg: cfg, handoff: handoff, bytes: size}, nil
}

// v0 §10 amendment (2026-10-02): this is a cost order, not a capability ranking.
func autoCheapestSufficient(routes []jevRoute, probabilities map[string]float64) (string, []string, []float64) {
	ladder := append([]jevRoute(nil), routes...)
	rank := func(route jevRoute) int {
		switch route.Model {
		case "gpt-6-luna":
			return 0
		case "gpt-6-sol":
			return 1
		default:
			return 2
		}
	}
	effortIndex := func(route jevRoute) int {
		info, _ := findModel(route.Model)
		for i, effort := range info.Efforts {
			if effort == route.Effort {
				return i
			}
		}
		return 0 // Models without an effort setting contribute only the configured pair.
	}
	sort.SliceStable(ladder, func(i, j int) bool {
		if rank(ladder[i]) != rank(ladder[j]) {
			return rank(ladder[i]) < rank(ladder[j])
		}
		return effortIndex(ladder[i]) < effortIndex(ladder[j])
	})
	ids, cumulative := make([]string, 0, len(ladder)), make([]float64, 0, len(ladder))
	selected, sum := "", 0.0
	for _, route := range ladder {
		sum += probabilities[route.ID]
		ids, cumulative = append(ids, route.ID), append(cumulative, sum)
		if selected == "" && sum >= autoSufficiency {
			selected = route.ID
		}
	}
	return selected, ids, cumulative
}

func autoHigherEffort(cfg Config, route jevRoute) bool {
	if cfg.Model != route.Model {
		return false
	}
	info, _ := findModel(cfg.Model)
	current, selected := -1, -1
	for i, effort := range info.Efforts {
		if effort == cfg.ReasoningEffort {
			current = i
		}
		if effort == route.Effort {
			selected = i
		}
	}
	return current >= 0 && selected > current
}

func routingToolFailure(history []Entry) bool {
	for _, entry := range history {
		if entry.Tool == nil || entry.Tool.Outcome.OK {
			continue
		}
		switch entry.Tool.Outcome.Code {
		case "permission_denied", "permission_required", "plan_mode", "not_executed":
		default:
			return true
		}
	}
	return false
}

func (r *Run) routeNext(ctx context.Context) error {
	s, a := r.session, r.session.auto
	if a == nil || !a.enabled || r.routingOff {
		return nil
	}
	failure := routingToolFailure(s.history[r.routingEntries:])
	if r.steps != 0 && r.steps-r.routingStep < autoDwell && !failure {
		return nil
	}
	reason := "periodic"
	if r.steps == 0 {
		reason = "user_turn"
	} else if failure {
		reason = "tool_failure"
	}
	r.routingStep, r.routingEntries = r.steps, len(s.history)
	metadata := map[string]any{"boundary_reason": reason, "source_model": s.cfg.Model, "source_effort": s.cfg.ReasoningEffort, "rule": "cheapest_sufficient", "threshold": autoSufficiency}
	if r.routingSwitches >= autoSwitchLimit {
		r.routingOff = true
		metadata["action"], metadata["reason"] = "defer", "switch_limit"
		r.trace.Write("auto.route", r.steps+1, metadata)
		return nil
	}
	if _, _, err := visibleHistory(s.history); err != nil {
		// v0 §10: a timeout awaiting its first model response defers only
		// this boundary, not every later route in the same run.
		if !errors.Is(err, errUnacknowledgedExecTimeout) {
			r.routingOff = true
		}
		metadata["action"], metadata["reason"] = "defer", err.Error()
		r.trace.Write("auto.route", r.steps+1, metadata)
		return nil
	}
	var candidates []autoCandidate
	var routes []jevRoute
	var excluded []string
	for _, route := range a.routes {
		// v0 §10 amendment (2026-10-02): avoid repeated cross-model prefills mid-run.
		if r.steps != 0 && route.Model != s.cfg.Model {
			excluded = append(excluded, route.ID+": model_change_requires_user_turn")
			continue
		}
		candidate, err := stageAutoCandidate(ctx, s, route)
		if ctx.Err() != nil {
			return ctx.Err()
		}
		if err != nil {
			excluded = append(excluded, route.ID+": "+err.Error())
			continue
		}
		candidates, routes = append(candidates, candidate), append(routes, route)
	}
	metadata["candidates"], metadata["excluded"] = routes, excluded
	var selected *autoCandidate
	for i := range candidates {
		// v0 §10 amendment (2026-10-02): failure keeps the current pair mid-run.
		if r.steps == 0 && candidates[i].route.ID == a.fallback.ID || r.steps != 0 && candidates[i].cfg.Model == s.cfg.Model && candidates[i].cfg.ReasoningEffort == s.cfg.ReasoningEffort {
			selected = &candidates[i]
			break
		}
	}
	fallbackReason := ""
	if len(routes) < 2 {
		fallbackReason = "fewer_than_two_admitted_routes"
	} else if strings.TrimSpace(a.jev.key) == "" {
		fallbackReason = "missing_router_credentials"
		r.routingOff = true
	} else {
		packet, err := buildRoutingPacket(s.cfg, s.history, s.planMode, routes)
		if err != nil {
			fallbackReason = err.Error()
			r.routingOff = true
		} else {
			metadata["omitted_earlier_entries"], metadata["omitted_native_items"], metadata["previews"] = packet.OmittedEarlier, packet.OmittedNative, packet.Truncations
			metadata["omitted_earlier_user_requests"] = packet.OmittedRequests
			s.display.startStatus("choosing a route with Jev")
			decision, err := a.jev.decide(ctx, packet.State, routes)
			s.display.stopStatus()
			if decision.Attempted {
				if r.routerUsage == nil {
					r.routerUsage = &Usage{Known: true}
				}
				r.routerUsage.Add(decision.Usage)
				a.usage.Add(decision.Usage)
				a.attempts++
			}
			metadata["jev_model"], metadata["router_usage"], metadata["router_http_status"] = decision.Model, decision.Usage, decision.HTTPStatus
			metadata["router_latency_ms"], metadata["packet_bytes"], metadata["packet_sha256"] = decision.DurationMS, decision.RequestBytes, decision.RequestSHA256
			metadata["confidence"], metadata["probabilities"] = decision.Confidence, decision.Probabilities
			if ctx.Err() != nil {
				metadata["action"] = "cancelled"
				r.trace.Write("auto.route", r.steps+1, metadata)
				return ctx.Err()
			}
			if err != nil {
				fallbackReason = err.Error()
				r.routingOff = true
			} else {
				// v0 §10 amendment (2026-10-02): confidence describes concentration, not sufficiency.
				id, ladder, cumulative := autoCheapestSufficient(routes, decision.Probabilities)
				metadata["ladder"], metadata["cumulative"] = ladder, cumulative
				for i := range candidates {
					if candidates[i].route.ID == id {
						selected = &candidates[i]
						break
					}
				}
			}
		}
	}
	metadata["action"] = "stay"
	if fallbackReason != "" {
		metadata["reason"] = fallbackReason
		metadata["action"] = "fallback"
	}
	if selected == nil {
		metadata["action"], metadata["reason"] = "defer", "no_compatible_fallback"
		r.routingOff = true
	} else {
		metadata["selected_route"] = selected.route
		changed := selected.cfg.Model != s.cfg.Model || selected.cfg.ReasoningEffort != s.cfg.ReasoningEffort
		if changed && fallbackReason == "" && r.steps != 0 && r.steps-r.routingSwitchStep < autoDwell && !(failure && autoHigherEffort(s.cfg, selected.route)) {
			metadata["action"], metadata["reason"] = "defer", "minimum_dwell"
		} else if changed {
			if err := ctx.Err(); err != nil {
				return err
			}
			if selected.cfg.Model != s.cfg.Model {
				s.tokensPerByte = 0
			}
			r.routingPostSwitch = true
			s.cfg, s.handoff, s.model = selected.cfg, selected.handoff, a.models[selected.route.ID]
			s.lastRequest = Usage{}
			r.routingSwitches++
			r.routingSwitchStep = r.steps
			metadata["action"], metadata["generation_request_bytes"] = "switch", selected.bytes
			if selected.handoff != nil {
				metadata["handoff_entries"], metadata["handoff_native_items"] = selected.handoff.Entries, selected.handoff.NativeItems
			}
			s.display.note(fmt.Sprintf("auto: %s / %s · %s", selected.cfg.Model, selected.cfg.ReasoningEffort, reason))
		}
	}
	r.trace.Write("auto.route", r.steps+1, metadata)
	if fallbackReason != "" {
		s.display.note(fmt.Sprintf("auto: %s / %s · %s", s.cfg.Model, s.cfg.ReasoningEffort, sanitize(fallbackReason)))
	}
	return nil
}

func (s *Session) pinAuto() {
	if s.auto != nil {
		s.auto.enabled = false
	}
}

func (c *conversation) commandAuto(argument string, stderr io.Writer) {
	if c.scripted != nil {
		fmt.Fprintln(stderr, "Auto is unavailable in a scripted chat")
		return
	}
	switch argument {
	case "":
		if c.session.auto != nil && c.session.auto.enabled {
			fmt.Fprintln(stderr, "auto on; /auto off pins the current route")
		} else {
			fmt.Fprintln(stderr, "auto off; /auto on enables TypeSafe routing")
		}
	case "off":
		c.session.pinAuto()
		fmt.Fprintln(stderr, "auto off; current model and effort pinned")
	case "on":
		a, err := newAutoRouting(c.session.cfg, os.Getenv("TYPESAFE_API_KEY"), c.keys, c.proxy, c.client, c.session.trace)
		if err != nil {
			fmt.Fprintf(stderr, "auto unchanged: %s\n", err)
			return
		}
		if previous := c.session.auto; previous != nil {
			a.usage, a.attempts = previous.usage, previous.attempts
		}
		c.session.auto = a
		autoDisclosure(stderr, a)
	default:
		fmt.Fprintln(stderr, "usage: /auto [on|off]")
	}
}

func routerUsageLine(usage Usage) string {
	if !usage.Known {
		return "router     Jev usage unknown; separate from generation"
	}
	return fmt.Sprintf("router     Jev %s tokens in · %s out; separate from generation", formatCount(usage.InputTokens), formatCount(usage.OutputTokens))
}

// v0 §10 amendment (2026-10-02): measure the first attempt, not a cache-hit promise.
func (r *Run) recordRouteUsage(usage Usage) {
	if !r.routingPostSwitch {
		return
	}
	r.routingPostSwitch = false
	r.trace.Write("auto.route", r.steps, map[string]any{
		"action": "post_switch_usage", "switch_step": r.routingSwitchStep + 1,
		"model": r.cfg.Model, "effort": r.cfg.ReasoningEffort,
		"usage_known": usage.Known, "input_tokens": usage.InputTokens,
		"cached_input_tokens": usage.CachedInputTokens,
	})
}
