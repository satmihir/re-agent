# re:agent
#
#   make          build ./reagent
#   make check    format, vet, and the offline suite; needs no credentials
#   make live     conformance runs against both providers; reads keys from .env

.PHONY: all build check live

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
