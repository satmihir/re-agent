use crate::{
    tool_exec::{self, ExecTail},
    tools::{fail, outcome},
    workspace::Workspace,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
};

const MAX_ACTIVE: usize = 4;
const MAX_RECORDS: usize = 16;

/// Jobs owns one Session's workers, their undelivered outcomes, and recent delivered records.
#[derive(Default)]
pub(crate) struct Jobs {
    entries: Mutex<VecDeque<Job>>,
}

struct Job {
    id: String,
    root: PathBuf,
    cancel: Arc<AtomicBool>,
    live: Arc<Mutex<ExecTail>>,
    worker: Option<JoinHandle<Value>>,
    result: Option<Value>,
    delivered: bool,
}

impl Job {
    fn poll(&mut self) {
        if !self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            return;
        }
        let mut result = self.worker.take().unwrap().join().unwrap_or_else(|_| {
            outcome(
                false,
                "job_error",
                "background exec worker panicked",
                Value::Null,
                "unknown",
            )
        });
        result["workspace"] = json!(self.root.to_string_lossy());
        result["job_id"] = json!(self.id);
        self.result = Some(result);
    }

    fn summary(&self) -> Value {
        let state = match &self.result {
            Some(r) if r["ok"] == true => "completed",
            Some(r) if r["code"] == "cancelled" => "cancelled",
            Some(_) => "failed",
            None if self.cancel.load(Ordering::Acquire) => "cancelling",
            None => "running",
        };
        json!({"id":self.id,"workspace":self.root.to_string_lossy(),"state":state})
    }
}

// Large argv or skipped commands can exhaust the exec budget before its stream tails fit.
fn compact_completion(result: &mut Value, mut tail: Vec<String>) {
    fn retain_status(step: &mut Value) {
        if let Some(map) = step.as_object_mut() {
            map.retain(|key, _| {
                matches!(
                    key.as_str(),
                    "exit_code"
                        | "signal"
                        | "termination_reason"
                        | "cleanup_error"
                        | "stdout_bytes_seen"
                        | "stderr_bytes_seen"
                        | "output_may_be_incomplete"
                        | "duration_ms"
                        | "timeout_ms"
                )
            });
        }
    }
    let data = &mut result["data"];
    if let Some(steps) = data["steps"].as_array_mut() {
        for step in steps {
            retain_status(step);
        }
        if let Some(not_run) = data.as_object_mut().unwrap().remove("not_run") {
            data["not_run_count"] = json!(not_run.as_array().map_or(0, Vec::len));
        }
    } else {
        retain_status(data);
    }
    result["message"] = json!("exec details truncated; tail contains the most recent output lines");
    result["truncated"] = json!(true);
    // A byte may expand to six JSON bytes (e.g. NUL); leave room for array delimiters and job_id.
    let room = tool_exec::LIMIT.saturating_sub(crate::types::json_string(result).len() + 128);
    let bytes_per_line = room / (6 * tail.len().max(1));
    for line in &mut tail {
        let start = line.ceil_char_boundary(line.len().saturating_sub(bytes_per_line));
        line.drain(..start);
    }
    result["data"]["tail"] = json!(tail);
}

impl Jobs {
    /// Starts a worker for validated foreground exec arguments (without the background selector).
    pub(crate) fn start(
        &mut self,
        workspace: &mut Workspace,
        config: &Value,
        args: &Value,
    ) -> Result<Value, String> {
        let prepared = match tool_exec::prepare(workspace, args, 3_600_000) {
            Ok(prepared) => prepared,
            Err(result) => return Ok(result),
        };
        if crate::cancellation::cancelled(config) {
            return Ok(fail(
                "cancelled",
                "the Run was cancelled before the job started",
            ));
        }
        let entries = self.entries.get_mut().unwrap();
        for job in &mut *entries {
            job.poll();
        }
        if entries.iter().filter(|j| j.worker.is_some()).count() >= MAX_ACTIVE {
            return Ok(fail(
                "job_limit",
                "this Session already has four active exec jobs",
            ));
        }
        if entries.len() >= MAX_RECORDS {
            // Never silently lose an outcome that the Session hasn't observed yet.
            let Some(index) = entries.iter().position(|j| j.delivered) else {
                return Ok(fail(
                    "job_limit",
                    "deliver completed exec jobs before starting more",
                ));
            };
            entries.remove(index);
        }
        let id = format!("job-{}", crate::types::new_id());
        let cancel = Arc::new(AtomicBool::new(false));
        let live = Arc::new(Mutex::new(ExecTail::default()));
        let flag = cancel.clone();
        let tail = live.clone();
        // Do not copy the entire Session config (which may include private data) into a worker.
        let exec_config = json!({
            "session_id":config["session_id"],
            "agent_tree_id":config["agent_tree_id"], "agent_id":config["agent_id"]
        });
        let worker = thread::Builder::new()
            .name(id.clone())
            .spawn(move || {
                let mut result = prepared
                    .execute(&exec_config, &flag, None, Some(&tail))
                    .unwrap_or_else(|error| {
                        outcome(false, "job_error", &error, Value::Null, "unknown")
                    });
                // Reserve room for job_id, added when the worker is joined.
                if crate::types::json_string(&result).len() + 64 > tool_exec::LIMIT {
                    compact_completion(&mut result, tail.lock().unwrap().lines());
                }
                result
            })
            .map_err(|e| format!("start background exec worker: {}", tool_exec::stable(&e)))?;
        let job = Job {
            id,
            root: workspace.root.clone(),
            cancel,
            live,
            worker: Some(worker),
            result: None,
            delivered: false,
        };
        let started = tool_exec::attributed(
            outcome(
                true,
                "ok",
                "background exec started",
                job.summary(),
                "applied",
            ),
            &job.root,
        );
        entries.push_back(job);
        Ok(started)
    }

