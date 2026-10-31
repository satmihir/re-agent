// Run once with: go run testdata/legacy_checkpoint.go testdata/legacy_checkpoint.json
// Types mirror checkpoint v1 at 0499a506; added unknown fields exercise Go's skip rules.
package main

import (
	"bytes"
	"crypto/sha256"
	"encoding/gob"
	"encoding/json"
	"fmt"
	"os"
	"time"
)

type Usage struct {
	Known                                                         bool
	InputTokens, CachedInputTokens, OutputTokens, ReasoningTokens int64
	UnreportedAttempts                                            int
}
type UserTurn struct {
	Text              string
	Workspace, Roster json.RawMessage
	Source, Plan      string
}
type ToolCall struct{ CallID, Name, Arguments string }
type OutputBlock struct {
	Kind, Text string
	Call       *ToolCall
}
type NativeOutput struct {
	Provider string
	Items    []json.RawMessage
}
type ModelResponse struct {
	ResponseID, Model string
	Blocks            []OutputBlock
	Native            NativeOutput
	Usage             Usage
}
type ToolOutcome struct {
	Workspace     string
	OK            bool
	Code, Message string
	Data          json.RawMessage
	Truncated     bool
	Effect        string
}
type ToolResult struct {
	CallID, Name string
	Outcome      ToolOutcome
}
type ShellCommand struct {
	Workspace, Kind, Command          string
	ExitCode                          *int
	Signal                            *string
	Interrupted                       bool
	Output                            string
	OutputBytesSeen                   int
	OutputTruncated, EncodingReplaced bool
	DurationMS                        int64
}
type Summary struct {
	Text            string
	ReplacedEntries int
	Model           string
}
type Entry struct {
	Kind      string
	User      *UserTurn
	Assistant *ModelResponse
	Tool      *ToolResult
	Shell     *ShellCommand
	Summary   *Summary
}
type contextGCDrop struct {
	Reason      string
	Step        int
	Placeholder string
}
type contextGCState struct {
	Dropped   map[string]contextGCDrop
	Paths     map[string]string
	LastPass  int
	Inherited map[string]ToolOutcome
}
type AgentThread struct {
	ID, ParentID, Name, State, Task   string
	Steps                             int
	Cost                              Usage
	TracePath, Authority, ContextMode string
	EffectsUnknown                    bool
}
type agentObservation struct {
	AgentThread
	Depth int
}
type agentSubtree struct {
	Threads            []agentObservation
	TotalCost          Usage
	TotalAgents        int
	NextOffset         *int
	AccountingComplete bool
}
type modelHandoff struct {
	Text, Plan           string
	Entries, NativeItems int
}
type pinnedPrompt struct {
	Text    string
	Project *string
}
type chatCheckpoint struct {
	Version                                     int
	ID, Provider, Model, Effort, Launch, Active string
	ReadOnly, Plan, NoProjectInstructions       bool
	ProjectInstructions, LaunchInstructions     *string
	MaxSteps, MaxToolCalls                      int
	ModelRetryWindow                            time.Duration
	InRunCompact, ContextGC                     bool
	GC                                          contextGCState
	GCRequests                                  int
	Agents                                      bool
	AgentRoster                                 []AgentThread
	AgentSubtree                                *agentSubtree
	ReportFriction, Auto, AutoEnabled           bool
	AutoFallbackModel, AutoFallbackEffort       string
	AutoUsage                                   Usage
	AutoAttempts                                int
	AutoCompactOff                              bool
	History                                     []Entry
	PendingSubmission                           *UserTurn
	Seen                                        map[string]bool
	Handoff                                     *modelHandoff
	CompactedPlan, Blocked, LastTrace           string
	LastRequest                                 Usage
	TokensPerByte                               float64
	Usage                                       Usage
	Phase, InFlight                             string
	Prompt                                      *pinnedPrompt
	LegacyTime                                  time.Time
	LegacyArray                                 [2]uint
}

