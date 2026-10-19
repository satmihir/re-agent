package reagent

import (
	"context"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"runtime"
	"strconv"
	"strings"
	"syscall"
	"time"
)

type execProcess struct {
	pid, ppid, pgid int
	state           string
}

func execProcessSnapshot() ([]execProcess, error) {
	if runtime.GOOS == "linux" {
		entries, err := os.ReadDir("/proc")
		if err != nil {
			return nil, err
		}
		var processes []execProcess
		for _, entry := range entries {
			pid, err := strconv.Atoi(entry.Name())
			if err != nil {
				continue
			}
			stat, err := os.ReadFile("/proc/" + entry.Name() + "/stat")
			if os.IsNotExist(err) {
				continue
			}
			if err != nil {
				return nil, err
			}
			// The parenthesized command name can itself contain spaces or ')'.
			fields := strings.Fields(string(stat)[strings.LastIndexByte(string(stat), ')')+1:])
			if len(fields) < 3 {
				return nil, fmt.Errorf("invalid process stat for PID %d", pid)
			}
			ppid, err := strconv.Atoi(fields[1])
			if err != nil {
				return nil, err
			}
			pgid, err := strconv.Atoi(fields[2])
			if err != nil {
				return nil, err
			}
			processes = append(processes, execProcess{pid: pid, ppid: ppid, pgid: pgid, state: fields[0]})
		}
		return processes, nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	output, err := exec.CommandContext(ctx, "/bin/ps", "-axo", "pid=,ppid=,pgid=").Output()
	if err != nil {
		return nil, err
	}
	var processes []execProcess
	for _, line := range strings.Split(string(output), "\n") {
		if strings.TrimSpace(line) == "" {
			continue
		}
		var process execProcess
		if _, err := fmt.Sscan(line, &process.pid, &process.ppid, &process.pgid); err != nil {
			return nil, err
		}
		processes = append(processes, process)
	}
	return processes, nil
}

func terminateExecTree(pid int) ([]int, error) {
	processes, snapshotErr := execProcessSnapshot()
	children := make(map[int][]execProcess)
	for _, process := range processes {
		children[process.ppid] = append(children[process.ppid], process)
	}
	tree := []execProcess{{pid: pid, pgid: pid}}
	for i := 0; i < len(tree); i++ {
		tree = append(tree, children[tree[i].pid]...)
	}
	groups := make(map[int]bool)
	for _, process := range tree {
		// A descendant may join our group; never signal the harness itself.
		if process.pgid > 0 && process.pgid != syscall.Getpgrp() {
			groups[process.pgid] = true
		}
	}
	signal := func(sig syscall.Signal) {
		for pgid := range groups {
			_ = syscall.Kill(-pgid, sig)
		}
		for _, process := range tree {
			_ = syscall.Kill(process.pid, sig)
		}
	}
	signal(syscall.SIGTERM)
	time.Sleep(200 * time.Millisecond)
	signal(syscall.SIGKILL)

	deadline := time.Now().Add(200 * time.Millisecond)
	for {
		alive, err := liveExecPIDs(tree)
		if err != nil {
			return alive, errors.Join(snapshotErr, err)
		}
		if len(alive) == 0 || time.Now().After(deadline) {
			return alive, snapshotErr
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func liveExecPIDs(tree []execProcess) ([]int, error) {
	var candidates []int
	var names []string
	for _, process := range tree {
		if syscall.Kill(process.pid, 0) != syscall.ESRCH {
			candidates = append(candidates, process.pid)
			names = append(names, strconv.Itoa(process.pid))
		}
	}
	if len(candidates) == 0 {
		return nil, nil
	}
	if runtime.GOOS == "linux" {
		processes, err := execProcessSnapshot()
		if err != nil {
			return candidates, err
		}
		live := make(map[int]bool)
		for _, process := range processes {
			live[process.pid] = process.state != "Z" && process.state != "X"
		}
		var alive []int
		for _, pid := range candidates {
			if live[pid] {
				alive = append(alive, pid)
			}
		}
		return alive, nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	output, err := exec.CommandContext(ctx, "/bin/ps", "-o", "pid=,stat=", "-p", strings.Join(names, ",")).Output()
	var exitErr *exec.ExitError
	if err != nil && !(errors.As(err, &exitErr) && exitErr.ExitCode() == 1 && len(output) == 0) {
		return candidates, err
	}
	var alive []int
	for _, line := range strings.Split(string(output), "\n") {
		fields := strings.Fields(line)
		if len(fields) != 2 {
			continue
		}
		pid, err := strconv.Atoi(fields[0])
		if err != nil {
			return candidates, err
		}
		if !strings.HasPrefix(fields[1], "Z") && !strings.HasPrefix(fields[1], "X") {
			alive = append(alive, pid)
		}
	}
	return alive, nil
}
