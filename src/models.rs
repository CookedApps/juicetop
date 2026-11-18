//! Local model servers: finds llama.cpp, vLLM, SGLang and Text Generation Inference servers on
//! this machine, reads their Prometheus token counters, and reports tokens per minute so the
//! dashboard can show energy and cost per token.
//!
//! Discovery looks only at listening sockets of processes whose name looks like a model server,
//! and only talks to them over the loopback interface.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::live::ModelLive;
use crate::procs;
use crate::store::TokenMinute;

/// Substrings of process names that are worth probing.
const SERVER_NAMES: &[&str] = &[
    "llama-server",
    "llama.cpp",
    "vllm",
    "sglang",
    "text-generation",
    "lmdeploy",
    "koboldcpp",
    "tabby",
    "local-ai",
    "strata",
];

/// (prompt counter, generated counter) per server family.
const COUNTERS: &[(&str, &str)] = &[
    (
        "llamacpp:prompt_tokens_total",
        "llamacpp:tokens_predicted_total",
    ),
    ("vllm:prompt_tokens_total", "vllm:generation_tokens_total"),
    (
        "sglang:prompt_tokens_total",
        "sglang:generation_tokens_total",
    ),
    (
        "tgi_request_input_length_sum",
        "tgi_request_generated_tokens_sum",
    ),
];

const SCRAPE_EVERY: Duration = Duration::from_secs(5);
const DISCOVER_EVERY: Duration = Duration::from_secs(30);

/// Token counters read from one `/metrics` page.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Counters {
    pub prompt: f64,
    pub generated: f64,
}

/// Parses Prometheus text format and sums the known token counters over all label sets.
/// Returns `None` when the page has none of them. Also returns a `model_name` label if present.
pub fn parse_metrics(text: &str) -> Option<(Counters, Option<String>)> {
    // Some servers publish several families for the same tokens (Strata exports both the vLLM
    // and the llama.cpp names), so read one family: the first in `COUNTERS` that is present.
    let mut families: Vec<Option<Counters>> = vec![None; COUNTERS.len()];
    let mut model = None;
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let Some((name_labels, value)) = line.rsplit_once(' ') else {
            continue;
        };
        let name = name_labels.split('{').next().unwrap_or(name_labels);
        let Ok(v) = value.trim().parse::<f64>() else {
            continue;
        };
        for (i, (p, g)) in COUNTERS.iter().enumerate() {
            if name != *p && name != *g {
                continue;
            }
            let c = families[i].get_or_insert_with(Counters::default);
            if name == *p {
                c.prompt += v;
            } else {
                c.generated += v;
            }
            if model.is_none() {
                model = label(name_labels, "model_name");
            }
        }
    }
    families.into_iter().flatten().next().map(|c| (c, model))
}

fn label(name_labels: &str, key: &str) -> Option<String> {
    let start = name_labels.find(&format!("{key}=\""))? + key.len() + 2;
    let rest = &name_labels[start..];
    Some(rest[..rest.find('"')?].to_string())
}

/// Listening TCP ports with their socket inode, from /proc/net/tcp and tcp6.
pub fn listening_ports(proc_root: &Path) -> HashMap<u64, u16> {
    let mut out = HashMap::new();
    for f in ["net/tcp", "net/tcp6"] {
        let Ok(text) = fs::read_to_string(proc_root.join(f)) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            // local_address is column 1 ("ADDR:PORT" in hex), state column 3, inode column 9.
            if cols.len() < 10 || cols[3] != "0A" {
                continue;
            }
            let Some(port) = cols[1]
                .rsplit(':')
                .next()
                .and_then(|p| u16::from_str_radix(p, 16).ok())
            else {
                continue;
            };
            if let Ok(inode) = cols[9].parse::<u64>() {
                out.insert(inode, port);
            }
        }
    }
    out
}

