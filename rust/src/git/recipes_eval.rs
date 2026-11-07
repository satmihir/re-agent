//! The git_do decision eval: replays recorded and hand-written requests against
//! fixed repository states and tallies which recipe would run. It never runs a
//! recipe. The live sweep is opt-in (`make git-do-eval`); the rest is offline.

use super::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const SPLIT_RULE: &str = "SHA-256(call_id) first byte odd = held_out; even = tuning. Fixed before evaluation; never tune on held_out. Reconstructed snapshots use the turn's workspace_state, preceding tool edits and git/PR history; original calls predate decision logging.";
const REAL: &str = include_str!("../../testdata/git_do_real_cases.jsonl");
const HAND: &str = include_str!("../../testdata/git_do_eval_cases.json");

struct Case {
    call_id: String,
    group: String,
    set: String,
    args: Value,
    snapshot: Value,
    expected: Vec<String>,
    named_recipe: String,
}

#[derive(Default)]
struct Tally {
    cases: usize,
    repeats: usize,
    git_only: usize,
    right_ran: usize,
    false_declines: usize,
    wrong_ran: usize,
    correct_declines: usize,
    changed: usize,
    skipped: usize,
}

fn split(call_id: &str) -> &'static str {
    if Sha256::digest(call_id.as_bytes())[0] % 2 == 1 {
        "held_out"
    } else {
        "tuning"
    }
}

fn strings_of(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn real_cases() -> Vec<Case> {
    let mut lines = REAL.lines();
    let header: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert!(
        header["kind"] == "split" && header["rule"] == SPLIT_RULE,
        "invalid eval split header"
    );
    let mut seen = BTreeSet::new();
    let mut cases = vec![];
    for line in lines {
        let c: Value = serde_json::from_str(line).unwrap();
        let call_id = s(&c, "call_id").to_string();
        let expected = strings_of(&c["expected"]);
        let named_recipe = s(&c, "named_recipe").to_string();
        assert!(
            !call_id.is_empty()
                && seen.insert(call_id.clone())
                && !s(&c, "session_id").is_empty()
                && !s(&c, "run_id").is_empty()
                && c["intent"] == c["arguments"]["intent"]
                && c["reconstructed"] == true
                && !expected.is_empty()
                && !s(&c["snapshot"], "branch").is_empty()
                && !s(&c["snapshot"], "default_branch").is_empty()
                && ["done", "declined"].contains(&s(&c, "status")),
            "incomplete or duplicate eval case: {line}"
        );
        for want in &expected {
            assert!(
                want == "decline" || named(want),
                "unknown expected recipe {want}"
            );
        }
        assert!(
            named_recipe.is_empty()
                || (named(&named_recipe)
                    && s(&c["arguments"], "intent").starts_with(&format!("{named_recipe}:"))),
            "named recipe was not in the model intent: {call_id}"
        );
        cases.push(Case {
            group: "real".into(),
            set: split(&call_id).into(),
            call_id,
            args: c["arguments"].clone(),
            snapshot: c["snapshot"].clone(),
            expected,
            named_recipe,
        });
    }
    assert!(!cases.is_empty(), "missing real evaluation cases");
    cases
}

fn all_cases() -> Vec<Case> {
    let hand: Value = serde_json::from_str(HAND).unwrap();
    let mut cases = real_cases();
    for c in hand["cases"].as_array().unwrap() {
        let mut expected = strings_of(&c[3]);
        if expected == ["none"] {
            expected = vec!["decline".into()];
        }
        let state = &hand["states"][c[2].as_str().unwrap()];
        assert!(state.is_object(), "unknown eval state {}", c[2]);
        cases.push(Case {
            call_id: String::new(),
            group: format!("hand/{}", c[0].as_str().unwrap()),
            set: "hand".into(),
            args: json!({"intent": c[1]}),
            snapshot: state.clone(),
            expected,
            named_recipe: String::new(),
        });
    }
    cases
}

/// Builds a Run over a recorded snapshot instead of a live repository.
fn run_for<'a>(c: &Case, root: &'a Path, jev: bool) -> Run<'a> {
    let mut args = c.args.clone();
    if !jev && s(&args, "recipe").is_empty() {
        args["recipe"] = json!(c.named_recipe);
    }
    Run {
        root,
        pr: args["pr"].as_i64().unwrap_or(0),
        args,
        state: c.snapshot.clone(),
        changed: strings_of(&c.snapshot["changed"]),
        finished_head: String::new(),
        observed: String::new(),
        steps: vec![],
        result: json!({"recipe":"","chosen_by":"model","status":"","steps":[],"decision":{"snapshot":c.snapshot,"probabilities":{}}}),
        jev,
        prefix: String::new(),
        cancellation: Default::default(),
    }
}