    /// Lists jobs or inspects or requests cancellation of one job in this Session.
    pub(crate) fn inspect(&self, args: &Value) -> Value {
        if let Err(error) = tool_exec::keys(args, &["operation", "id"]) {
            return error;
        }
        let mut entries = self.entries.lock().unwrap();
        match args["operation"].as_str() {
            Some("list") => {
                if args.get("id").is_some() {
                    return fail("invalid_arguments", "list does not take an id");
                }
                let jobs: Vec<_> = entries
                    .iter_mut()
                    .map(|job| {
                        job.poll();
                        job.summary()
                    })
                    .collect();
                outcome(true, "ok", "", json!({"jobs":jobs}), "none")
            }
            Some(op @ ("status" | "cancel")) => {
                let Some(id) = args["id"].as_str().filter(|s| !s.is_empty()) else {
                    return fail("invalid_arguments", "status and cancel require a job id");
                };
                let Some(job) = entries.iter_mut().find(|j| j.id == id) else {
                    return fail("job_not_found", "no such job in this Session");
                };
                job.poll();
                let mut effect = "none";
                if op == "cancel"
                    && job.worker.is_some()
                    && !job.cancel.swap(true, Ordering::AcqRel)
                {
                    effect = "applied";
                }
                let mut data = job.summary();
                if let Some(result) = &job.result {
                    data["outcome"] = result.clone();
                } else {
                    data["tail"] = json!(job.live.lock().unwrap().lines());
                }
                tool_exec::attributed(outcome(true, "ok", "", data, effect), &job.root)
            }
            _ => fail(
                "invalid_arguments",
                "operation must be list, status, or cancel",
            ),
        }
    }

    /// Returns newly finished exec outcomes with their job_id, without consuming status history.
    pub(crate) fn take_completed(&mut self) -> Vec<Value> {
        let mut completed = Vec::new();
        for job in self.entries.get_mut().unwrap() {
            job.poll();
            if !job.delivered {
                if let Some(result) = &job.result {
                    completed.push(result.clone());
                    job.delivered = true;
                }
            }
        }
        completed
    }

    /// Returns only the identities of running and finished-but-undelivered jobs for checkpoints.
    pub(crate) fn outstanding(&self) -> Vec<Value> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter(|job| !job.delivered)
            .map(|job| json!({"id":job.id,"workspace":job.root.to_string_lossy()}))
            .collect()
    }

    /// Reports whether a completion is ready for delivery, without consuming it.
    pub(crate) fn pending(&self) -> bool {
        self.entries.lock().unwrap().iter().any(|job| {
            !job.delivered
                && (job.result.is_some()
                    || job.worker.as_ref().is_some_and(JoinHandle::is_finished))
        })
    }
}

