package reagent

import (
	"context"
	"fmt"
	"io"
	"strconv"
)

// v0 §10 amendment (2026-09-30): only terminal input can grant dynamic access.
func workspacePermission(ctx context.Context, input lineReader, progress io.Writer, cfg Config, plan bool, destination workspaceDestination) (bool, error) {
	if ctx.Err() != nil {
		return false, errCancelled
	}
	if _, noninteractive := input.(*scannerReader); noninteractive {
		return false, errNotInteractive
	}
	if terminal, ok := input.(*terminalReader); ok && terminal.turn != nil {
		terminal.pauseTurnInput()
		terminal.discardTurnInput()
		defer terminal.resumeTurnInput()
	}
	fmt.Fprintln(progress, "Allow this location as a workspace?")
	fmt.Fprintln(progress, "Location: "+strconv.Quote(destination.path))
	if destination.missing {
		fmt.Fprintln(progress, "Destination does not exist; only this exact location will be approved, not its parent.")
	}
	fmt.Fprintln(progress, "Authority: "+cfg.Registry.Mode().String())
	if plan {
		fmt.Fprintln(progress, "Plan mode remains on; edits and commands are refused.")
	}
	fmt.Fprintln(progress, "Duration: until this chat exits, including /reset, /model, and compaction.")
	fmt.Fprintln(progress, "Files read here may be sent to the provider and recorded in traces.")
	index, err := input.Choose(pickerConfig{title: "Workspace consent", cancelLabel: "deny", freshInput: true}, []choice{
		{label: "Allow for this session"}, {label: "Deny"},
	}, 1)
	if err != nil {
		return false, err
	}
	return index == 0 && ctx.Err() == nil, nil
}