func main() {
	project, launch, signal := "active project", "launch project", "TERM"
	exit, offset := -1, 3
	usage := Usage{Known: true, InputTokens: 9223372036854775807, CachedInputTokens: 13, OutputTokens: 21, ReasoningTokens: 3, UnreportedAttempts: 2}
	thread := AgentThread{ID: "t1", ParentID: "root", Name: "legacy", State: "running", Task: "inspect", Steps: 4, Cost: usage, Authority: "read", ContextMode: "text", EffectsUnknown: true}
	cp := chatCheckpoint{
		Version: 1, ID: "0123456789abcdef", Provider: "openai", Model: "gpt-6-sol", Effort: "high",
		Launch: "/launch", Active: "/active", ReadOnly: true, Plan: true,
		ProjectInstructions: &project, LaunchInstructions: &launch, MaxSteps: 8, MaxToolCalls: 16, ModelRetryWindow: 1500 * time.Millisecond,
		InRunCompact: true, ContextGC: true, GCRequests: 2, Agents: true,
		GC:          contextGCState{Dropped: map[string]contextGCDrop{"c1": {Reason: "age", Step: 2, Placeholder: "recall"}}, Paths: map[string]string{"c1": "src/a"}, LastPass: 7, Inherited: map[string]ToolOutcome{"c1": {OK: true, Data: json.RawMessage(" {\n \"spacing\" : 1 } "), Effect: "none"}}},
		AgentRoster: []AgentThread{thread}, AgentSubtree: &agentSubtree{Threads: []agentObservation{{AgentThread: thread, Depth: 2}}, TotalCost: usage, TotalAgents: 1, NextOffset: &offset, AccountingComplete: true},
		ReportFriction: true, Auto: true, AutoEnabled: true, AutoFallbackModel: "fallback", AutoFallbackEffort: "medium", AutoUsage: usage, AutoAttempts: 2, AutoCompactOff: true,
		History: []Entry{
			{Kind: "user", User: &UserTurn{Text: "hello", Workspace: json.RawMessage(" { \"kind\" : \"workspace_state\" } "), Roster: json.RawMessage("[ 1, 2 ]"), Plan: "plan"}},
			{Kind: "assistant", Assistant: &ModelResponse{ResponseID: "r1", Model: "gpt-6-sol", Blocks: []OutputBlock{{Kind: "text", Text: "inspect"}, {Kind: "tool_call", Call: &ToolCall{CallID: "c1", Name: "read_file", Arguments: "{\"path\":\"a\"}"}}}, Native: NativeOutput{Provider: "openai", Items: []json.RawMessage{json.RawMessage(" { \"type\" : \"message\" } "), json.RawMessage{0xff, 0, 0xfe}}}, Usage: usage}},
			{Kind: "tool", Tool: &ToolResult{CallID: "c1", Name: "read_file", Outcome: ToolOutcome{Workspace: "/active", OK: true, Code: "ok", Message: "done", Data: json.RawMessage(" { \"value\" : 1 } "), Effect: "none"}}},
			{Kind: "shell", Shell: &ShellCommand{Workspace: "/active", Kind: "shell", Command: "exit", ExitCode: &exit, Signal: &signal, Interrupted: true, Output: "out", OutputBytesSeen: 3, OutputTruncated: true, EncodingReplaced: true, DurationMS: 25}},
			{Kind: "summary", Summary: &Summary{Text: "summary", ReplacedEntries: 3, Model: "old"}},
		},
		PendingSubmission: &UserTurn{Text: "pending", Workspace: json.RawMessage(" null ")}, Seen: map[string]bool{"c1": true, "unused": false},
		Handoff: &modelHandoff{Text: "handoff", Plan: "plan", Entries: 4, NativeItems: 2}, CompactedPlan: "plan", LastTrace: "trace.jsonl", LastRequest: usage, TokensPerByte: 0.375, Usage: usage,
		Phase: "terminal", Prompt: &pinnedPrompt{Text: "recorded instructions", Project: &project}, LegacyTime: time.Date(2026, 10, 1, 2, 3, 4, 5, time.UTC), LegacyArray: [2]uint{1, 256},
	}
	var state bytes.Buffer
	if err := gob.NewEncoder(&state).Encode(cp); err != nil {
		panic(err)
	}
	body, err := json.Marshal(struct {
		Version int    `json:"version"`
		ID      string `json:"id"`
		State   []byte `json:"state"`
		SHA256  string `json:"sha256"`
	}{1, cp.ID, state.Bytes(), fmt.Sprintf("%x", sha256.Sum256(state.Bytes()))})
	if err != nil {
		panic(err)
	}
	if err := os.WriteFile(os.Args[1], append(body, '\n'), 0600); err != nil {
		panic(err)
	}
}