impl Drop for Jobs {
    fn drop(&mut self) {
        let jobs = self.entries.get_mut().unwrap_or_else(|e| e.into_inner());
        // Signal all workers before waiting for any one process tree's cleanup.
        for job in jobs.iter() {
            job.cancel.store(true, Ordering::Release);
        }
        for job in jobs {
            if let Some(worker) = job.worker.take() {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{Duration, Instant},
    };

    fn config() -> Value {
        json!({"session_id":crate::types::new_id()})
    }

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let start = Instant::now();
        while !condition() {
            assert!(
                start.elapsed() < Duration::from_secs(8),
                "job did not reach expected state"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn start(jobs: &mut Jobs, ws: &mut Workspace, config: &Value, args: Value) -> String {
        let r = jobs.start(ws, config, &args).unwrap();
        assert_eq!(r["ok"], true, "{r}");
        r["data"]["id"].as_str().unwrap().to_owned()
    }

    fn status(jobs: &Jobs, id: &str) -> Value {
        jobs.inspect(&json!({"operation":"status","id":id}))["data"].clone()
    }

    #[test]
    fn malformed_jobs_are_rejected_before_admission() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let config = config();
        let mut jobs = Jobs::default();
        for (args, code) in [
            (json!({"cwd":".","argv":[]}), "invalid_arguments"),
            (json!({"cwd":".","argv":["echo","\0"]}), "invalid_arguments"),
            (
                json!({"cwd":".","argv":["echo"],"then":[[]]}),
                "invalid_arguments",
            ),
            (json!({"cwd":"../outside","argv":["true"]}), "invalid_path"),
            (json!({"cwd":"absent","argv":["true"]}), "not_found"),
        ] {
            assert_eq!(jobs.start(&mut ws, &config, &args).unwrap()["code"], code);
        }
        for args in [
            json!({}),
            json!({"operation":"list","id":"x"}),
            json!({"operation":"cancel"}),
            json!({"operation":"status","id":45}),
        ] {
            assert_eq!(jobs.inspect(&args)["code"], "invalid_arguments");
        }
        assert!(jobs.inspect(&json!({"operation":"list"}))["data"]["jobs"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(!jobs.pending());
    }

    #[test]
    fn completion_reports_stopped_chain_once_and_remains_inspectable() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let mut jobs = Jobs::default();
        let id = start(
            &mut jobs,
            &mut ws,
            &config(),
            json!({"cwd":".","argv":["/bin/sh","-c","printf first"],"then":[["/bin/sh","-c","printf 'failed' >&2; exit 7"],["touch","forbidden"]]}),
        );
        wait_for(|| status(&jobs, &id)["state"] == "failed");
        assert!(jobs.pending());
        let result = status(&jobs, &id)["outcome"].clone();
        assert_eq!(result["job_id"], id);
        assert_eq!(result["workspace"], ws.root.to_string_lossy().as_ref());
        assert_eq!(result["effect"], "applied");
        assert_eq!(result["data"]["steps"][0]["timeout_ms"], 3_600_000);
        assert_eq!(result["data"]["steps"][0]["stdout"], "first");
        assert_eq!(result["data"]["steps"][1]["stderr"], "failed");
        assert_eq!(result["data"]["steps"][1]["exit_code"], 7);
        assert_eq!(result["data"]["not_run"], json!([["touch", "forbidden"]]));
        assert!(!dir.path().join("forbidden").exists());
        assert_eq!(jobs.take_completed(), vec![result.clone()]);
        assert_eq!(status(&jobs, &id)["outcome"], result);
        assert!(jobs.take_completed().is_empty());
        assert!(!jobs.pending());
    }

    #[test]
    fn live_tail_and_cancellation_are_job_local() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let config = config();
        let mut jobs = Jobs::default();
        let first = start(
            &mut jobs,
            &mut ws,
            &config,
            json!({"cwd":".","argv":["/bin/sh","-c","printf 'old\\n1\\n2\\n3\\n4\\nready\\n'; exec sleep 60"]}),
        );
        let other = start(
            &mut jobs,
            &mut ws,
            &config,
            json!({"cwd":".","argv":["/bin/sh","-c","while [ ! -f release ]; do sleep 0.05; done; printf finished"],"timeout_ms":45_000}),
        );
        wait_for(|| status(&jobs, &first)["tail"] == json!(["1", "2", "3", "4", "ready"]));
        assert!(
            !jobs.pending(),
            "a running job must not wake a follow-up Run"
        );
        let checkpoint = vec![
            json!({"id":first,"workspace":ws.root}),
            json!({"id":other,"workspace":ws.root}),
        ];
        assert_eq!(jobs.outstanding(), checkpoint);
        let cancel = json!({"operation":"cancel","id":first});
        assert_eq!(Jobs::default().inspect(&cancel)["code"], "job_not_found");
        let cancelling = jobs.inspect(&cancel);
        assert_eq!(cancelling["data"]["state"], "cancelling");
        assert!(!crate::cancellation::cancelled(&config));
        crate::cancellation::cancel(&config);
        fs::write(dir.path().join("release"), "").unwrap();
        wait_for(|| {
            status(&jobs, &other)["state"] == "completed"
                && status(&jobs, &first)["state"] == "cancelled"
        });
        assert_eq!(jobs.outstanding(), checkpoint);
        let results = jobs.take_completed();
        assert!(jobs.outstanding().is_empty());
        assert_eq!(results[0]["effect"], "unknown");
        assert_eq!(results[1]["data"]["stdout"], "finished");
        assert_eq!(results[1]["data"]["timeout_ms"], 45_000);
        assert_eq!(jobs.inspect(&cancel)["effect"], "none");
    }

    #[test]
    fn drop_cancels_and_reaps_active_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let config = config();
        let mut jobs = Jobs::default();
        for i in 0..MAX_ACTIVE {
            start(
                &mut jobs,
                &mut ws,
                &config,
                json!({"cwd":".","argv":["/bin/sh","-c",format!("echo $$ > pid-{i}; exec sleep 60")]}),
            );
        }
        let full = jobs
            .start(
                &mut ws,
                &config,
                &json!({"cwd":".","argv":["touch","forbidden"]}),
            )
            .unwrap();
        assert_eq!(full["code"], "job_limit");
        let pids: Vec<_> = (0..MAX_ACTIVE)
            .map(|i| {
                let path = dir.path().join(format!("pid-{i}"));
                let mut pid = None;
                wait_for(|| {
                    pid = fs::read_to_string(&path)
                        .ok()
                        .and_then(|s| s.trim().parse::<i32>().ok());
                    pid.is_some()
                });
                pid.unwrap()
            })
            .collect();
        drop(jobs);
        for pid in pids {
            assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "job {pid} still running");
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
        assert!(!dir.path().join("forbidden").exists());
    }

    #[test]
    fn oversized_job_metadata_does_not_hide_the_exit_status() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let mut jobs = Jobs::default();
        let id = start(
            &mut jobs,
            &mut ws,
            &config(),
            json!({
                "cwd":".", "argv":["/bin/sh", "-c", "printf 'last failure\\n' >&2; exit 3", "x".repeat(tool_exec::LIMIT + 1)]
            }),
        );
        wait_for(|| jobs.pending());
        let result = &jobs.take_completed()[0];
        assert_eq!(result["truncated"], true);
        assert_eq!(result["data"]["exit_code"], 3);
        assert_eq!(result["data"]["tail"], json!(["last failure"]));
        assert_eq!(result["job_id"], id);
        assert!(crate::types::json_string(result).len() <= tool_exec::LIMIT);
    }

    #[test]
    fn oversized_chain_keeps_step_status_and_final_output_in_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let mut jobs = Jobs::default();
        let id = start(
            &mut jobs,
            &mut ws,
            &config(),
            json!({
                "cwd":".", "argv":["true"], "then":[
                    ["/bin/sh", "-c", "i=0; while [ $i -lt 10000 ]; do echo noise; i=$((i+1)); done; echo 'last failure'; exit 1"],
                    ["echo", "x".repeat(tool_exec::LIMIT + 1)]
                ]
            }),
        );
        wait_for(|| jobs.pending());
        let result = &jobs.take_completed()[0];
        assert_eq!(result["truncated"], true);
        let data = &result["data"];
        assert_eq!(data["steps"][0]["exit_code"], 0);
        assert_eq!(data["steps"][1]["exit_code"], 1);
        assert_eq!(data["not_run_count"], 1);
        assert!(data["tail"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s == "last failure"));
        assert!(crate::types::json_string(result).len() <= tool_exec::LIMIT);
        assert_eq!(status(&jobs, &id)["outcome"], *result);
    }

    #[test]
    fn admission_evicts_only_delivered_terminal_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let mut ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        let config = config();
        let mut jobs = Jobs::default();
        let args = json!({"cwd":".","argv":["true"]});
        let mut first = None;
        for _ in 0..MAX_RECORDS {
            let id = start(&mut jobs, &mut ws, &config, args.clone());
            wait_for(|| status(&jobs, &id)["state"] == "completed");
            first.get_or_insert(id);
        }
        assert_eq!(
            jobs.start(&mut ws, &config, &args).unwrap()["code"],
            "job_limit"
        );
        assert_eq!(jobs.take_completed().len(), MAX_RECORDS);
        start(&mut jobs, &mut ws, &config, args);
        assert_eq!(
            jobs.inspect(&json!({"operation":"status","id":first}))["code"],
            "job_not_found"
        );
        assert_eq!(
            jobs.inspect(&json!({"operation":"list"}))["data"]["jobs"]
                .as_array()
                .unwrap()
                .len(),
            MAX_RECORDS
        );
    }
}
