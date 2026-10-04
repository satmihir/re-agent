package reagent

import (
	"fmt"
	"strconv"
	"strings"
	"syscall"
	"time"
)

// cursorRow asks the terminal once per activation. Unrelated type-ahead is kept
// for the editor; a terminal that does not answer within 100 ms is not trusted.
func cursorRow(fd int, out *terminalRegion) int {
	if !out.raw {
		return 0
	}
	flags, _, errno := syscall.Syscall(syscall.SYS_FCNTL, uintptr(fd), syscall.F_GETFL, 0)
	if errno != 0 || syscall.SetNonblock(fd, true) != nil {
		return 0
	}
	defer syscall.SetNonblock(fd, flags&syscall.O_NONBLOCK != 0)
	fmt.Fprint(out.out, "\x1b[6n")
	var pending []byte
	defer func() { out.keys.pending = append(out.keys.pending, pending...) }()
	deadline := time.Now().Add(100 * time.Millisecond)
	for time.Now().Before(deadline) && len(pending) < 256 {
		var chunk [64]byte
		n, err := syscall.Read(fd, chunk[:])
		if n > 0 {
			pending = append(pending, chunk[:n]...)
		}
		for i := 0; i+4 < len(pending); i++ {
			if pending[i] != '\x1b' || pending[i+1] != '[' {
				continue
			}
			for end := i + 2; end < len(pending); end++ {
				if pending[end] != 'R' {
					continue
				}
				row, col, ok := parseCursorPosition(string(pending[i+2 : end]))
				if ok && row > 0 && col > 0 {
					pending = append(pending[:i], pending[end+1:]...)
					return row
				}
				break
			}
		}
		if err != nil && err != syscall.EAGAIN && err != syscall.EINTR {
			return 0
		}
		time.Sleep(5 * time.Millisecond)
	}
	return 0
}

func parseCursorPosition(text string) (int, int, bool) {
	rowText, colText, found := strings.Cut(text, ";")
	if !found {
		return 0, 0, false
	}
	row, err := strconv.Atoi(rowText)
	if err != nil {
		return 0, 0, false
	}
	col, err := strconv.Atoi(colText)
	return row, col, err == nil
}
