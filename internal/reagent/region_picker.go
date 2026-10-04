package reagent

import (
	"io"
)

// chooseRegion replaces the input with picker rows without changing its key rules.
func (r *terminalReader) chooseRegion(config pickerConfig, header string, p pickerState, width, height int) (int, error) {
	redraw := func() {
		rows := regionPickerRows(header, p.options, p.cursor, width, r.regionStatus, r.styled)
		r.region.draw(rows, 1, 0, width, height)
	}
	redraw()
	defer func() {
		if config.fromPrompt {
			rows, row, col := regionInputRows(nil, 0, width, r.regionStatus, r.styled)
			r.region.draw(rows, row, col, width, height)
		} else {
			r.region.collapse()
		}
	}()
	if config.freshInput && r.region.keys != nil {
		// Cursor-position probing can read type-ahead between the first drain and draw.
		r.keys.pending = nil
		if err := r.discardInput(); err != nil {
			return 0, err
		}
	}
	for {
		var chunk [256]byte
		var n int
		var readErr error
		if len(r.keys.pending) > 0 {
			n = copy(chunk[:], r.keys.pending)
			r.keys.pending = r.keys.pending[n:]
		} else {
			n, readErr = r.keys.inner.Read(chunk[:])
		}
		for _, k := range decodeKeys(chunk[:n], config.shortcuts) {
			if !config.shortcuts && k == keyUnknown {
				return 0, errCancelled
			}
			switch k {
			case keyUp:
				p.move(-1)
				redraw()
			case keyDown:
				p.move(1)
				redraw()
			default:
				if index, done, cancelled := p.choose(k); done {
					return index, nil
				} else if cancelled {
					return 0, errCancelled
				}
			}
		}
		if readErr != nil {
			return 0, readErr
		}
		if n == 0 {
			if r.keys.poll {
				continue
			}
			return 0, io.EOF
		}
	}
}
