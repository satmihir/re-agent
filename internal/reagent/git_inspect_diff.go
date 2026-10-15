package reagent

import (
	"bytes"
	"fmt"
	"strconv"
	"strings"
)

const (
	gitInspectDiffMaxBytes    = 1 << 20
	gitInspectDiffMaxDistance = 512
	gitInspectDiffMaxWork     = 16 << 20
)

// gitInspectUnified renders one-sided changes directly, otherwise using classic
// Myers O(ND); exceeding its byte, distance or work bounds returns no partial patch.
func gitInspectUnified(old, current []byte, oldName, newName string) (string, bool) {
	if len(old) > gitInspectDiffMaxBytes || len(current) > gitInspectDiffMaxBytes {
		return "", false
	}
	if bytes.Equal(old, current) {
		return "", true
	}
	a, b := gitInspectLineEnds(old), gitInspectLineEnds(current)
	var edits []byte
	switch {
	case len(old) == 0:
		edits = bytes.Repeat([]byte{'+'}, len(b)-1)
	case len(current) == 0:
		edits = bytes.Repeat([]byte{'-'}, len(a)-1)
	default:
		var ok bool
		edits, ok = gitInspectMyers(old, current, a, b)
		if !ok {
			return "", false
		}
	}
	return gitInspectRenderUnified(old, current, a, b, edits, oldName, newName), true
}

func gitInspectLineEnds(data []byte) []int {
	count := bytes.Count(data, []byte{'\n'})
	if len(data) > 0 && data[len(data)-1] != '\n' {
		count++
	}
	ends := make([]int, 1, count+1)
	for i, c := range data {
		if c == '\n' {
			ends = append(ends, i+1)
		}
	}
	if ends[len(ends)-1] != len(data) {
		ends = append(ends, len(data))
	}
	return ends
}

func gitInspectMyers(old, current []byte, a, b []int) ([]byte, bool) {
	n, m := len(a)-1, len(b)-1
	if n-m > gitInspectDiffMaxDistance || m-n > gitInspectDiffMaxDistance {
		return nil, false
	}
	offset := gitInspectDiffMaxDistance + 1
	v := make([]int, 2*gitInspectDiffMaxDistance+3)
	trace := make([][]int, 0, gitInspectDiffMaxDistance+1)
	work := 0
	for d := 0; d <= gitInspectDiffMaxDistance; d++ {
		trace = append(trace, append([]int(nil), v...))
		for k := -d; k <= d; k += 2 {
			work++
			if work > gitInspectDiffMaxWork {
				return nil, false
			}
			// Equal predecessor reaches choose deletion; backtracking uses the same rule.
			x := v[offset+k-1] + 1
			if k == -d || (k != d && v[offset+k-1] < v[offset+k+1]) {
				x = v[offset+k+1]
			}
			y := x - k
			for x < n && y < m {
				left, right := old[a[x]:a[x+1]], current[b[y]:b[y+1]]
				work++
				if len(left) == len(right) {
					work += len(left)
				}
				if work > gitInspectDiffMaxWork {
					return nil, false
				}
				if !bytes.Equal(left, right) {
					break
				}
				x++
				y++
			}
			v[offset+k] = x
			if x >= n && y >= m {
				return gitInspectBacktrack(trace, n, m, offset), true
			}
		}
	}
	return nil, false
}

func gitInspectBacktrack(trace [][]int, x, y, offset int) []byte {
	edits := make([]byte, 0, x+y)
	for d := len(trace) - 1; d > 0; d-- {
		v, k := trace[d], x-y
		previous := k - 1
		if k == -d || (k != d && v[offset+k-1] < v[offset+k+1]) {
			previous = k + 1
		}
		px := v[offset+previous]
		py := px - previous
		for x > px && y > py {
			edits = append(edits, ' ')
			x--
			y--
		}
		if x == px {
			edits = append(edits, '+')
			y--
		} else {
			edits = append(edits, '-')
			x--
		}
	}
	for x > 0 && y > 0 {
		edits = append(edits, ' ')
		x--
		y--
	}
	for i, j := 0, len(edits)-1; i < j; i, j = i+1, j-1 {
		edits[i], edits[j] = edits[j], edits[i]
	}
	return edits
}

func gitInspectRenderUnified(old, current []byte, a, b []int, edits []byte, oldName, newName string) string {
	type hunk struct{ start, end int }
	var hunks []hunk
	for i, edit := range edits {
		if edit == ' ' {
			continue
		}
		start, end := max(0, i-3), min(len(edits), i+4)
		if len(hunks) > 0 && start <= hunks[len(hunks)-1].end {
			hunks[len(hunks)-1].end = end
			continue
		}
		hunks = append(hunks, hunk{start, end})
	}
	var out strings.Builder
	fmt.Fprintf(&out, "--- %s\n+++ %s\n", gitInspectDiffName(oldName), gitInspectDiffName(newName))
	pos, oldLine, newLine := 0, 0, 0
	for _, h := range hunks {
		for ; pos < h.start; pos++ {
			if edits[pos] != '+' {
				oldLine++
			}
			if edits[pos] != '-' {
				newLine++
			}
		}
		oldCount, newCount := 0, 0
		for _, edit := range edits[h.start:h.end] {
			if edit != '+' {
				oldCount++
			}
			if edit != '-' {
				newCount++
			}
		}
		fmt.Fprintf(&out, "@@ -%s +%s @@\n", gitInspectHunkRange(oldLine, oldCount), gitInspectHunkRange(newLine, newCount))
		for ; pos < h.end; pos++ {
			edit := edits[pos]
			var line []byte
			if edit == '+' {
				line = current[b[newLine]:b[newLine+1]]
			} else {
				line = old[a[oldLine]:a[oldLine+1]]
			}
			out.WriteByte(edit)
			out.Write(line)
			if line[len(line)-1] != '\n' {
				out.WriteString("\n\\ No newline at end of file\n")
			}
			if edit != '+' {
				oldLine++
			}
			if edit != '-' {
				newLine++
			}
		}
	}
	return out.String()
}

func gitInspectHunkRange(before, count int) string {
	if count == 0 {
		return strconv.Itoa(before) + ",0"
	}
	if count == 1 {
		return strconv.Itoa(before + 1)
	}
	return strconv.Itoa(before+1) + "," + strconv.Itoa(count)
}

func gitInspectDiffName(name string) string {
	if !strings.ContainsFunc(name, func(c rune) bool { return c < ' ' || c >= 0x7f || c == '\\' || c == '"' }) {
		return name
	}
	// Git's quoted paths use octal bytes, not Go's hex/Unicode escapes.
	var out strings.Builder
	out.WriteByte('"')
	for i := 0; i < len(name); i++ {
		c := name[i]
		switch c {
		case '\\', '"':
			out.WriteByte('\\')
			out.WriteByte(c)
		case '\n':
			out.WriteString(`\n`)
		case '\t':
			out.WriteString(`\t`)
		case '\r':
			out.WriteString(`\r`)
		default:
			if c < ' ' || c >= 0x7f {
				fmt.Fprintf(&out, `\%03o`, c)
			} else {
				out.WriteByte(c)
			}
		}
	}
	out.WriteByte('"')
	return out.String()
}