/// Describes one wrong outcome with the probabilities and threshold behind it.
fn finding(key: &str, c: &Case, repeat: usize, result: &Value, ran: bool, jev: bool) -> String {
    let decision = &result["decision"];
    let mut detail = "outcome_p=n/a outcome_threshold=n/a scope_more_p=n/a".to_string();
    if jev {
        let mut choice = s(decision, "jev_outcome");
        if choice.is_empty() {
            choice = s(result, "recipe");
        }
        let mut threshold = decision["threshold"].as_f64().unwrap_or(0.0);
        if decision["declined_by"] == "scope" {
            threshold = base_threshold(choice);
            if !s(decision, "named_recipe").is_empty() {
                threshold = if decision["named_agreed"] == true
                    && choice != "rebase_push"
                    && choice != "revert_pr"
                {
                    0.7
                } else {
                    0.9
                };
            }
        }
        let p = |q: &str, k: &str| decision["probabilities"][q][k].as_f64().unwrap_or(0.0);
        detail = format!(
            "jev_outcome={choice} outcome_p={:.2} outcome_threshold={threshold:.2} scope_more_p={:.2}",
            p("outcome", choice),
            p("scope", "more")
        );
    }
    format!(
        "{key} {} repeat {repeat}: {} {detail} declined_by={} ran={ran} want={} intent={:?}",
        c.call_id,
        s(result, "recipe"),
        s(decision, "declined_by"),
        c.expected.join("|"),
        s(&c.args, "intent")
    )
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.into())
}

