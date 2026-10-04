# re:agent
#
#   make          build ./reagent
#   make check    format, vet, and the offline suite; needs no credentials
#   make live     conformance runs against both providers; reads keys from .env
#   make tty-check  drive the chat in a pseudo-terminal and check the screen; offline

.PHONY: all build check live tty-check

all: build

build:
	go build -o reagent ./cmd/reagent

check:
	@test -z "$$(gofmt -l .)" || { echo "gofmt: these files need formatting:"; gofmt -l .; exit 1; }
	go vet ./...
	go test ./...

# Loads .env when present so keys never pass through a shell history. Each
# provider's test skips itself when its key is absent, so a partial .env still
# runs what it can. No key at all is an error rather than a quiet pass.
live:
	@set -a; [ -f .env ] && . ./.env; set +a; \
	if [ -z "$$OPENAI_API_KEY" ] && [ -z "$$ANTHROPIC_API_KEY" ]; then \
		echo "no OPENAI_API_KEY or ANTHROPIC_API_KEY set; copy .env.example to .env and fill it in"; \
		exit 1; \
	fi; \
	REAGENT_LIVE_TESTS=1 go test ./internal/reagent/ -run Live -v -count=1

# Builds a throwaway binary and drives it through bench/tty's scenarios, which
# exercise the real terminal path that go test cannot: a pty, its buffer and
# flags, and the cursor-report round trip. Needs python3; no keys or network.
tty-check:
	@dir=$$(mktemp -d) && go build -o $$dir/reagent ./cmd/reagent && \
	python3 bench/tty/check.py $$dir/reagent; status=$$?; rm -rf $$dir; exit $$status
