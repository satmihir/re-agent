use super::*;
use std::{collections::BTreeMap, fs, os::unix::fs::PermissionsExt};

const PREFIX: &[&str] = &[
    "--no-pager",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.attributesFile=/dev/null",
    "-c",
    "core.quotePath=true",
    "-c",
    "diff.external=",
    "-c",
    "diff.submodule=short",
    "-c",
    "log.showSignature=false",
    "-c",
    "credential.helper=",
    "-c",
    "protocol.allow=never",
    "-c",
    "gc.auto=0",
    "-c",
    "maintenance.auto=false",
];
const ENV: &[(&str, &str)] = &[
    ("LANG", "C"),
    ("LC_ALL", "C"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_SYSTEM", "/dev/null"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_ATTR_NOSYSTEM", "1"),
    ("GIT_OPTIONAL_LOCKS", "0"),
    ("GIT_NO_REPLACE_OBJECTS", "1"),
    ("GIT_NO_LAZY_FETCH", "1"),
    ("GIT_TERMINAL_PROMPT", "0"),
];

#[test]
fn isolated_command_matches_reference_prefix_cwd_and_environment() {
    let dir = tempfile::tempdir().unwrap();
    let command = inspect_command(dir.path());
    assert_eq!(command.get_program(), "git");
    assert_eq!(command.get_args().collect::<Vec<_>>(), PREFIX);
    assert_eq!(command.get_current_dir(), Some(dir.path()));
    let mut expected: BTreeMap<_, _> = ENV
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect();
    expected.insert("PATH".into(), std::env::var_os("PATH").unwrap_or_default());
    let actual: BTreeMap<_, _> = command
        .get_envs()
        .map(|(k, v)| (k.to_owned(), v.unwrap().to_owned()))
        .collect();
    assert_eq!(actual, expected);
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", utf8(&output.stderr));
    utf8(&output.stdout).trim().to_owned()
}

