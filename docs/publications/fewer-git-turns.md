# Fewer turns for git: what we tried and what worked

From early October 2026. It ran from one question to five merged pull
requests (#92–#96), and most of what we expected turned out wrong.

## The question

re:agent's sessions spend a lot of time on git: commit, push, open a pull
request, reply to review. Did that take more expensive model turns than it
should?

The traces said yes. Across 104 real sessions:

| | Share |
|---|---:|
| Model turns that ran only git or `gh` | 965 of 5,742 (17%) |
| Input tokens those turns re-sent | 135M (17%) |
| Model wait time | 96 minutes (10%) |
| git-only turns holding exactly one command | 367 of 368 |

The last row was the clue. Opening a PR typically took six or seven turns:
status, diff, add, commit, push, `gh pr create`, status again. Each one
re-sent the whole conversation, and on git turns that conversation was big:

| Context size on git turns | p25 | p50 | p90 | p99 |
|---|---:|---:|---:|---:|
| Input tokens | 54k | 97k | 307k | 565k |

## A benchmark first (#92)

Arguing from traces only goes so far, so we built `bench/git`, nine common
git tasks:

- open a PR
- follow up on review
- branch from the latest main
- rebase and push
- split commits
- amend
- revert in a PR
- explain a branch
- report PR status

Each task starts from a clone of re:agent pinned at one commit. Its origin is
a local bare repository, and a stub `gh` keeps pull requests in a file, so
nothing reaches GitHub. A checker inspects the end state, and the trace gives
turns, calls, tokens and time.

`--self-test` replays reference solutions through the scripted model. Each
must pass both one command per turn and all commands in one turn, and doing
nothing must fail every task. It calls no provider. Later the benchmark
learned to run Codex (`--agent codex`) and Claude Code (`--agent claude`) on
the same tasks, and that turned out to matter most.

Baseline, three repeats each:

| | Passed | Turns per task (fewest possible: 2) |
|---|---:|---:|
| GPT-6 Sol | 27/27 | 6.5 |
| GPT-6 Luna | 24/27 | 6.3 |

Across those runs, no turn ever contained two changes to the repository.

## What didn't work: asking for batches

The obvious fix was to let the model send several commands in one response.
We tried two versions:
1. An instructions line saying "put the tool calls you already know in one
   response".
2. That, plus making a batch stop at its first failed call, so dependent
   steps would be safe together.

| | Sol turns | Luna turns |
|---|---:|---:|
| `main` | 6.5 | 6.3 |
| Prompt asking for batches | 6.1 | 6.0 |
| …plus batches stop at the first failure | 6.3 | 5.8 |

Across about a thousand turns, still no response held two changes. Models did
batch reads a little more, but not mutations. Neither the prompt nor the
safety change moved them.

## What Codex showed us

The same tasks through the Codex CLI, with the same models and the same
proxy:

| | Sol turns | Luna turns | git commands per call |
|---|---:|---:|---:|
| re:agent | 6.5 | 6.3 | 1.0 |
| Codex | 5.1 | 4.2 | 1.8–2.8 |

Codex never sent two tool calls in one response either. Its gain came from
`&&` inside a single shell command:

```
git switch -c x && git add README.md && git commit -m "…"
git push -u origin x && gh pr create …
```

That was the insight. To a model, separate tool calls in one response read as
parallel, so independent. Models won't put dependent steps there whatever the
harness promises. `a && b && c` is one action that states its own
dependencies, and models have seen a lot of shell written that way. Codex's
shell tool takes a string, which makes `&&` natural. re:agent's `exec` took
an argument vector with no shell, which made chaining awkward.

## What worked: `then` (#93)

`exec` gained an optional `then`: more argument vectors, each run only if the
one before succeeded. It's `&&` without a shell, so every command stays
literal and visible in the trace.

| | Sol turns | Luna turns |
|---|---:|---:|
| `main` | 6.5 | 6.3 |
| `then`, in the tool description only | 4.6 | 5.9 |
| `then`, plus one example in the instructions | **3.8** | **4.0** |
| Codex, for reference | 5.1 | 4.2 |

Sol picked it up from the description. Luna used it 3 times in 27 runs until
the instructions showed one example, then 30 times. The smaller model needed
to be shown the feature, not just offered it. Input tokens per task fell by
44% for Sol and 35% for Luna.

## Same model, different harnesses (#94, #95)

Sonnet 5.5 was efficient everywhere:

| Sonnet 5.5 | Turns per task |
|---|---:|
| re:agent before `then` | 3.2 |
| re:agent with `then` | 2.9 |
| Claude Code | 3.2 |
| re:agent with the #95 fix | **2.7** |

It already wrote `sh -c "… && …"` on its own, so the harness gap was a GPT
problem. Sonnet found a flaw in `then`, though. It sometimes read `argv` as
"the program" and `then` as "its commands", and sent `{"argv":"git",
"then":[…]}`: 4 times in 27 runs, each a wasted turn on a Go decoder message.
#95 made the description show `argv` and `then` together, and made a string
`argv` get an error naming the expected shape. That brought it to 0.

Claude Code sent about 40% fewer uncached tokens than re:agent at similar
turn counts. That one is still open.

## What a turn costs

Cached input costs 10% of the normal input price on Sol, Luna and Sonnet 5.5
(5% on GPT-6.1 Sol and Opus 5.5). In practice the cache misses more than
you'd hope. On real git turns, 15% of input was uncached, from idle gaps and
model switches. That puts an Sol turn at about $0.47 per million context
tokens, not $0.20.

| Session context | One git operation in-session (Sol, 3.8 turns) |
|---|---:|
| 97k (median) | $0.17 |
| 307k (p90) | $0.55 |
| 565k (p99) | $1.01 |

The same work done by a fresh, memoryless agent costs about $0.014 on Sol and
$0.002 on Luna, whatever the parent's context size. In-session cost grows
with the conversation, and a sub-agent's stays flat. That makes delegating
routine outcomes attractive, and it led to the next question.

## Can a classifier do it with no model? (Jev and recipes)

Jev is a fast, cheap choice classifier: you give it state and named options,
and it returns probabilities in about 0.2 seconds. We wrote twelve git
recipes by hand, and let Jev pick which one an English request means:

- `status`, `explain_branch`, `pr_status`
- `commit`, `split_commits`, `amend`
- `commit_push`, `ship_pr`, `follow_up_pr`
- `new_branch`, `rebase_push`, `revert_pr`

A recipe runs its commands and then checks its own result. To find out, we
probed Jev and built an eval of 68 requests. Thirty were users' own wording
from traces ("create a PR", "push + PR", "rebase from origin/main"). The rest
were agent-style, mixed with non-git work, or unsafe.

**Jev's shape:**
- Choice questions only.
- More than 8 options work; the 8 had been re:agent's own cap.
- Several questions can go in one request.
- Every question needs a "none": given two wrong options, Jev still picked
  one at 0.51.

**The first version let 8 wrong recipes run.** All were mixed requests:
"implement, commit and create a PR" became `ship_pr` at 0.99, ignoring the
"implement". Three changes fixed that, none of them extra calls:
1. **Code prunes first.** With no PR open, `follow_up_pr` isn't offered.
2. **A scope question in the same request:** does this also ask for non-git
   work? If so, decline.
3. **Clearer descriptions.**

**A single question beat a tree of questions.** The tree was slower, and its
multiplied confidences declined more. Once it filed "revert … and open a PR"
under "publish" instead of "undo".

With no language model at all, Jev plus recipes passed all 27 benchmark runs
in 0.7 seconds each, using about 1,100 Jev tokens per task. That was after
fixing two bugs in our own code, not Jev's. One of them is the most useful
caution in this whole story. A branch-name parser read "branch called
feat/status-row" as a branch named `called`, and the recipe's own checks
still said "done". **The checks prove the mechanics happened, not that they
matched the request.**

## `git_do` (#96)

Should the model write the request, or should the user's prompt go straight
to Jev? The model, first:
- **Its requests are clean.** Agent-style requests were 100% right and all
  ran. Users' raw wording was 90% right and 77% ran.
- **It handles mixed work.** It does the coding, then calls
  `git_do("commit and open a PR")`.
- **It writes the text recipes can't.** "Create a PR" is the most common git
  request, and the PR body needs to know what was done.

`git_do` takes `intent` plus optional `message`, `title`, `body`, `branch`,
`pr` and `commit`. `--git-do recipe` is the control arm, where the model
names the recipe itself.

| | Passed | Turns per task | Wall time per task |
|---|---:|---:|---:|
| Luna, `main` (with `then`) | 25/27 | 4.2–4.7 | ~12–16 s |
| Luna + `git_do`, Jev chooses | 27/27 | 2.2 | 7.6 s |
| Luna + `git_do`, model chooses | 25/26 | 2.4 | 7.8 s |
| Sol, `main` | 27/27 | 3.8 | 19.3 s |
| Sol + `git_do`, first description | 27/27 | 3.0 | 20.2 s |
| Sol + `git_do`, final description | 27/27 | **2.3** | **7.3 s** |

**Jev against the model choosing:** turns came out about equal, so most of
the gain is the tool's shape: one call that reads the repo itself and returns
proof. Jev was more robust where the model chooses badly. It picked the
commit to revert when Luna didn't pass one.

**Sol's extra turn was a prompt problem.** It ran `status` or `diff` before
23 of 27 calls. One sentence in the description fixed it: `git_do` reads the
repository itself, so call it directly.

**Porting the eval to Go found two more things:**
1. **Rounding.** Jev rounds probabilities to hundredths, so sums of 0.99 are
   normal and must be accepted.
2. **Near misses.** Near a 0.7 threshold, Jev sometimes chose `commit_push`
   for "anything to push to the branch?" (0.73) and `rebase_push` for "pull
   from main and try again" (0.86). Recipes that change the repository now
   need 0.9, and read-only ones keep 0.7.

After that, `make git-do-eval` shows no wrong recipe running. Users'
git-only requests are 90% right and 73% run, and the rest decline to `exec`.

## Lessons

1. **Measure across harnesses.** Our own variations taught us little; one run
   of Codex found the mechanism.
2. **Models chain only in a form that states the dependency.** Parallel tool
   calls read as independent. Shape the tool around that instead of
   promising safety in the prompt.
3. **Smaller models need to be shown, not just told.** One example in the
   instructions did what a tool description couldn't.
4. **The same model can need help in one harness and none in another.** Fix
   for the models you run.
5. **A cheap classifier plus handwritten code can do routine outcomes**, if
   code prunes what the state can't allow, a scope question catches mixed
   requests, and the bar is higher for anything that publishes.
6. **Self-checks verify mechanics, not intent.** Values read from a request
   need their own checks, or should come from the model.
7. **Keep measurement honest.** Count provider errors apart from failures.
   Rerun what looks like a regression, since three repeats are noisy. Don't
   let a pipe swallow a failing test.

## Still open

- **Real sessions.** How often does `git_do` decline? The eval says about a
  quarter of terse user requests.
- **`/git <request>`.** A direct path from the user's prompt for mechanical
  outcomes, with no model turn.
- **Shadow mode.** Classify every prompt with Jev without acting, and compare
  with what the model did, before routing anything automatically.
- **A memoryless git sub-agent** on a cheap model, with these recipes' checks
  as its contract.
- **Claude Code's caching.** Why it sends 40% fewer uncached tokens at the
  same turn counts.
