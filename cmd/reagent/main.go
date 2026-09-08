// Command reagent runs one agent task and prints its reply.
package main

import (
	"context"
	"os"

	"github.com/satmihir/re-agent/internal/reagent"
)

func main() {
	os.Exit(reagent.Main(context.Background(), os.Args[1:], os.Stdin, os.Stdout, os.Stderr))
}