/// Ports a process listens on.
fn ports_of(pid_dir: &Path, listening: &HashMap<u64, u16>) -> Vec<u16> {
    let mut ports = Vec::new();
    let Ok(fds) = fs::read_dir(pid_dir.join("fd")) else {
        return ports;
    };
    for fd in fds.flatten() {
        let Ok(target) = fs::read_link(fd.path()) else {
            continue;
        };
        let t = target.to_string_lossy();
        if let Some(inode) = t
            .strip_prefix("socket:[")
            .and_then(|s| s.strip_suffix(']'))
            .and_then(|s| s.parse().ok())
            && let Some(port) = listening.get(&inode)
        {
            ports.push(*port);
        }
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[derive(Debug, Clone)]
struct Endpoint {
    process: String,
    model: String,
    last: Option<(Instant, Counters)>,
    /// Tokens since the last minute was taken.
    pending: Counters,
    rate: Counters,
}

#[derive(Default)]
struct State {
    endpoints: BTreeMap<String, Endpoint>,
}

/// Watches model servers on a background thread.
#[derive(Clone)]
pub struct ModelWatcher {
    state: Arc<Mutex<State>>,
}

impl ModelWatcher {
    /// Starts the background thread. `extra` are user-given metrics URLs.
    pub fn start(extra: Vec<String>) -> Self {
        let watcher = Self {
            state: Arc::new(Mutex::new(State::default())),
        };
        let w = watcher.clone();
        std::thread::Builder::new()
            .name("juicetop-models".into())
            .spawn(move || w.run(PathBuf::from("/proc"), extra))
            .ok();
        watcher
    }

    fn run(&self, proc_root: PathBuf, extra: Vec<String>) {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_millis(800)))
            .build()
            .into();
        let mut targets: BTreeMap<String, String> = BTreeMap::new(); // endpoint -> process
        let mut last_discovery: Option<Instant> = None;
        loop {
            if last_discovery.is_none_or(|t| t.elapsed() >= DISCOVER_EVERY) {
                targets = discover(&proc_root);
                for url in &extra {
                    targets
                        .entry(endpoint_of(url))
                        .or_insert_with(|| "(remote)".into());
                }
                last_discovery = Some(Instant::now());
            }
            for (endpoint, process) in &targets {
                let url = if endpoint.starts_with("http") {
                    endpoint.clone()
                } else {
                    format!("http://{endpoint}/metrics")
                };
                let Ok(mut resp) = agent.get(&url).call() else {
                    continue;
                };
                let Ok(body) = resp.body_mut().read_to_string() else {
                    continue;
                };
                let Some((counters, label_model)) = parse_metrics(&body) else {
                    continue;
                };
                let model = label_model
                    .or_else(|| model_name(&agent, endpoint))
                    .unwrap_or_default();
                self.record(endpoint, process, &model, counters, Instant::now());
            }
            // Forget servers that went away.
            if let Ok(mut s) = self.state.lock() {
                s.endpoints.retain(|e, _| targets.contains_key(e));
            }
            std::thread::sleep(SCRAPE_EVERY);
        }
    }

    fn record(&self, endpoint: &str, process: &str, model: &str, c: Counters, at: Instant) {
        let Ok(mut s) = self.state.lock() else { return };
        let e = s
            .endpoints
            .entry(endpoint.to_string())
            .or_insert_with(|| Endpoint {
                process: process.to_string(),
                model: model.to_string(),
                last: None,
                pending: Counters::default(),
                rate: Counters::default(),
            });
        if !model.is_empty() {
            e.model = model.to_string();
        }
        if let Some((t, prev)) = e.last {
            // Counters reset when the server restarts.
            let d = |now: f64, before: f64| if now >= before { now - before } else { now };
            let delta = Counters {
                prompt: d(c.prompt, prev.prompt),
                generated: d(c.generated, prev.generated),
            };
            let secs = at.duration_since(t).as_secs_f64().max(0.001);
            e.pending.prompt += delta.prompt;
            e.pending.generated += delta.generated;
            e.rate = Counters {
                prompt: delta.prompt / secs,
                generated: delta.generated / secs,
            };
        }
        e.last = Some((at, c));
    }

    /// Live token rates per server.
    pub fn live(&self) -> Vec<ModelLive> {
        let Ok(s) = self.state.lock() else {
            return Vec::new();
        };
        s.endpoints
            .iter()
            .map(|(ep, e)| ModelLive {
                endpoint: ep.clone(),
                process: e.process.clone(),
                model: e.model.clone(),
                gen_tps: e.rate.generated,
                prompt_tps: e.rate.prompt,
            })
            .collect()
    }

    /// Tokens since the last call, per server, for the minute being written.
    pub fn take_minute(&self) -> Vec<TokenMinute> {
        let Ok(mut s) = self.state.lock() else {
            return Vec::new();
        };
        s.endpoints
            .iter_mut()
            .map(|(ep, e)| {
                let p = std::mem::take(&mut e.pending);
                TokenMinute {
                    endpoint: ep.clone(),
                    process: e.process.clone(),
                    model: e.model.clone(),
                    prompt_tokens: p.prompt.round() as u64,
                    gen_tokens: p.generated.round() as u64,
                }
            })
            .collect()
    }
}

