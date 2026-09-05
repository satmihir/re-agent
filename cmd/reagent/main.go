// Command reagent runs one agent task and prints its reply.
package main

import (
	"context"
	"os"
	"os/signal"

	"github.com/satmihir/re-agent/internal/reagent"
)

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	defer stop()
	os.Exit(reagent.Main(ctx, os.Args[1:], os.Stdout, os.Stderr))
}
