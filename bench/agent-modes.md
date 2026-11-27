# Does delegation earn its cost?

Run two paired comparisons on the same tasks and binary. `solo` vs `agents` measures the value of *offering* delegation. `agents` vs `agents-ask` measures the incremental value of allowing a child to ask its parent. Keep runs that never spawn or ask in their assigned condition: hard tasks may prompt more delegation, so comparing spontaneous users against nonusers answers a different question.

## Pilot

Start with the eight tasks in `bench/tasks.txt`. One pass of all three modes is 24 task-runs. Set a spend ceiling before launching. Pin the tested revision **including the flag**, model, effort, task set, prompt, task timeout and root budgets. Run one label per mode/pass, for example:

```sh
python3 bench/run.py --ref "$REF" --model "$MODEL" --effort "$EFFORT" --label solo-r1 --agent-mode solo
python3 bench/run.py --ref "$REF" --model "$MODEL" --effort "$EFFORT" --label agents-r1 --agent-mode agents
python3 bench/run.py --ref "$REF" --model "$MODEL" --effort "$EFFORT" --label ask-r1 --agent-mode agents-ask
python3 bench/compare.py solo-r1 agents-r1 ask-r1
```

For repetitions, rotate the order of arms across passes, or randomize within task if running one task per label. Give every label a unique name. Don't stop an arm early because it is losing. Record provider/harness failures separately and predefine any infrastructure reruns. The eight tasks come from just two repositories: use this pilot to check instrumentation and estimate variance, not to choose a default.

## Readout

- **Correctness:** use the SWE-bench evaluator's resolved result, not the agent's completion status. For each contrast, tabulate both solved, baseline only, treatment only, neither. The paired change in solve rate is `(treatment only - baseline only) / number of pairs`. With repeats, show per-task solve fractions and uncertainty resampling whole tasks rather than treating repeats as independent issues.
- **Cost and speed:** compare total tokens over the whole tree, cached input separately, and elapsed seconds. If using dollars, fix the same price schedule and weight uncached input, cached input, and output separately. Include failed runs in the numerator of total cost per solve; if there are no solves, report that rather than a ratio. Flag incomplete/unknown usage, don't price it as complete. Equal root Step/ToolCall limits do **not** impose equal total compute. Call this an equal-wall-time-limit comparison, not an equal-token-budget experiment. A hard equal-spend study needs a separate tree-wide budget.
- **Mechanism:** count runs with a direct spawn, questions delivered to the root, and replies (`agent.send` with `question_id`). Inspect paired wins and losses for what changed after the answer. The root trace doesn't count grandchild questions addressed to another child. If nobody asks, the trial measured the cost of *offering* the tool, not its usefulness when exercised.
- **Target the communication mechanism separately:** give only the parent a decision (e.g., compatibility policy), then assign the child a fixed brief that omits it. Compare the same delegation with and without questions against a machine-checked requirement. Add a control where the brief already contains the decision, so unnecessary questions cost time without improving correctness. Do not pool these deliberately constructed cases with the natural SWE-bench results.

Before expanding to more repositories/tasks, set a decision criterion: e.g., higher solve rate within an agreed cost ceiling, or equivalent solve rate with lower cost or latency. If the paired differences remain too uncertain, report that instead of declaring a winner.