#[test]
fn hostile_inspection_commands_are_recorded_without_running_configured_programs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.name", "Test"]);
    git(&root, &["config", "user.email", "test@example.invalid"]);
    fs::write(root.join("a.txt"), "base\n").unwrap();
    git(&root, &["add", "a.txt"]);
    git(&root, &["commit", "-q", "-m", "Initial"]);
    let marker = dir.path().join("hostile-ran");
    let hostile = dir.path().join("hostile");
    fs::write(
        &hostile,
        format!("#!/bin/sh\nprintf x >> '{}'\ncat\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&hostile, fs::Permissions::from_mode(0o755)).unwrap();
    let hooks = dir.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    std::os::unix::fs::symlink(&hostile, hooks.join("post-index-change")).unwrap();
    for key in [
        "core.fsmonitor",
        "core.pager",
        "diff.external",
        "diff.hostile.textconv",
        "filter.hostile.clean",
        "filter.hostile.smudge",
        "credential.helper",
    ] {
        git(&root, &["config", key, hostile.to_str().unwrap()]);
    }
    git(
        &root,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    fs::write(
        root.join(".gitattributes"),
        "a.txt filter=hostile diff=hostile text eol=crlf ident\n",
    )
    .unwrap();
    fs::write(root.join("a.txt"), "changed\r\n").unwrap();
    git(&root, &["status", "--porcelain"]);
    assert!(
        marker.exists(),
        "hostile configuration positive control did not run"
    );
    fs::remove_file(&marker).unwrap();

    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let log = dir.path().join("commands.jsonl");
    let recorder = format!(
        r#"#!/usr/bin/python3
import json, os, sys
with open({}, 'a') as f:
    f.write(json.dumps({{'argv': sys.argv[1:], 'cwd': os.getcwd(), 'env': dict(os.environ)}}) + '\n')
os.execv('/usr/bin/git', ['git'] + sys.argv[1:])
"#,
        json!(log)
    );
    fs::write(bin.join("git"), recorder).unwrap();
    fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tool_git::command_tests::record_hostile_inspection_child",
            "--nocapture",
        ])
        .env("REAGENT_GIT_AUDIT_ROOT", &root)
        .env("PATH", &bin)
        .env("HOME", dir.path().join("hostile-home"))
        .env("GIT_DIR", "/unavailable/hostile-git")
        .env("GIT_WORK_TREE", "/unavailable/hostile-worktree")
        .env("GIT_INDEX_FILE", "/unavailable/hostile-index")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.fsmonitor")
        .env("GIT_CONFIG_VALUE_0", &hostile)
        .env("OPENAI_API_KEY", "test-secret-must-not-reach-git")
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}\n{}",
        utf8(&status.stdout),
        utf8(&status.stderr)
    );
    assert!(
        !marker.exists(),
        "inspection ran a hostile configured program"
    );
    // Apple's Python launcher adds its own environment even after env_clear().
    let baseline = Command::new("/usr/bin/python3")
        .args(["-c", "import json, os; print(json.dumps(dict(os.environ)))"])
        .env_clear()
        .envs(ENV.iter().copied())
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert!(baseline.status.success(), "{}", utf8(&baseline.stderr));
    let expected_env: Value = serde_json::from_slice(&baseline.stdout).unwrap();
    let records: Vec<Value> = fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for record in &records {
        let argv = record["argv"].as_array().unwrap();
        assert_eq!(&argv[..PREFIX.len()], json!(PREFIX).as_array().unwrap());
        assert_eq!(record["cwd"], json!(root));
        assert_eq!(record["env"], expected_env);
    }
    let oid = git(&root, &["rev-parse", "HEAD"]);
    let blob = git(&root, &["rev-parse", "HEAD:a.txt"]);
    let preflight = vec![
        strings(&["rev-parse", "--show-toplevel"]),
        strings(&[
            "config",
            "--get-regexp",
            r"^(remote\..*\.promisor|extensions\.partialclone)$",
        ]),
        strings(&["rev-parse", "--verify", "--end-of-options", "HEAD^{commit}"]),
    ];
    let scope = pathspecs(&["a.txt".into()]);
    let mut inventory = vec![
        strings(&["rev-parse", "--show-object-format"]),
        strings(&["ls-tree", "-r", "-z", &oid, "--", ":(literal)a.txt"]),
    ];
    for args in [
        vec!["ls-files", "--stage", "--debug", "-z", "--"],
        vec!["ls-files", "-z", "--others", "--exclude-standard", "--"],
    ] {
        let mut args = strings(&args);
        args.extend(scope.clone());
        inventory.push(args);
    }
    inventory.extend([
        strings(&["config", "--get", "core.autocrlf"]),
        strings(&["config", "--get", "core.eol"]),
        strings(&[
            "check-attr",
            "--stdin",
            "-z",
            "filter",
            "text",
            "eol",
            "crlf",
            "ident",
            "working-tree-encoding",
        ]),
    ]);
    let options = [
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
        "--no-color",
        "--ignore-submodules=none",
        "--submodule=short",
        "--relative",
    ];
    let mut expected = preflight.clone();
    expected.extend(inventory.clone());
    expected.extend(preflight.clone());
    expected.extend(inventory);
    expected.extend([
        strings(&["cat-file", "-s", &blob]),
        strings(&["cat-file", "blob", &blob]),
    ]);
    for staged in [true, false] {
        expected.extend(preflight.clone());
        if !staged {
            expected.push(preflight[2].clone());
        }
        let mut args = strings(&["diff"]);
        args.extend(strings(&options));
        if staged {
            args.push("--cached".into());
        }
        args.push(oid.clone());
        if !staged {
            args.push(oid.clone());
        }
        args.push("--".into());
        args.extend(scope.clone());
        expected.push(args);
    }
    expected.extend(preflight.clone());
    let mut show = strings(&[
        "show",
        "--format=fuller",
        "--stat",
        "--root",
        "--no-notes",
        "--no-show-signature",
        "--no-use-mailmap",
    ]);
    show.extend(strings(&options));
    show.extend([oid.clone(), "--".into()]);
    show.extend(pathspecs(&[]));
    expected.push(show);
    expected.extend(preflight.clone());
    expected.push(strings(&["cat-file", "blob", &format!("{oid}:./a.txt")]));
    expected.extend(preflight);
    let mut args = strings(&[
        "log",
        "--format=%H %s",
        "--no-notes",
        "--no-show-signature",
        "--max-count=20",
        &oid,
        "--",
    ]);
    args.extend(scope);
    expected.push(args);
    let actual: Vec<Vec<String>> = records
        .iter()
        .map(|r| {
            r["argv"].as_array().unwrap()[PREFIX.len()..]
                .iter()
                .map(|a| a.as_str().unwrap().to_owned())
                .collect()
        })
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn record_hostile_inspection_child() {
    let Some(root) = std::env::var_os("REAGENT_GIT_AUDIT_ROOT") else {
        return;
    };
    let mut workspace = Workspace::open(root.to_str().unwrap()).unwrap();
    for args in [
        json!({"operation":"status","paths":["a.txt"]}),
        json!({"operation":"worktree_diff","paths":["a.txt"]}),
        json!({"operation":"diff","staged":true,"paths":["a.txt"]}),
        json!({"operation":"diff","from":"HEAD","to":"HEAD","paths":["a.txt"]}),
        json!({"operation":"show"}),
        json!({"operation":"show","path":"a.txt"}),
        json!({"operation":"log","paths":["a.txt"]}),
    ] {
        let outcome = inspect(&mut workspace, &json!({}), &args).unwrap();
        assert_eq!(outcome["ok"], true, "{outcome}");
        if args["operation"] == "status" || args["operation"] == "worktree_diff" {
            let text = outcome["data"]["output"].as_str().unwrap();
            assert!(text.contains("comparison not filter-aware"), "{text}");
        }
    }
}

