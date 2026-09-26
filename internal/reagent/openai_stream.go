package reagent

import (
	"bytes"
	"encoding/json"
	"fmt"
	"strings"
)

// streamEvent is the part of one Responses stream event that assembly reads.
type streamEvent struct {
	Type        string          `json:"type"`
	Response    json.RawMessage `json:"response"`
	Item        json.RawMessage `json:"item"`
	OutputIndex *int            `json:"output_index"`
	Message     string          `json:"message"`
}

// assembleOpenAIStream rebuilds the body a non-streamed request would have
// returned from a server-sent event stream (v0 §6 amendment of 2026-09-25).
//
// The stream is only a transport: nothing is shown as it arrives, and the
// result goes through the same normalization as any other body. The final
// event's own output is used when it has one. Some proxies leave it empty and
// deliver each item only in its output_item.done event, so those items, placed
// by output_index, fill an empty output.
func assembleOpenAIStream(raw []byte) ([]byte, error) {
	items := map[int]json.RawMessage{}
	var final json.RawMessage
	for _, data := range streamData(raw) {
		var event streamEvent
		if err := json.Unmarshal([]byte(data), &event); err != nil {
			return nil, &ModelError{Status: StatusProtocolError, Message: "stream event is not valid JSON"}
		}
		switch event.Type {
		case "response.output_item.done":
			if event.OutputIndex == nil || len(event.Item) == 0 {
				return nil, &ModelError{Status: StatusProtocolError, Message: "output_item.done carries no indexed item"}
			}
			items[*event.OutputIndex] = event.Item
		case "response.failed":
			return nil, streamFailure(event.Response)
		case "error":
			return nil, &ModelError{Status: StatusProviderError, Message: "provider reported a stream error: " + event.Message}
		case "response.completed", "response.incomplete":
			// An incomplete response is normalized like a non-streamed one,
			// which is what reports it and its reason.
			final = event.Response
		}
		if final != nil {
			break
		}
	}
	if final == nil {
		return nil, &ModelError{Status: StatusIncompleteResp, Message: "stream ended before the response completed"}
	}

	var body map[string]json.RawMessage
	if err := json.Unmarshal(final, &body); err != nil {
		return nil, &ModelError{Status: StatusProtocolError, Message: "final stream event carries no response object"}
	}
	var output []json.RawMessage
	if err := json.Unmarshal(body["output"], &output); err == nil && len(output) > 0 {
		return final, nil
	}
	output = make([]json.RawMessage, len(items))
	for index, item := range items {
		if index < 0 || index >= len(items) {
			return nil, &ModelError{Status: StatusProtocolError, Message: fmt.Sprintf("stream is missing output items before index %d", index)}
		}
		output[index] = item
	}
	encoded, err := json.Marshal(output)
	if err != nil {
		return nil, err
	}
	body["output"] = encoded
	return json.Marshal(body)
}

// streamData returns the data of each event in a server-sent event stream, in
// order. Events are separated by a blank line, a data field may span several
// lines, and comments and other fields are ignored.
func streamData(raw []byte) []string {
	var events []string
	var data []string
	flush := func() {
		if len(data) > 0 {
			events = append(events, strings.Join(data, "\n"))
			data = nil
		}
	}
	for _, line := range bytes.Split(raw, []byte("\n")) {
		text := strings.TrimSuffix(string(line), "\r")
		switch {
		case text == "":
			flush()
		case strings.HasPrefix(text, "data:"):
			value := strings.TrimPrefix(strings.TrimPrefix(text, "data:"), " ")
			if value != "[DONE]" {
				data = append(data, value)
			}
		}
	}
	flush()
	return events
}

// streamFailure reports a failed response with the provider's own reason and
// whatever usage it had already cost.
func streamFailure(response json.RawMessage) error {
	var body responsesBody
	_ = json.Unmarshal(response, &body)
	message := "provider reported a failed response"
	if body.Error != nil && body.Error.Message != "" {
		message += ": " + body.Error.Message
	}
	return &ModelError{Status: StatusProviderError, Usage: body.Usage.normalized(), Message: message}
}