#[test]
fn git_do_eval() {
    if std::env::var("REAGENT_GIT_DO_EVAL").as_deref() != Ok("1") {
        eprintln!("set REAGENT_GIT_DO_EVAL=1 to run the opt-in evaluation");
        return;
    }
    let mode = env_or("REAGENT_GIT_DO_EVAL_MODE", "jev");
    assert!(
        mode == "jev" || mode == "recipe",
        "unknown eval mode {mode:?}"
    );
    let jev = mode == "jev";
    if jev {
        assert!(
            !std::env::var("TYPESAFE_API_KEY")
                .unwrap_or_default()
                .is_empty(),
            "TYPESAFE_API_KEY required for Jev eval"
        );
    }
    let raw = env_or("REAGENT_GIT_DO_EVAL_REPEATS", "5");
    let repeats: usize = raw
        .parse()
        .ok()
        .filter(|n| *n >= 1)
        .unwrap_or_else(|| panic!("invalid repeat count {raw:?}"));
    let set = env_or("REAGENT_GIT_DO_EVAL_SET", "all");
    assert!(
        ["all", "tuning", "held_out"].contains(&set.as_str()),
        "unknown eval set {set:?}"
    );
    let root = Path::new(".");
    let mut groups: BTreeMap<String, Tally> = BTreeMap::new();
    let mut findings = vec![];
    for c in all_cases() {
        if (set == "tuning" && c.set == "held_out") || (set == "held_out" && c.set != "held_out") {
            continue;
        }
        let key = format!("{}/{}", c.group, c.set);
        let g = groups.entry(key.clone()).or_default();
        g.cases += 1;
        if !jev && c.named_recipe.is_empty() && s(&c.args, "recipe").is_empty() {
            g.skipped += 1; // No observed model recipe; never infer one from the label.
            continue;
        }
        let mut answers = BTreeSet::new();
        for repeat in 1..=repeats {
            let mut run = run_for(&c, root, jev);
            let allowed = run.feasible();
            let ran = run.choose(&allowed).is_ok();
            let choice = s(&run.result, "recipe").to_string();
            answers.insert(format!("{choice}/{ran}"));
            g.repeats += 1;
            let declines = c.expected[0] == "decline";
            if !declines {
                g.git_only += 1;
            }
            let right = c.expected.contains(&choice);
            match (ran, declines, right) {
                (false, true, _) => g.correct_declines += 1,
                (false, false, _) => g.false_declines += 1,
                (true, _, true) => g.right_ran += 1,
                (true, _, false) => g.wrong_ran += 1,
            }
            if (!ran && !declines) || (ran && !right) {
                findings.push(finding(&key, &c, repeat, &run.result, ran, jev));
            }
        }
        if answers.len() > 1 {
            g.changed += 1;
        }
    }
    for (key, g) in &groups {
        eprintln!(
            "{key:<20} cases={} repeats={} right-and-ran={} false-declines={} wrong-and-ran={} correct-declines={} changed-answer-cases={} skipped-no-model-name={}",
            g.cases, g.repeats, g.right_ran, g.false_declines, g.wrong_ran, g.correct_declines, g.changed, g.skipped
        );
    }
    for line in &findings {
        eprintln!("{line}");
    }
    let wrong: usize = groups.values().map(|g| g.wrong_ran).sum();
    let mut failures = vec![];
    if wrong > 0 {
        failures.push(format!("eval gate: {wrong} wrong recipes would run"));
    }
    if set != "tuning" {
        // Never inspect held-out outcomes during tuning.
        let held = groups.get("real/held_out").filter(|g| g.git_only > 0);
        let held = held.expect("no held-out real git-only attempts evaluated");
        eprintln!(
            "real/held_out git-only right-and-ran={}/{} ({:.1}%); gate >=90%",
            held.right_ran,
            held.git_only,
            100.0 * held.right_ran as f64 / held.git_only as f64
        );
        if held.right_ran * 10 < held.git_only * 9 {
            failures.push(format!(
                "held-out gate: right-and-ran={}/{} (need >=90%)",
                held.right_ran, held.git_only
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn eval_cases_split_by_call_id_and_expect_feasible_recipes() {
    let mut sets = BTreeMap::new();
    for c in real_cases() {
        *sets.entry(c.set.clone()).or_insert(0) += 1;
        let run = run_for(&c, Path::new("."), true);
        for want in &c.expected {
            assert!(
                want == "decline" || run.feasible().contains(&want.as_str()),
                "{}: {want} is not feasible",
                c.call_id
            );
        }
    }
    assert!(
        sets.get("tuning").is_some() && sets.get("held_out").is_some(),
        "missing a split: {sets:?}"
    );
    let hand = all_cases().len() - real_cases().len();
    assert_eq!(hand, 102);
}

#[test]
fn eval_replays_historical_declines() {
    let by_id: BTreeMap<String, Case> = real_cases()
        .into_iter()
        .map(|c| (c.call_id.clone(), c))
        .collect();
    let answers = |outcome: &str, p: f64, scope: &str, q: f64| {
        let other = if scope == "more" { "git_only" } else { "more" };
        json!({
            "outcome": {"choice": outcome, "probabilities": {outcome: p, "none": 1.0 - p}},
            "scope": {"choice": scope, "probabilities": {scope: q, other: 1.0 - q}},
        })
    };
    for (id, answers, choice, ran) in [
        (
            "call_f6c99083bd4649a5a8f666b288d316cc",
            answers("new_branch", 0.88, "git_only", 0.99),
            "new_branch",
            true,
        ),
        (
            "call_bbf6a735470d4ad88bd97f748e6cc8e6",
            answers("follow_up_pr", 0.95, "more", 0.69),
            "more",
            false,
        ),
        (
            "call_3434e78337314d09b685de1ce2bfc50f",
            answers("ship_pr", 0.89, "git_only", 0.99),
            "ship_pr",
            true,
        ),
    ] {
        let c = by_id.get(id).expect("historical false decline missing");
        let mut run = run_for(c, Path::new("."), true);
        let allowed = run.feasible();
        let prefix = s(&c.args, "intent")
            .split_once(':')
            .map(|(p, _)| p)
            .filter(|p| named(p))
            .unwrap_or("")
            .to_string();
        let result = run.judge(&answers, &allowed, &prefix);
        assert_eq!(
            (s(&run.result, "recipe"), result.is_ok()),
            (choice, ran),
            "{id}"
        );
    }
    let follow = &by_id["call_bbf6a735470d4ad88bd97f748e6cc8e6"];
    let mut run = run_for(follow, Path::new("."), false);
    let allowed = run.feasible();
    assert_eq!(run.choose(&allowed).ok().as_deref(), Some("follow_up_pr"));
}

#[test]
fn eval_findings_include_intent_and_probabilities() {
    let result = json!({"recipe":"more","decision":{"declined_by":"scope","jev_outcome":"follow_up_pr","probabilities":{
        "outcome":{"follow_up_pr":0.95,"none":0.05},"scope":{"more":0.69,"git_only":0.31}}}});
    let c = Case {
        call_id: String::new(),
        group: "hand/agent".into(),
        set: "hand".into(),
        args: json!({"intent":"follow_up_pr: commit, push and comment"}),
        snapshot: Value::Null,
        expected: vec!["follow_up_pr".into()],
        named_recipe: String::new(),
    };
    let line = finding("hand/agent/hand", &c, 2, &result, false, true);
    for want in [
        r#"intent="follow_up_pr: commit, push and comment""#,
        "outcome_p=0.95",
        "outcome_threshold=0.85",
        "scope_more_p=0.69",
    ] {
        assert!(line.contains(want), "{want:?} missing from {line}");
    }
}
