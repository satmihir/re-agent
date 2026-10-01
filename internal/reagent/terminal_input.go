package reagent

import (
	"fmt"
	"io"
	"syscall"
)

// v0 §10 amendment (2026-09-30): type-ahead is not consent to a question not yet shown.
func discardTerminalInput(fd int) (err error) {
	flags, _, errno := syscall.Syscall(syscall.SYS_FCNTL, uintptr(fd), syscall.F_GETFL, 0)
	if errno != 0 {
		return fmt.Errorf("read terminal flags: %w", errno)
	}
	if err := syscall.SetNonblock(fd, true); err != nil {
		return fmt.Errorf("drain terminal input: %w", err)
	}
	defer func() {
		if restoreErr := syscall.SetNonblock(fd, flags&syscall.O_NONBLOCK != 0); restoreErr != nil && err == nil {
			err = fmt.Errorf("restore terminal flags: %w", restoreErr)
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
			return fmt.Errorf("discard terminal input: %w", readErr)
		}
	}
	// A continuing stream refuses the picker rather than waiting forever or granting on leftover keys.
	return fmt.Errorf("too much pending terminal input; consent refused")
}
