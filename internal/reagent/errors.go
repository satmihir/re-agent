package reagent

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net"
	"os/exec"
	"regexp/syntax"
	"strings"
	"syscall"
)

// boundaryError owns library diagnostics while retaining the original cause.
type boundaryError struct {
	operation string
	cause     error
}

func (e boundaryError) Error() string { return e.operation + ": " + stableError(e.cause) }
func (e boundaryError) Unwrap() error { return e.cause }

// stableError describes a library failure without exposing its implementation's prose.
// Call it at library boundaries, not on errors already phrased by re:agent.
func stableError(err error) string {
	switch {
	case errors.Is(err, context.Canceled):
		return "operation cancelled"
	case errors.Is(err, context.DeadlineExceeded):
		return "operation timed out"
	case errors.Is(err, fs.ErrNotExist):
		return "path does not exist"
	case errors.Is(err, fs.ErrPermission):
		return "permission denied"
	case errors.Is(err, fs.ErrExist):
		return "path already exists"
	case errors.Is(err, syscall.ENOTDIR):
		return "path component is not a directory"
	case errors.Is(err, syscall.EISDIR):
		return "path is a directory"
	case errors.Is(err, syscall.ENOTEMPTY):
		return "directory is not empty"
	case errors.Is(err, syscall.ELOOP):
		return "too many symbolic links"
	case errors.Is(err, syscall.ENOSPC):
		return "no storage space available"
	case errors.Is(err, syscall.EMFILE), errors.Is(err, syscall.ENFILE):
		return "open file limit reached"
	case errors.Is(err, fs.ErrInvalid):
		return "invalid operation"
	case errors.Is(err, io.EOF), errors.Is(err, io.ErrUnexpectedEOF):
		return "unexpected end of input"
	case errors.Is(err, exec.ErrNotFound):
		return "executable not found"
	}
	var exit *exec.ExitError
	if errors.As(err, &exit) {
		return fmt.Sprintf("command exited with status %d", exit.ExitCode())
	}
	var network net.Error
	if errors.As(err, &network) && network.Timeout() {
		return "operation timed out"
	}
	return "operation failed"
}

type argumentError string

func (e argumentError) Error() string { return string(e) }

func jsonError(err error, size int) string {
	var owned argumentError
	if errors.As(err, &owned) {
		return owned.Error()
	}
	var syntaxError *json.SyntaxError
	if errors.As(err, &syntaxError) {
		return fmt.Sprintf("invalid JSON at byte %d", syntaxError.Offset)
	}
	var typeError *json.UnmarshalTypeError
	if errors.As(err, &typeError) {
		expected := jsonFieldType(typeError.Field)
		field := typeError.Field
		if field == "" {
			field = "value"
		}
		return fmt.Sprintf("field %q must be %s", field, expected)
	}
	// The decoder exposes no typed unknown-field error; retain only its quoted field.
	if field, ok := strings.CutPrefix(err.Error(), "json: unknown field "); ok {
		return "unknown field " + field
	}
	if errors.Is(err, io.EOF) || errors.Is(err, io.ErrUnexpectedEOF) {
		return fmt.Sprintf("invalid JSON at byte %d: incomplete input", size+1)
	}
	return "invalid JSON value"
}

func regexError(pattern string, err error) string {
	reason := "invalid pattern"
	position := 1
	var compile *syntax.Error
	if errors.As(err, &compile) {
		position = regexPosition(pattern, compile.Code, compile.Expr)
		switch compile.Code {
		case syntax.ErrInvalidCharClass:
			reason = "invalid character class"
		case syntax.ErrInvalidCharRange:
			reason = "invalid character range"
		case syntax.ErrInvalidEscape:
			reason = "invalid escape"
		case syntax.ErrInvalidNamedCapture:
			reason = "invalid capture name"
		case syntax.ErrInvalidPerlOp:
			reason = "unsupported RE2 construct"
		case syntax.ErrInvalidRepeatOp:
			reason = "repeated repetition operator"
		case syntax.ErrInvalidRepeatSize:
			reason = "invalid repetition count"
		case syntax.ErrMissingBracket:
			reason = "unclosed character class"
		case syntax.ErrMissingParen:
			reason = "unclosed group"
		case syntax.ErrMissingRepeatArgument:
			reason = "repetition operator has no expression"
		case syntax.ErrTrailingBackslash:
			reason = "trailing backslash"
			position = len(pattern)
		case syntax.ErrUnexpectedParen:
			reason = "unexpected closing group"
		case syntax.ErrLarge:
			reason = "pattern exceeds RE2 size limit"
		case syntax.ErrNestingDepth:
			reason = "pattern exceeds RE2 nesting limit"
		}
	}
	return fmt.Sprintf("invalid RE2 regex at byte %d: %s", position, reason)
}