#[test]
fn git_payload_order_and_encoded_output_limit_match_reference() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    fs::write(dir.path().join("large.txt"), "x".repeat(40_000)).unwrap();
    git(dir.path(), &["add", "large.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "Initial"]);
    let mut workspace = Workspace::open(dir.path().to_str().unwrap()).unwrap();
    let result = inspect(
        &mut workspace,
        &json!({}),
        &json!({"operation":"show","path":"large.txt"}),
    )
    .unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["truncated"], true);
    assert_eq!(result["data"]["output_bytes_seen"], 40_000);
    assert_eq!(
        result["data"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "operation",
            "rev",
            "path",
            "output",
            "output_bytes_seen",
            "output_truncated",
            "encoding_replaced"
        ]
    );
    assert_eq!(crate::types::json_string(&result).len(), LIMIT - 1);
    let mut measured = result.clone();
    measured["truncated"] = json!(false);
    assert_eq!(crate::types::json_string(&measured).len(), LIMIT);
    let text = measured["data"]["output"].as_str().unwrap().to_owned() + "x";
    measured["data"]["output"] = json!(text);
    assert_eq!(crate::types::json_string(&measured).len(), LIMIT + 1);

    git(dir.path(), &["config", "branch.main.remote", "."]);
    git(
        dir.path(),
        &["config", "branch.main.merge", "refs/heads/main"],
    );
    let declined = git_do(
        &mut workspace,
        &json!({"git_do":"model"}),
        &json!({"recipe":"amend"}),
    )
    .unwrap();
    assert_eq!(declined["code"], "declined");
    assert_eq!(
        declined["data"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "recipe",
            "chosen_by",
            "status",
            "reason",
            "steps",
            "decision"
        ]
    );
    assert_eq!(
        declined["data"]["decision"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["offered", "chosen_by", "declined_by", "snapshot"]
    );
    let snapshot = &declined["data"]["decision"]["snapshot"];
    assert_eq!(
        snapshot
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        [
            "branch",
            "default_branch",
            "upstream",
            "ahead_of_upstream",
            "behind_default",
            "ahead_of_default",
            "changed",
            "recent_commits"
        ]
    );
    assert!(declined["message"].as_str().unwrap().contains(
        r#"state: {"branch":"main","default_branch":"main","upstream":"main","ahead_of_upstream":0"#
    ));
}

#[test]
fn recipe_entry_without_steps_is_null_but_selection_refusal_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    git(
        dir.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    fs::write(dir.path().join("a.txt"), "base\n").unwrap();
    git(dir.path(), &["add", "a.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "Initial"]);
    let mut workspace = Workspace::open(dir.path().to_str().unwrap()).unwrap();
    let config = json!({"git_do":"model"});
    let selection = git_do(
        &mut workspace,
        &config,
        &json!({"recipe":"status","paths":["a.txt"]}),
    )
    .unwrap();
    assert_eq!(selection["code"], "declined");
    assert_eq!(selection["data"]["steps"], json!([]));
    let recipe = git_do(
        &mut workspace,
        &config,
        &json!({"recipe":"new_branch","branch":"invalid branch"}),
    )
    .unwrap();
    assert_eq!(recipe["code"], "declined");
    assert!(recipe["data"]["steps"].is_null(), "{recipe}");
    assert_eq!(recipe["effect"], "none");
    fs::write(dir.path().join("a.txt"), "changed\n").unwrap();
    let committed = git_do(
        &mut workspace,
        &config,
        &json!({"recipe":"commit","message":"Update"}),
    )
    .unwrap();
    assert_eq!(committed["ok"], true, "{committed}");
    assert!(!committed["data"]["steps"].as_array().unwrap().is_empty());
}
