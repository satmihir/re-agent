package reagent

import (
	"fmt"
	"io"
	"syscall"
	"time"
)

// v0 §10 amendment (2026-09-30): type-ahead is not consent to a question not yet shown.
func discardTerminalInput(fd int) (err error) {
	flags, _, errno := syscall.Syscall(syscall.SYS_FCNTL, uintptr(fd), syscall.F_GETFL, 0)
	if errno != 0 {
		return boundaryError{"read terminal flags", errno}
	}
	if err := syscall.SetNonblock(fd, true); err != nil {
		return boundaryError{"drain terminal input", err}
	}
	defer func() {
		if restoreErr := syscall.SetNonblock(fd, flags&syscall.O_NONBLOCK != 0); restoreErr != nil && err == nil {
			err = boundaryError{"restore terminal flags", restoreErr}
		}
	}()
	var pending [256]byte
	for discarded := 0; discarded <= MaxResultBytes; {
		n, readErr := syscall.Read(fd, pending[:])
		switch readErr {
		case syscall.EAGAIN:
			return nil
		case syscall.EINTR:
			continue
		case nil:
			if n == 0 {
				return io.EOF
			}
			discarded += n
		default:
			return boundaryError{"discard terminal input", readErr}
		}
	}
	// A continuing stream refuses the picker rather than waiting forever or granting on leftover keys.
	return fmt.Errorf("too much pending terminal input; consent refused")
}

// pollingReader permits width checks or cancellation while terminal input is idle.
// An empty read lets the caller check its state before polling again.
type pollingReader struct{ fd int }

func (r pollingReader) Read(p []byte) (n int, err error) {
	flags, _, errno := syscall.Syscall(syscall.SYS_FCNTL, uintptr(r.fd), syscall.F_GETFL, 0)
	if errno != 0 {
		return 0, boundaryError{"read terminal flags", errno}
	}
	if err := syscall.SetNonblock(r.fd, true); err != nil {
		return 0, boundaryError{"poll terminal", err}
	}
	defer func() {
		if restoreErr := syscall.SetNonblock(r.fd, flags&syscall.O_NONBLOCK != 0); restoreErr != nil && err == nil {
			err = boundaryError{"restore terminal flags", restoreErr}
		}
	}()
	n, err = syscall.Read(r.fd, p)
	if err == syscall.EAGAIN || err == syscall.EINTR {
		time.Sleep(50 * time.Millisecond)
		return 0, nil
	}
	if err != nil {
		return 0, boundaryError{"read terminal", err}
	}
	if n == 0 {
		return 0, io.EOF
	}
	return n, nil
}