/// "http://127.0.0.1:8000/metrics" becomes "127.0.0.1:8000".
fn endpoint_of(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split('/').next().unwrap_or(rest).to_string()
}

/// Model servers listening on this machine: "127.0.0.1:port" -> process name.
fn discover(proc_root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let listening = listening_ports(proc_root);
    if listening.is_empty() {
        return out;
    }
    let Ok(entries) = fs::read_dir(proc_root) else {
        return out;
    };
    for e in entries.flatten() {
        if !e
            .file_name()
            .to_string_lossy()
            .chars()
            .all(|c| c.is_ascii_digit())
        {
            continue;
        }
        let dir = e.path();
        let cmdline = fs::read(dir.join("cmdline")).unwrap_or_default();
        let joined = String::from_utf8_lossy(&cmdline).to_lowercase();
        if !SERVER_NAMES.iter().any(|n| joined.contains(n)) {
            continue;
        }
        let ports = ports_of(&dir, &listening);
        if ports.is_empty() {
            continue;
        }
        // The server and its children: some servers run the model in a child process.
        let pid: u32 = e.file_name().to_string_lossy().parse().unwrap_or(0);
        let names = family_names(proc_root, pid);
        for port in ports {
            out.insert(format!("127.0.0.1:{port}"), names.clone());
        }
    }
    out
}

/// Names of a process and all its descendants, comma separated, as the process list names them.
fn family_names(proc_root: &Path, root: u32) -> String {
    let mut all: Vec<(u32, u32, String)> = Vec::new();
    if let Ok(entries) = fs::read_dir(proc_root) {
        for e in entries.flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Ok(stat) = fs::read_to_string(e.path().join("stat")) else {
                continue;
            };
            let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
                continue;
            };
            let ppid = stat[close + 1..]
                .split_whitespace()
                .nth(1)
                .and_then(|p| p.parse().ok())
                .unwrap_or(0);
            let cmdline = fs::read(e.path().join("cmdline")).unwrap_or_default();
            all.push((
                pid,
                ppid,
                procs::name_from(&stat[open + 1..close], &cmdline),
            ));
        }
    }
    let mut family = vec![root];
    let mut i = 0;
    while i < family.len() {
        let p = family[i];
        family.extend(
            all.iter()
                .filter(|(_, pp, _)| *pp == p)
                .map(|(pid, _, _)| *pid),
        );
        i += 1;
    }
    let mut names: Vec<String> = all
        .iter()
        .filter(|(pid, _, _)| family.contains(pid))
        .map(|(_, _, n)| n.clone())
        .collect();
    names.sort();
    names.dedup();
    names.join(",")
}

