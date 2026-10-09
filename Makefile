# re:agent
#
#   make          build ./reagent
#   make check    format, vet, and the offline suite; needs no credentials
#   make live     conformance runs against both providers; reads ./.env or main checkout .env in a worktree
#   make tty-check  drive the chat in a pseudo-terminal and check the screen; offline
#   make git-do-eval  repeated git_do decisions; MODE=jev|recipe N=5 SET=all|tuning|held_out; Jev reads .env

.PHONY: all build check live tty-check git-do-eval

all: build

build:
	go build -o reagent ./cmd/reagent

check:
	@test -z "$$(gofmt -l .)" || { echo "gofmt: these files need formatting:"; gofmt -l .; exit 1; }
	go vet ./...
	go test ./...

# Loads ./.env first, otherwise the main checkout's .env in a linked worktree.
# Only the path is printed, never the keys. A partial .env runs what it can;
# no provider key at all is an error rather than a quiet pass.
load_env = env_file=; \
	if [ -f ./.env ]; then env_file=./.env; \
	else common_dir=$$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || common_dir=; \
		if [ "$$(basename "$$common_dir")" = .git ] && [ -f "$$(dirname "$$common_dir")/.env" ]; then env_file="$$(dirname "$$common_dir")/.env"; fi; \
	fi; \
	if [ -n "$$env_file" ]; then echo "loading $$env_file"; set -a; . "$$env_file"; set +a; \
	else echo "no .env found"; fi
live:
	@$(load_env); \
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

# Repeats each case; recipe mode uses only actual model-named outcomes and needs no key.
# Wrong runs or <90% held-out real git-only right-and-ran fail (v0 §10.9).
git-do-eval:
	@$(load_env); \
	if [ "$(MODE)" = jev ] && [ -z "$$TYPESAFE_API_KEY" ]; then echo "no TYPESAFE_API_KEY set; add it to .env"; exit 1; fi; \
	REAGENT_GIT_DO_EVAL=1 REAGENT_GIT_DO_EVAL_MODE=$(MODE) REAGENT_GIT_DO_EVAL_REPEATS=$(N) REAGENT_GIT_DO_EVAL_SET=$(SET) \
	go test ./internal/reagent/ -run '^TestGitDoEval$$' -v -count=1

MODE ?= jev
N ?= 5
SET ?= all