// Field contracts, rather than Go type names, keep aliases and collection
// element failures in the same vocabulary as the public schemas.
func jsonFieldType(field string) string {
	parts := strings.Split(field, ".")
	switch parts[len(parts)-1] {
	case "path", "text", "query", "id", "name", "task", "brief", "model", "authority", "message", "intent", "recipe", "title", "body", "branch", "commit", "category", "summary", "details", "expected_sha256", "old_text", "new_text", "append_text", "cwd", "operation", "from", "to", "rev", "kind", "type", "provider", "content", "response_id", "call_id", "arguments":
		return "a string"
	case "regex", "stat", "staged", "subtree", "recursive", "fork", "replace_all", "known":
		return "a boolean"
	case "start_line", "max_lines", "max_results", "limit", "offset", "count", "pr", "timeout_ms", "timeout", "input_tokens", "cached_input_tokens", "output_tokens", "reasoning_tokens", "unreported_attempts":
		return "an integer"
	case "paths", "files", "argv", "ids", "related_call_ids":
		return "an array of strings"
	case "entries":
		return "an array of integers"
	case "then":
		return "an array of command arrays"
	case "edits", "blocks", "history":
		return "an array of objects"
	case "items":
		return "an array"
	case "context", "native", "usage", "assistant", "user", "tool", "call":
		return "an object"
	}
	return "a value matching its declared JSON type"
}

// The compiler omits offsets, so skip literal escapes/classes/quotes when
// locating its rejected fragment, and track unmatched delimiters ourselves.
func regexPosition(pattern string, code syntax.ErrorCode, fragment string) int {
	var groups []int
	first := -1
	class := -1
	classFirst := false
	quoted := false
	for i := 0; i < len(pattern); i++ {
		if quoted {
			if strings.HasPrefix(pattern[i:], `\E`) {
				quoted = false
				i++
			}
			continue
		}
		matchesContext := class < 0
		if code == syntax.ErrInvalidCharClass || code == syntax.ErrInvalidCharRange {
			matchesContext = class >= 0
		}
		if code == syntax.ErrInvalidEscape {
			matchesContext = true
		}
		if first < 0 && fragment != "" && matchesContext && strings.HasPrefix(pattern[i:], fragment) {
			first = i
		}
		if pattern[i] == '\\' {
			if class < 0 && strings.HasPrefix(pattern[i:], `\Q`) {
				quoted = true
			}
			i++
			classFirst = false
			continue
		}
		if class >= 0 {
			if strings.HasPrefix(pattern[i:], "[:") {
				if end := strings.Index(pattern[i+2:], ":]"); end >= 0 {
					i += end + 3
					classFirst = false
					continue
				}
			}
			if pattern[i] == ']' && !classFirst {
				class = -1
			}
			classFirst = false
			continue
		}
		switch pattern[i] {
		case '[':
			class, classFirst = i, true
			if i+1 < len(pattern) && pattern[i+1] == '^' {
				i++
			}
		case '(':
			groups = append(groups, i)
		case ')':
			if len(groups) == 0 {
				if code == syntax.ErrUnexpectedParen {
					return i + 1
				}
				continue
			}
			groups = groups[:len(groups)-1]
		}
	}
	if code == syntax.ErrMissingBracket && class >= 0 {
		return class + 1
	}
	if code == syntax.ErrMissingParen && len(groups) != 0 {
		return groups[len(groups)-1] + 1
	}
	if first >= 0 {
		return first + 1
	}
	return 1
}