/// The first model id from the OpenAI-compatible /v1/models endpoint.
fn model_name(agent: &ureq::Agent, endpoint: &str) -> Option<String> {
    if endpoint.starts_with("http") {
        return None;
    }
    let mut resp = agent
        .get(&format!("http://{endpoint}/v1/models"))
        .call()
        .ok()?;
    let v: serde_json::Value = resp.body_mut().read_json().ok()?;
    let id = v.get("data")?.get(0)?.get("id")?.as_str()?;
    Some(
        id.rsplit('/')
            .next()
            .unwrap_or(id)
            .trim_end_matches(".gguf")
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vllm_and_llamacpp_metrics() {
        let vllm = r#"# HELP vllm:generation_tokens_total Number of generation tokens processed.
# TYPE vllm:generation_tokens_total counter
vllm:generation_tokens_total{model_name="meta-llama/Llama-3.3-70B-Instruct"} 12345.0
vllm:prompt_tokens_total{model_name="meta-llama/Llama-3.3-70B-Instruct"} 500.0
vllm:num_requests_running{model_name="x"} 3.0
"#;
        let (c, model) = parse_metrics(vllm).unwrap();
        assert_eq!(
            c,
            Counters {
                prompt: 500.0,
                generated: 12345.0
            }
        );
        assert_eq!(model.as_deref(), Some("meta-llama/Llama-3.3-70B-Instruct"));

        let llama = "llamacpp:prompt_tokens_total 10\nllamacpp:tokens_predicted_total 90\n";
        assert_eq!(
            parse_metrics(llama).unwrap().0,
            Counters {
                prompt: 10.0,
                generated: 90.0
            }
        );
        assert!(parse_metrics("process_cpu_seconds_total 1.5\n").is_none());
        // Strata publishes both families; the tokens must not count twice.
        let both = "vllm:generation_tokens_total 50\nllamacpp:tokens_predicted_total 50\nllamacpp:prompt_tokens_total 7\n";
        assert_eq!(
            parse_metrics(both).unwrap().0,
            Counters {
                prompt: 7.0,
                generated: 50.0
            }
        );
    }

    #[test]
    fn family_includes_child_processes() {
        let tmp = tempfile::tempdir().unwrap();
        for (pid, ppid, comm, cmd) in [
            (10, 1, "python3", &b"python3\0-m\0serve\0"[..]),
            (
                11,
                10,
                "strata",
                b"/opt/strata/engine/strata\0--port\x009000\0",
            ),
            (12, 1, "bash", b"bash\0"),
        ] {
            let d = tmp.path().join(pid.to_string());
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("stat"), format!("{pid} ({comm}) S {ppid} 0 0")).unwrap();
            fs::write(d.join("cmdline"), cmd).unwrap();
        }
        assert_eq!(family_names(tmp.path(), 10), "serve,strata");
    }

    #[test]
    fn finds_listening_ports() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("net")).unwrap();
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242 1 0
   1: 0100007F:1F91 0100007F:9C40 01 00000000:00000000 00:00000000 00000000  1000        0 4243 1 0
";
        fs::write(tmp.path().join("net/tcp"), tcp).unwrap();
        let ports = listening_ports(tmp.path());
        assert_eq!(ports.get(&4242), Some(&8080));
        assert_eq!(ports.len(), 1, "only LISTEN sockets");
    }

    #[test]
    fn rates_and_minutes_from_counter_deltas() {
        let w = ModelWatcher {
            state: Arc::new(Mutex::new(State::default())),
        };
        let t0 = Instant::now();
        w.record(
            "127.0.0.1:8000",
            "vllm",
            "m",
            Counters {
                prompt: 100.0,
                generated: 1000.0,
            },
            t0,
        );
        w.record(
            "127.0.0.1:8000",
            "vllm",
            "m",
            Counters {
                prompt: 150.0,
                generated: 1500.0,
            },
            t0 + Duration::from_secs(5),
        );
        assert_eq!(w.live()[0].gen_tps, 100.0);
        // A restart resets the counters; the new value counts as the delta.
        w.record(
            "127.0.0.1:8000",
            "vllm",
            "m",
            Counters {
                prompt: 0.0,
                generated: 20.0,
            },
            t0 + Duration::from_secs(10),
        );
        let m = w.take_minute();
        assert_eq!((m[0].prompt_tokens, m[0].gen_tokens), (50, 520));
        assert_eq!(w.take_minute()[0].gen_tokens, 0);
        assert_eq!(endpoint_of("http://10.0.0.5:8000/metrics"), "10.0.0.5:8000");
    }
}
