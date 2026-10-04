package reagent

import (
	"context"
	"sync"
)

// v0 §10.3: turnInput keeps type-ahead editable and queues one submission.
type turnInput struct {
	editor         *editor
	prefix, queued string
	ctx            context.Context
	cancel         context.CancelFunc
	stop           context.CancelFunc
	wg             sync.WaitGroup
	exit           bool
}

func (r *terminalReader) startTurnInput(ctx context.Context, cancel context.CancelFunc) {
	if r.region == nil {
		return
	}
	r.region.mu.Lock()
	e := r.nextEditor
	if e == nil {
		e = &editor{}
	}
	r.nextEditor = nil
	e.width, e.prompt = r.width, regionPrompt(r.regionStatus, r.styled)
	r.turn = &turnInput{editor: e, ctx: ctx, cancel: cancel}
	r.redrawRegionLocked()
	r.region.mu.Unlock()
	r.resumeTurnInput()
}

func (r *terminalReader) resumeTurnInput() {
	if r.turn == nil || r.turn.ctx.Err() != nil || r.turn.stop != nil {
		return
	}
	r.region.mu.Lock()
	r.redrawRegionLocked()
	r.region.mu.Unlock()
	ctx, stop := context.WithCancel(r.turn.ctx)
	r.turn.stop = stop
	r.turn.wg.Add(1)
	go func() { defer r.turn.wg.Done(); r.pollTurnInput(ctx) }()
}

func (r *terminalReader) pauseTurnInput() bool {
	if r.turn == nil || r.turn.stop == nil {
		return false
	}
	r.turn.stop()
	r.turn.wg.Wait()
	r.turn.stop = nil
	return true
}

func (r *terminalReader) finishTurnInput() {
	if r.region == nil || r.turn == nil {
		return
	}
	r.pauseTurnInput()
	r.region.mu.Lock()
	r.nextEditor, r.nextSubmission, r.nextEOF = r.turn.editor, r.turn.queued, r.turn.exit
	r.turn = nil
	r.region.mu.Unlock()
}

func (r *terminalReader) discardTurnInput() {
	if r.turn == nil {
		return
	}
	r.region.mu.Lock()
	r.turn.editor = &editor{width: r.width, prompt: regionPrompt(r.regionStatus, r.styled)}
	r.turn.prefix, r.turn.queued, r.turn.exit = "", "", false
	r.regionStatus.queued = false
	r.queued, r.partial = nil, nil
	r.keys.pending, r.keys.hold = nil, nil
	r.keys.paste, r.keys.lastCR, r.pasting = false, false, false
	r.redrawRegionLocked()
	r.region.mu.Unlock()
}

func (r *terminalReader) pollTurnInput(ctx context.Context) {
	for ctx.Err() == nil {
		if len(r.queued) == 0 {
			var chunk [256]byte
			n, err := r.keys.Read(chunk[:])
			if err != nil {
				return
			}
			keys, rest := decodeInputKeys(append(r.partial, chunk[:n]...))
			r.partial = append([]byte(nil), rest...)
			r.queued = keys
		}
		if len(r.queued) == 0 {
			r.region.mu.Lock()
			if width, height, err := r.size(); err == nil && (width != r.region.width || height != r.region.height) {
				r.redrawRegionLocked()
			}
			r.region.mu.Unlock()
			continue
		}
		k := r.queued[0]
		r.queued = r.queued[1:]
		r.region.mu.Lock()
		if ctx.Err() == nil {
			r.applyTurnKey(k)
			r.redrawRegionLocked()
		}
		r.region.mu.Unlock()
	}
}

func (r *terminalReader) applyTurnKey(k inputKey) {
	t := r.turn
	if k.name == "interrupt" {
		t.cancel()
		return
	}
	if t.queued != "" || t.exit {
		return
	}
	if k.name == "paste-start" {
		r.pasting = true
		return
	}
	if k.name == "paste-end" {
		r.pasting = false
		return
	}
	if r.pasting && k.name == "enter" {
		k = inputKey{text: '\n'}
	}
	if r.pasting && k.name == "tab" {
		k = inputKey{text: '\t'}
	}
	action := t.editor.apply(k, &r.history, r.complete)
	if action == "eof" {
		t.exit = true
		return
	}
	if action != "enter" {
		return
	}
	line := t.editor.value()
	r.history.Add(line)
	if len(line) > 0 && line[len(line)-1] == '\\' && (len(line) < 2 || line[len(line)-2] != '\\') {
		t.prefix += line[:len(line)-1] + "\n"
		t.editor.replace(nil)
		return
	}
	if t.prefix+line == "" {
		return
	}
	t.queued = t.prefix + line
	t.editor.replace([]rune(t.queued))
	r.regionStatus.queued = true
}

// redrawRegionLocked keeps spinner ticks and type-ahead on the same caret.
func (r *terminalReader) redrawRegionLocked() {
	if r.region == nil {
		return
	}
	width, height, err := r.size()
	if err != nil || width < 2 || height < 2 {
		return
	}
	r.width = width
	e := r.nextEditor
	prefix := ""
	if r.turn != nil {
		e, prefix = r.turn.editor, r.turn.prefix
	}
	if e == nil {
		e = &editor{}
	}
	e.width = width
	e.prompt = regionPrompt(r.regionStatus, r.styled)
	var rows []string
	var row, col int
	if prefix == "" {
		rows, row, col = regionInputRows(e.buffer, e.caret, width, r.regionStatus, r.styled)
	} else {
		rows, row, col = regionContinuationRows(prefix, e.buffer, e.caret, width, r.regionStatus, r.styled)
	}
	r.region.drawLocked(rows, row, col, width, height)
}

func (r *terminalReader) closeRegion() {
	if r.region == nil {
		return
	}
	r.pauseTurnInput()
	r.region.collapse()
	r.releaseRaw()
}

func (r *terminalReader) showEmptyRegion() {
	if r.region == nil {
		return
	}
	width, height, err := r.size()
	if err != nil || width < 2 || height < 2 {
		return
	}
	r.width = width
	rows, row, col := regionInputRows(nil, 0, width, r.regionStatus, r.styled)
	r.region.draw(rows, row, col, width, height)
}

func (r *terminalReader) submitRegionLine(line string) {
	if r.region == nil {
		return
	}
	if line != "" {
		r.region.submit(regionSubmitted(line, r.width, r.styled))
	}
	if !r.region.active {
		r.showEmptyRegion()
	}
}

func (r *terminalReader) handoffRegion() error {
	if r.region == nil {
		return nil
	}
	r.pauseTurnInput()
	r.region.collapse()
	r.releaseRaw()
	return nil
}

func (r *terminalReader) restoreRegion() error {
	if r.region == nil {
		return nil
	}
	restore, err := r.rawMode()
	if err != nil {
		return err
	}
	r.rawRestore = restore
	r.showEmptyRegion()
	return nil
}
