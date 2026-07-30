//! Workflow graphs — a general DAG of agent nodes.
//!
//! `graph.rs` implements one fixed shape (worker → judge panel → revise).
//! This module generalises that: an arbitrary directed acyclic graph of nodes,
//! where each node is an agent call whose prompt can interpolate the outputs of
//! its parents. Nodes with no unmet dependencies run concurrently.
//!
//! Six named patterns are built on top of it, because each is just a topology:
//!
//! | Pattern               | Topology                                          |
//! |-----------------------|---------------------------------------------------|
//! | Classify-And-Act      | classifier → one of N branches                     |
//! | Fanout-And-Synthesize | N workers in parallel → synthesizer                |
//! | Adversarial Verify    | worker → N verifiers → verdict                     |
//! | Generate-And-Filter   | N generators → filter (rubric + dedupe)            |
//! | Tournament            | attempts → pairwise judges → final                 |
//! | Loop Until Done       | the graph re-run until a node reports nothing new  |
//!
//! Execution is deliberately parameterised over a node-runner callback rather
//! than calling the model directly, so the scheduling logic here is unit
//! testable without a network or a model.

use std::collections::{HashMap, HashSet, VecDeque};

use futures_util::stream::{FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};

/// Hard ceiling on loop-until-done rounds, whatever a profile asks for.
const MAX_ROUNDS_CEILING: u64 = 25;

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowProfile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Named pattern this profile came from, or "custom".
    #[serde(default)]
    pub pattern: String,
    #[serde(default)]
    pub nodes: Vec<WorkflowNode>,
    /// Cap on nodes running at once. None = no cap beyond the level width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_parallel: Option<usize>,
    /// Loop-until-done control. Ignored by the other patterns.
    #[serde(default, rename = "loop", skip_serializing_if = "Option::is_none")]
    pub loop_: Option<LoopKnobs>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkflowNode {
    pub name: String,
    /// Free-form label surfaced in the UI ("generator", "judge", ...).
    #[serde(default)]
    pub role: String,
    /// Names of nodes whose outputs feed this one. Empty = a root node.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Prompt template. `{{task}}` is the original request; `{{node_name}}`
    /// interpolates that node's output.
    #[serde(default)]
    pub prompt: String,
    /// "" = inherit the session model.
    #[serde(default)]
    pub model: String,
    /// "" = inherit the session endpoint.
    #[serde(default)]
    pub api_url: String,
    #[serde(default)]
    pub api_key: String,
    /// Reasoning-effort tier for CLI providers that support one. "" = default.
    #[serde(default)]
    pub effort: String,
    /// Makes this node's outgoing edges a *choice* rather than a fan-out: it
    /// names one of its dependents, and the branches it did not pick never run.
    ///
    /// Without this a router has to fan out to every branch and have the losers
    /// reply "SKIPPED", which pays for N agent calls to use one of them.
    #[serde(default)]
    pub handoff: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LoopKnobs {
    /// Max rounds, clamped to 1..=MAX_ROUNDS_CEILING (default 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u64>,
    /// Node whose output decides whether to go again. Defaults to the last
    /// terminal node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_node: Option<String>,
    /// Stop once the check node's output contains this marker (case
    /// insensitive). A model that cannot signal completion would otherwise
    /// loop until the ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_marker: Option<String>,
}

// ---------------------------------------------------------------------------
// Execution results
// ---------------------------------------------------------------------------

/// What the caller needs in order to actually run one node.
#[derive(Debug, Clone)]
pub struct NodeRun {
    pub name: String,
    pub role: String,
    /// Prompt with all placeholders already resolved.
    pub prompt: String,
    pub model: String,
    pub api_url: String,
    pub api_key: String,
    pub effort: String,
    /// 0-based round, for loop-until-done. Always 0 otherwise.
    pub round: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeOutcome {
    pub name: String,
    pub role: String,
    pub round: u64,
    pub ok: bool,
    pub output: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// True when the node never ran because an upstream handoff chose a
    /// different branch. Distinct from a failure: nothing went wrong, and it
    /// does not make the run unsuccessful.
    #[serde(default)]
    pub skipped: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkflowRun {
    pub profile: String,
    pub pattern: String,
    pub rounds: u64,
    pub outcomes: Vec<NodeOutcome>,
    /// Output of the terminal node(s), joined. What the caller shows the user.
    pub final_output: String,
    /// True when every node succeeded.
    pub ok: bool,
}

// ---------------------------------------------------------------------------
// Validation + topology
// ---------------------------------------------------------------------------

impl WorkflowProfile {
    /// Nodes that nothing depends on — the graph's outputs.
    pub fn terminal_nodes(&self) -> Vec<String> {
        let referenced: HashSet<&str> =
            self.nodes.iter().flat_map(|n| n.inputs.iter().map(String::as_str)).collect();
        self.nodes
            .iter()
            .filter(|n| !referenced.contains(n.name.as_str()))
            .map(|n| n.name.clone())
            .collect()
    }

    /// Group nodes into levels: everything in a level can run concurrently,
    /// and every level depends only on the ones before it.
    ///
    /// Kahn's algorithm. Returns an error naming the offending nodes when the
    /// graph has a cycle or references a node that does not exist — both would
    /// otherwise deadlock at run time.
    pub fn levels(&self) -> Result<Vec<Vec<usize>>, String> {
        if self.nodes.is_empty() {
            return Err("workflow has no nodes".into());
        }

        let mut index: HashMap<&str, usize> = HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if n.name.trim().is_empty() {
                return Err(format!("node {i} has an empty name"));
            }
            if index.insert(n.name.as_str(), i).is_some() {
                return Err(format!("duplicate node name: {}", n.name));
            }
        }

        // Dependency count per node, and who each node unblocks.
        let mut indegree = vec![0usize; self.nodes.len()];
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];
        for (i, n) in self.nodes.iter().enumerate() {
            for dep in &n.inputs {
                let Some(&d) = index.get(dep.as_str()) else {
                    return Err(format!("node '{}' references unknown input '{}'", n.name, dep));
                };
                if d == i {
                    return Err(format!("node '{}' lists itself as an input", n.name));
                }
                indegree[i] += 1;
                dependents[d].push(i);
            }
        }

        let mut levels: Vec<Vec<usize>> = Vec::new();
        let mut frontier: Vec<usize> =
            (0..self.nodes.len()).filter(|&i| indegree[i] == 0).collect();
        if frontier.is_empty() {
            return Err("workflow has no root node — every node depends on another (cycle)".into());
        }
        let mut placed = 0usize;

        while !frontier.is_empty() {
            placed += frontier.len();
            let mut next: Vec<usize> = Vec::new();
            for &i in &frontier {
                for &d in &dependents[i] {
                    indegree[d] -= 1;
                    if indegree[d] == 0 {
                        next.push(d);
                    }
                }
            }
            levels.push(std::mem::take(&mut frontier));
            frontier = next;
        }

        if placed != self.nodes.len() {
            let stuck: Vec<&str> = self
                .nodes
                .iter()
                .enumerate()
                .filter(|(i, _)| indegree[*i] > 0)
                .map(|(_, n)| n.name.as_str())
                .collect();
            return Err(format!("workflow has a cycle involving: {}", stuck.join(", ")));
        }
        Ok(levels)
    }

    /// Graph shape the scheduler needs: who each node unblocks, how many
    /// dependencies it is waiting on, its transitive ancestors, and a stable
    /// topological order for reporting.
    fn topology(&self) -> Result<Topology, String> {
        // Also validates names, duplicates, unknown inputs and cycles.
        let levels = self.levels()?;

        let index: HashMap<&str, usize> =
            self.nodes.iter().enumerate().map(|(i, n)| (n.name.as_str(), i)).collect();
        let resolve = |dep: &str| -> Result<usize, String> {
            index.get(dep).copied().ok_or_else(|| format!("unknown input '{dep}'"))
        };

        let count = self.nodes.len();
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); count];
        let mut parents: Vec<Vec<usize>> = vec![Vec::new(); count];
        let mut indegree = vec![0usize; count];
        for (i, n) in self.nodes.iter().enumerate() {
            for dep in &n.inputs {
                let d = resolve(dep)?;
                indegree[i] += 1;
                dependents[d].push(i);
                parents[i].push(d);
            }
        }

        // Transitive ancestors, filled in topological order so that every
        // parent's own ancestor set is already complete when we reach a node.
        let order: Vec<usize> = levels.iter().flatten().copied().collect();
        let mut ancestors: Vec<HashSet<usize>> = vec![HashSet::new(); count];
        for &i in &order {
            let mut acc: HashSet<usize> = HashSet::new();
            for dep in &self.nodes[i].inputs {
                let d = resolve(dep)?;
                acc.insert(d);
                acc.extend(ancestors[d].iter().copied());
            }
            ancestors[i] = acc;
        }

        Ok(Topology { dependents, parents, indegree, ancestors, order })
    }
}

/// Precomputed graph shape for one profile.
struct Topology {
    /// `dependents[i]` = nodes that become closer to ready when `i` finishes.
    dependents: Vec<Vec<usize>>,
    /// `parents[i]` = the nodes `i` directly waits on.
    parents: Vec<Vec<usize>>,
    /// `indegree[i]` = how many inputs `i` waits on.
    indegree: Vec<usize>,
    /// `ancestors[i]` = every node `i` transitively depends on. These are the
    /// only nodes whose current-round output `i` may read without racing.
    ancestors: Vec<HashSet<usize>>,
    /// A stable topological order, used to report outcomes deterministically
    /// no matter what order the nodes actually finished in.
    order: Vec<usize>,
}

/// Resolve `{{task}}` and `{{node_name}}` against the task and prior outputs.
///
/// Unknown placeholders are left as-is rather than blanked, so a typo shows up
/// in the prompt instead of silently producing an empty instruction.
pub fn render_template(template: &str, task: &str, state: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes: Vec<char> = template.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '{' && i + 1 < bytes.len() && bytes[i + 1] == '{' {
            if let Some(close) = find_close(&bytes, i + 2) {
                let key: String = bytes[i + 2..close].iter().collect();
                let key = key.trim().to_string();
                let replacement = if key == "task" {
                    Some(task.to_string())
                } else {
                    state.get(&key).cloned()
                };
                if let Some(v) = replacement {
                    out.push_str(&v);
                    i = close + 2;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn find_close(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == '}' && chars[i + 1] == '}' {
            return Some(i);
        }
        // A newline inside a placeholder means it was never one.
        if chars[i] == '\n' {
            return None;
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Executor
// ---------------------------------------------------------------------------

/// What a node may see when its prompt is rendered.
///
/// Its transitive ancestors are guaranteed finished, so their **current-round**
/// output is used. Everything else falls back to the **previous round's**
/// snapshot, because reading a non-ancestor's current-round output would depend
/// on which unrelated node happened to finish first — the same prompt would
/// render differently run to run. Round 0 has no snapshot, so such a
/// placeholder is simply left visible, like any other unresolved name.
///
/// This is what makes `loop_until_done` work: its `agent` node reads
/// `{{findings}}`, which is its own *descendant*, and therefore always means
/// "what the previous round found".
fn visible_state(
    profile: &WorkflowProfile,
    topo: &Topology,
    idx: usize,
    produced: &[Option<String>],
    prev_round: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut visible = prev_round.clone();
    for &a in &topo.ancestors[idx] {
        if let Some(v) = &produced[a] {
            visible.insert(profile.nodes[a].name.clone(), v.clone());
        }
    }
    visible
}

/// Which downstream branch a handoff node selected.
///
/// Candidates are matched longest-name-first so `branch_10` is not shadowed by
/// `branch_1` appearing inside it.
///
/// Returns `None` when the output names no candidate at all. The caller then
/// runs every branch — more expensive than intended, but never wrong, which is
/// the right way to fail when a model ignores the instruction to pick one.
fn choose_handoff_target(
    profile: &WorkflowProfile,
    candidates: &[usize],
    output: &str,
) -> Option<usize> {
    let hay = output.to_lowercase();
    let mut ranked: Vec<usize> = candidates.to_vec();
    ranked.sort_by_key(|&i| std::cmp::Reverse(profile.nodes[i].name.len()));
    ranked.into_iter().find(|&i| {
        let name = profile.nodes[i].name.to_lowercase();
        !name.is_empty() && hay.contains(&name)
    })
}

/// Run a workflow as dataflow: every node starts the moment its own inputs have
/// landed, rather than waiting for an entire level to drain.
///
/// The distinction is worth real time here because nodes are agent calls that
/// take tens to hundreds of seconds and vary wildly in duration. Under level
/// barriers a tournament's second bout cannot start until the slowest attempt
/// in the whole field finishes, so one straggler stalls every branch. Under
/// dataflow the bout whose two attempts are done starts immediately.
///
/// `max_parallel` is a cap on nodes in flight at once, applied across the whole
/// graph rather than per level.
///
/// `run_node` performs the actual agent call. Parameterising it keeps the
/// scheduling logic testable and lets callers supply whatever model plumbing
/// they already have.
///
/// A node whose parents failed still runs, receiving their error text — some
/// patterns (verification, filtering) legitimately want to see a failure.
///
/// Completion order is nondeterministic by design, but the reported result is
/// not: outcomes come back in topological order, and prompts only ever read
/// state that was guaranteed to exist when the node started.
pub async fn execute<F, Fut>(
    profile: &WorkflowProfile,
    task: &str,
    run_node: F,
) -> Result<WorkflowRun, String>
where
    F: Fn(NodeRun) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let topo = profile.topology()?;
    let terminals = profile.terminal_nodes();

    let knobs = profile.loop_.clone().unwrap_or_default();
    let max_rounds = knobs
        .max_rounds
        .unwrap_or(if profile.pattern == PATTERN_LOOP_UNTIL_DONE { 3 } else { 1 })
        .clamp(1, MAX_ROUNDS_CEILING);
    let check_node = knobs
        .check_node
        .clone()
        .or_else(|| terminals.last().cloned())
        .unwrap_or_default();
    let stop_marker = knobs.stop_marker.clone().unwrap_or_else(|| "NO_NEW_FINDINGS".to_string());

    let count = profile.nodes.len();
    let cap = profile.max_parallel.filter(|c| *c > 0).unwrap_or(usize::MAX);

    let mut outcomes: Vec<NodeOutcome> = Vec::new();
    // What the previous round produced. Carried across rounds so a loop can
    // feed its own output back in; the final round's copy is the result.
    let mut state: HashMap<String, String> = HashMap::new();
    let mut rounds_run = 0u64;

    for round in 0..max_rounds {
        rounds_run = round + 1;

        let mut waiting = topo.indegree.clone();
        let mut ready: VecDeque<usize> = (0..count).filter(|&i| waiting[i] == 0).collect();
        // This round's output per node, in the form dependents should see it.
        let mut produced: Vec<Option<String>> = vec![None; count];
        let mut round_outcomes: Vec<Option<NodeOutcome>> = vec![None; count];
        // Branches an upstream handoff declined to take, and every node that
        // ends up unreachable because of one.
        let mut declined = vec![false; count];
        let mut skipped = vec![false; count];
        let mut inflight = FuturesUnordered::new();
        let mut completed = 0usize;

        while completed < count {
            // Start everything whose inputs have landed, up to the cap.
            while inflight.len() < cap {
                let Some(idx) = ready.pop_front() else { break };

                // A node is dead when a handoff passed it over, or when every
                // one of its parents was itself skipped. A node with even one
                // live parent still runs — that is what lets a collector node
                // downstream of a router see the branch that did run.
                let orphaned = !topo.parents[idx].is_empty()
                    && topo.parents[idx].iter().all(|&p| skipped[p]);
                if declined[idx] || orphaned {
                    skipped[idx] = true;
                    completed += 1;
                    let n = &profile.nodes[idx];
                    produced[idx] = Some(format!("[node '{}' not selected]", n.name));
                    round_outcomes[idx] = Some(NodeOutcome {
                        name: n.name.clone(),
                        role: n.role.clone(),
                        round,
                        ok: true,
                        output: String::new(),
                        error: None,
                        skipped: true,
                    });
                    for &d in &topo.dependents[idx] {
                        waiting[d] -= 1;
                        if waiting[d] == 0 {
                            ready.push_back(d);
                        }
                    }
                    // Skipping costs no concurrency slot, so keep draining.
                    continue;
                }

                let n = &profile.nodes[idx];
                let seen = visible_state(profile, &topo, idx, &produced, &state);
                let req = NodeRun {
                    name: n.name.clone(),
                    role: n.role.clone(),
                    prompt: render_template(&n.prompt, task, &seen),
                    model: n.model.clone(),
                    api_url: n.api_url.clone(),
                    api_key: n.api_key.clone(),
                    effort: n.effort.clone(),
                    round,
                };
                let fut = run_node(req.clone());
                inflight.push(async move { (idx, req, fut.await) });
            }

            // Nothing running and nothing ready would mean a cycle, which
            // `levels()` already rejected. Bail rather than spin.
            let Some((idx, req, res)) = inflight.next().await else { break };
            completed += 1;

            let outcome = match res {
                Ok(output) => NodeOutcome {
                    name: req.name.clone(),
                    role: req.role.clone(),
                    round,
                    ok: true,
                    output,
                    error: None,
                    skipped: false,
                },
                Err(e) => NodeOutcome {
                    name: req.name.clone(),
                    role: req.role.clone(),
                    round,
                    ok: false,
                    output: String::new(),
                    error: Some(e),
                    skipped: false,
                },
            };

            // A successful handoff node picks one branch; the rest never run.
            // A *failed* one picks nothing, so every branch stays live — losing
            // the routing decision should cost money, not correctness.
            if outcome.ok && profile.nodes[idx].handoff {
                let choices = topo.dependents[idx].clone();
                if let Some(chosen) = choose_handoff_target(profile, &choices, &outcome.output) {
                    for &c in &choices {
                        if c != chosen {
                            declined[c] = true;
                        }
                    }
                }
            }
            // Downstream nodes read the error text when a parent fails, so a
            // failure is visible in the prompt rather than silent.
            produced[idx] = Some(if outcome.ok {
                outcome.output.clone()
            } else {
                format!(
                    "[node '{}' failed: {}]",
                    outcome.name,
                    outcome.error.clone().unwrap_or_default()
                )
            });
            round_outcomes[idx] = Some(outcome);

            for &d in &topo.dependents[idx] {
                waiting[d] -= 1;
                if waiting[d] == 0 {
                    ready.push_back(d);
                }
            }
        }

        // Report in topological order regardless of who finished first, so the
        // transcript reads the way the graph is drawn.
        for &i in &topo.order {
            if let Some(o) = round_outcomes[i].take() {
                outcomes.push(o);
            }
        }
        for (i, out) in produced.into_iter().enumerate() {
            if let Some(v) = out {
                state.insert(profile.nodes[i].name.clone(), v);
            }
        }

        // Loop-until-done: stop as soon as the check node says there is
        // nothing new.
        let done = state
            .get(&check_node)
            .map(|v| v.to_uppercase().contains(&stop_marker.to_uppercase()))
            .unwrap_or(true);
        if done {
            break;
        }
    }

    let final_output = terminals
        .iter()
        .filter_map(|t| state.get(t).cloned())
        .collect::<Vec<_>>()
        .join("\n\n");

    Ok(WorkflowRun {
        profile: profile.name.clone(),
        pattern: profile.pattern.clone(),
        rounds: rounds_run,
        ok: outcomes.iter().all(|o| o.ok),
        final_output,
        outcomes,
    })
}

// ---------------------------------------------------------------------------
// The six patterns
// ---------------------------------------------------------------------------

pub const PATTERN_CLASSIFY_AND_ACT: &str = "classify_and_act";
pub const PATTERN_FANOUT_SYNTHESIZE: &str = "fanout_and_synthesize";
pub const PATTERN_ADVERSARIAL_VERIFY: &str = "adversarial_verification";
pub const PATTERN_GENERATE_AND_FILTER: &str = "generate_and_filter";
pub const PATTERN_TOURNAMENT: &str = "tournament";
pub const PATTERN_LOOP_UNTIL_DONE: &str = "loop_until_done";
pub const PATTERN_DEBATE: &str = "debate";
pub const PATTERN_HIERARCHICAL: &str = "hierarchical_swarm";
pub const PATTERN_SEQUENTIAL_PIPELINE: &str = "sequential_pipeline";
pub const PATTERN_MIXTURE_OF_AGENTS: &str = "mixture_of_agents";

/// Every built-in pattern, for populating a mode picker.
pub fn pattern_catalog() -> Vec<(&'static str, &'static str)> {
    vec![
        (PATTERN_CLASSIFY_AND_ACT, "Classify-And-Act — a classifier routes the task to one specialist"),
        (PATTERN_FANOUT_SYNTHESIZE, "Fanout-And-Synthesize — N agents work in parallel, one synthesises"),
        (PATTERN_ADVERSARIAL_VERIFY, "Adversarial Verification — verifiers try to refute the worker"),
        (PATTERN_GENERATE_AND_FILTER, "Generate-And-Filter — many ideas, filtered by rubric and deduped"),
        (PATTERN_TOURNAMENT, "Tournament — attempts compete via pairwise judges"),
        (PATTERN_LOOP_UNTIL_DONE, "Loop Until Done — repeat until nothing new is found"),
        (PATTERN_DEBATE, "Debate — a panel argues across rounds, then a moderator rules"),
        (PATTERN_HIERARCHICAL, "Hierarchical — a director splits the work, then integrates it"),
        (PATTERN_SEQUENTIAL_PIPELINE, "Sequential Pipeline — each stage refines the previous one"),
        (PATTERN_MIXTURE_OF_AGENTS, "Mixture-of-Agents — proposals, cross-aware refinement, aggregation"),
    ]
}

fn node(name: &str, role: &str, inputs: &[&str], prompt: &str) -> WorkflowNode {
    WorkflowNode {
        name: name.to_string(),
        role: role.to_string(),
        inputs: inputs.iter().map(|s| s.to_string()).collect(),
        prompt: prompt.to_string(),
        ..Default::default()
    }
}

/// Build one of the built-in patterns. `width` sizes the parallel part of the
/// topology (branches, workers, verifiers, generators, attempts).
pub fn build_pattern(pattern: &str, width: usize) -> Result<WorkflowProfile, String> {
    let w = width.clamp(2, 12);
    let mut p = WorkflowProfile { pattern: pattern.to_string(), ..Default::default() };

    match pattern {
        PATTERN_CLASSIFY_AND_ACT => {
            p.name = "Classify-And-Act".into();
            p.description = "A classifier picks the best specialist, and only that specialist runs.".into();
            let branch_list = (1..=w).map(|i| format!("branch_{i}")).collect::<Vec<_>>().join(", ");
            let mut classifier = node("classifier", "classifier", &[],
                &format!("Classify this task and name exactly ONE of [{branch_list}] best suited to it. \
Reply with the chosen name on the first line, then one sentence of justification.\n\nTASK:\n{{{{task}}}}"));
            // The branches are a choice, not a fan-out: the ones the classifier
            // passes over are never dispatched. Routing to one of 8 specialists
            // costs 3 agent calls rather than 10.
            classifier.handoff = true;
            p.nodes.push(classifier);
            for i in 1..=w {
                p.nodes.push(node(&format!("branch_{i}"), "specialist", &["classifier"],
                    &format!("You are specialist {i}, selected by the classifier for this task:\n\
{{{{classifier}}}}\n\nComplete the task fully.\n\nTASK:\n{{{{task}}}}")));
            }
            let inputs: Vec<String> = (1..=w).map(|i| format!("branch_{i}")).collect();
            let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
            let body = inputs.iter().map(|n| format!("{n}:\n{{{{{n}}}}}")).collect::<Vec<_>>().join("\n\n");
            p.nodes.push(node("result", "collector", &refs,
                &format!("Exactly one branch below ran; the others were not selected and are marked as \
such. Return the acting branch's answer verbatim, with no commentary.\n\n{body}")));
        }

        PATTERN_FANOUT_SYNTHESIZE => {
            p.name = "Fanout-And-Synthesize".into();
            p.description = "N agents attack the task in parallel; a synthesiser merges their work.".into();
            for i in 1..=w {
                p.nodes.push(node(&format!("worker_{i}"), "worker", &[],
                    &format!("You are worker {i} of {w}. Approach this task from your own angle — do not \
hedge toward what others might say.\n\nTASK:\n{{{{task}}}}")));
            }
            let inputs: Vec<String> = (1..=w).map(|i| format!("worker_{i}")).collect();
            let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
            let body = inputs.iter().map(|n| format!("--- {n} ---\n{{{{{n}}}}}")).collect::<Vec<_>>().join("\n\n");
            p.nodes.push(node("synthesis", "synthesizer", &refs,
                &format!("Merge these independent attempts into one answer. Keep what they agree on, \
resolve conflicts explicitly, and say when they disagree rather than averaging them away.\n\n\
TASK:\n{{{{task}}}}\n\n{body}")));
        }

        PATTERN_ADVERSARIAL_VERIFY => {
            p.name = "Adversarial Verification".into();
            p.description = "A worker answers; independent verifiers try to refute it.".into();
            p.nodes.push(node("worker", "worker", &[], "Complete this task.\n\nTASK:\n{{task}}"));
            let lenses = ["correctness", "security", "does-it-actually-reproduce", "edge cases",
                          "performance", "maintainability", "spec compliance", "data integrity",
                          "error handling", "concurrency", "API contract", "test coverage"];
            for i in 1..=w {
                let lens = lenses[(i - 1) % lenses.len()];
                p.nodes.push(node(&format!("verifier_{i}"), "verifier", &["worker"],
                    &format!("Try to REFUTE the answer below, viewed through the '{lens}' lens. \
Default to refuted=true when uncertain. State refuted=true or refuted=false on the first line, \
then your evidence.\n\nTASK:\n{{{{task}}}}\n\nANSWER:\n{{{{worker}}}}")));
            }
            let inputs: Vec<String> = (1..=w).map(|i| format!("verifier_{i}")).collect();
            let mut refs: Vec<&str> = vec!["worker"];
            refs.extend(inputs.iter().map(String::as_str));
            let body = inputs.iter().map(|n| format!("--- {n} ---\n{{{{{n}}}}}")).collect::<Vec<_>>().join("\n\n");
            p.nodes.push(node("verdict", "judge", &refs,
                &format!("Count the verifiers reporting refuted=true. If a majority refuted the answer, \
return a corrected answer. Otherwise return the original, noting any surviving caveats.\n\n\
ANSWER:\n{{{{worker}}}}\n\n{body}")));
        }

        PATTERN_GENERATE_AND_FILTER => {
            p.name = "Generate-And-Filter".into();
            p.description = "Many candidate ideas, cut down by an explicit rubric with deduplication.".into();
            for i in 1..=w {
                p.nodes.push(node(&format!("generator_{i}"), "generator", &[],
                    &format!("You are generator {i} of {w}. Produce 3-5 DISTINCT candidate ideas for the \
task. Favour range over polish; do not self-censor.\n\nTASK:\n{{{{task}}}}")));
            }
            let inputs: Vec<String> = (1..=w).map(|i| format!("generator_{i}")).collect();
            let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
            let body = inputs.iter().map(|n| format!("--- {n} ---\n{{{{{n}}}}}")).collect::<Vec<_>>().join("\n\n");
            p.nodes.push(node("filter", "filter", &refs,
                &format!("Merge every candidate below. First remove duplicates and near-duplicates, \
naming which you merged. Then score what remains against the task's real constraints and keep only \
the strongest. State the rubric you applied, and list what you discarded and why.\n\n\
TASK:\n{{{{task}}}}\n\n{body}")));
        }

        PATTERN_TOURNAMENT => {
            p.name = "Tournament".into();
            p.description = "Independent attempts compete through pairwise judges to a single winner.".into();
            for i in 1..=w {
                p.nodes.push(node(&format!("attempt_{i}"), "attempt", &[],
                    &format!("You are contender {i} of {w}. Produce your strongest complete answer.\n\nTASK:\n{{{{task}}}}")));
            }
            // Pairwise bracket: round by round, halve the field.
            let mut current: Vec<String> = (1..=w).map(|i| format!("attempt_{i}")).collect();
            let mut bout = 0usize;
            while current.len() > 1 {
                let mut next: Vec<String> = Vec::new();
                for pair in current.chunks(2) {
                    if pair.len() == 1 {
                        // Odd one out gets a bye.
                        next.push(pair[0].clone());
                        continue;
                    }
                    bout += 1;
                    let name = format!("bout_{bout}");
                    let (a, b) = (&pair[0], &pair[1]);
                    p.nodes.push(node(&name, "judge", &[a.as_str(), b.as_str()],
                        &format!("Judge these two answers against the task. Pick the better one and \
return it VERBATIM, with a one-line justification prefixed 'WINNER:'. Do not blend them.\n\n\
TASK:\n{{{{task}}}}\n\n--- A ---\n{{{{{a}}}}}\n\n--- B ---\n{{{{{b}}}}}")));
                    next.push(name);
                }
                current = next;
            }
            if let Some(champion) = current.first() {
                p.nodes.push(node("winner", "final", &[champion.as_str()],
                    &format!("Return the winning answer below, cleaned of any judging commentary.\n\n{{{{{champion}}}}}")));
            }
        }

        PATTERN_LOOP_UNTIL_DONE => {
            p.name = "Loop Until Done".into();
            p.description = "Re-runs until a round surfaces nothing new.".into();
            p.nodes.push(node("agent", "worker", &[],
                "Work the task. Previous rounds found:\n{{findings}}\n\nDo NOT repeat anything already \
listed above — look for what earlier rounds missed.\n\nTASK:\n{{task}}"));
            p.nodes.push(node("findings", "checker", &["agent"],
                "List only findings from this round that are genuinely NEW versus earlier rounds. \
If there are none, reply with exactly NO_NEW_FINDINGS.\n\nTHIS ROUND:\n{{agent}}"));
            p.loop_ = Some(LoopKnobs {
                max_rounds: Some(3),
                check_node: Some("findings".into()),
                stop_marker: Some("NO_NEW_FINDINGS".into()),
            });
        }

        PATTERN_DEBATE => {
            p.name = "Debate".into();
            p.description =
                "A panel argues in rounds, each round reading the last, then a moderator rules.".into();
            // Rounds are unrolled into the graph rather than looped, so the
            // transcript shows who said what when, and the moderator runs once.
            const ROUNDS: usize = 2;
            for i in 1..=w {
                p.nodes.push(node(&format!("panelist_{i}_r1"), "panelist", &[],
                    &format!("You are panelist {i} of {w}. State your position on the task and your \
strongest supporting argument. Do not hedge toward what others might say.\n\nTASK:\n{{{{task}}}}")));
            }
            for r in 2..=ROUNDS {
                let prev: Vec<String> = (1..=w).map(|i| format!("panelist_{i}_r{}", r - 1)).collect();
                let refs: Vec<&str> = prev.iter().map(String::as_str).collect();
                let body = prev.iter()
                    .map(|n| format!("--- {n} ---\n{{{{{n}}}}}"))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                for i in 1..=w {
                    p.nodes.push(node(&format!("panelist_{i}_r{r}"), "panelist", &refs,
                        &format!("You are panelist {i}. Round {r}. Read every position below, then \
respond: concede what genuinely defeats your argument, and press what still stands. Do not restate \
round {} verbatim.\n\nTASK:\n{{{{task}}}}\n\n{body}", r - 1)));
                }
            }
            let last: Vec<String> = (1..=w).map(|i| format!("panelist_{i}_r{ROUNDS}")).collect();
            let refs: Vec<&str> = last.iter().map(String::as_str).collect();
            let body = last.iter()
                .map(|n| format!("--- {n} ---\n{{{{{n}}}}}"))
                .collect::<Vec<_>>()
                .join("\n\n");
            p.nodes.push(node("moderator", "judge", &refs,
                &format!("Rule on this debate. Say what the panel converged on, what remains \
genuinely contested and why, and give the answer the evidence supports — do not split the \
difference to seem balanced.\n\nTASK:\n{{{{task}}}}\n\n{body}")));
        }

        PATTERN_HIERARCHICAL => {
            p.name = "Hierarchical".into();
            p.description = "A director decomposes the task, workers execute, the director integrates.".into();
            let worker_list = (1..=w).map(|i| format!("worker_{i}")).collect::<Vec<_>>().join(", ");
            p.nodes.push(node("director", "director", &[],
                &format!("Decompose this task into exactly {w} independent sub-tasks, one for each of \
[{worker_list}]. Sub-tasks must not overlap and must together cover the whole task. Address each \
worker by name.\n\nTASK:\n{{{{task}}}}")));
            for i in 1..=w {
                p.nodes.push(node(&format!("worker_{i}"), "worker", &["director"],
                    &format!("You are worker_{i}. Carry out ONLY the sub-task the director assigned to \
you; ignore the others. If your assignment is unclear, say so plainly instead of inventing one.\n\n\
DIRECTOR'S PLAN:\n{{{{director}}}}\n\nORIGINAL TASK:\n{{{{task}}}}")));
            }
            let mut refs: Vec<&str> = vec!["director"];
            let inputs: Vec<String> = (1..=w).map(|i| format!("worker_{i}")).collect();
            refs.extend(inputs.iter().map(String::as_str));
            let body = inputs.iter()
                .map(|n| format!("--- {n} ---\n{{{{{n}}}}}"))
                .collect::<Vec<_>>()
                .join("\n\n");
            p.nodes.push(node("integration", "director", &refs,
                &format!("You are the director again. Integrate the workers' results into one \
deliverable. Name any sub-task that came back incomplete rather than papering over it.\n\n\
ORIGINAL TASK:\n{{{{task}}}}\n\nYOUR PLAN:\n{{{{director}}}}\n\n{body}")));
        }

        PATTERN_SEQUENTIAL_PIPELINE => {
            p.name = "Sequential Pipeline".into();
            p.description = "A relay: each stage improves on the stage before it.".into();
            let jobs = ["draft it", "find everything wrong with it", "rewrite it against that critique",
                        "check the rewrite against the original task", "tighten it", "fact-check every claim",
                        "simplify without losing content", "final polish"];
            let roles = ["drafter", "critic", "reviser", "checker", "editor", "fact-checker",
                         "simplifier", "polisher"];
            for i in 1..=w {
                let job = jobs[(i - 1) % jobs.len()];
                let role = roles[(i - 1) % roles.len()];
                if i == 1 {
                    p.nodes.push(node("stage_1", role, &[],
                        &format!("Stage 1 of {w}: {job}.\n\nTASK:\n{{{{task}}}}")));
                } else {
                    let prev = format!("stage_{}", i - 1);
                    p.nodes.push(node(&format!("stage_{i}"), role, &[prev.as_str()],
                        &format!("Stage {i} of {w}: {job}. Work from the previous stage's output — \
carry forward what is already good rather than starting over.\n\nTASK:\n{{{{task}}}}\n\n\
PREVIOUS STAGE:\n{{{{{prev}}}}}")));
                }
            }
        }

        PATTERN_MIXTURE_OF_AGENTS => {
            p.name = "Mixture-of-Agents".into();
            p.description =
                "Independent proposals, a refinement layer that reads all of them, then aggregation.".into();
            for i in 1..=w {
                p.nodes.push(node(&format!("proposer_{i}"), "proposer", &[],
                    &format!("You are proposer {i} of {w}. Answer the task independently and \
completely.\n\nTASK:\n{{{{task}}}}")));
            }
            let props: Vec<String> = (1..=w).map(|i| format!("proposer_{i}")).collect();
            let prop_refs: Vec<&str> = props.iter().map(String::as_str).collect();
            let prop_body = props.iter()
                .map(|n| format!("--- {n} ---\n{{{{{n}}}}}"))
                .collect::<Vec<_>>()
                .join("\n\n");
            for i in 1..=w {
                p.nodes.push(node(&format!("refiner_{i}"), "refiner", &prop_refs,
                    &format!("You are refiner {i} of {w}. Every proposal is below. Produce one improved \
answer that takes the strongest parts of each and fixes what they got wrong. Do not simply pick a \
favourite.\n\nTASK:\n{{{{task}}}}\n\n{prop_body}")));
            }
            let refs_v: Vec<String> = (1..=w).map(|i| format!("refiner_{i}")).collect();
            let ref_refs: Vec<&str> = refs_v.iter().map(String::as_str).collect();
            let ref_body = refs_v.iter()
                .map(|n| format!("--- {n} ---\n{{{{{n}}}}}"))
                .collect::<Vec<_>>()
                .join("\n\n");
            p.nodes.push(node("aggregator", "synthesizer", &ref_refs,
                &format!("Produce the final answer from the refined candidates below. Where they still \
disagree, say which is better supported and why rather than averaging them.\n\n\
TASK:\n{{{{task}}}}\n\n{ref_body}")));
        }

        other => return Err(format!("unknown workflow pattern: {other}")),
    }

    // Every generated topology must be a runnable DAG.
    p.levels()?;
    Ok(p)
}

// ---------------------------------------------------------------------------
// Running a workflow against the real agent loop
// ---------------------------------------------------------------------------

/// Session-level defaults a workflow node inherits when it specifies none.
#[derive(Debug, Clone, Default)]
pub struct WorkflowContext {
    pub api_key: String,
    pub api_url: String,
    pub model: String,
    pub sandbox_dir: String,
    pub session_id: String,
    /// Per-role model roster. A node that names no model of its own is matched
    /// against this by role, which is what lets one workflow run its judges on
    /// a different model from its workers.
    pub model_pool: Vec<crate::server::data::ModelPoolEntry>,
}

/// Execute a workflow, running each node as a full agentic turn (tools and
/// all) via the normal tool loop. Nodes that name their own model/endpoint
/// override the session's, which is what makes per-role model selection work.
/// Receives a human-readable line as each node starts and finishes, so a caller
/// can surface progress. A long DAG run is otherwise silent for minutes.
pub type ProgressSink = std::sync::Arc<dyn Fn(String) + Send + Sync>;

pub async fn run_with_agent_loop(
    profile: &WorkflowProfile,
    task: &str,
    ctx: &WorkflowContext,
    progress: Option<ProgressSink>,
) -> Result<WorkflowRun, String> {
    use crate::server::services::toolbox::{call_with_tools, SubAgentConfig};

    execute(profile, task, |req: NodeRun| {
        let ctx = ctx.clone();
        let progress = progress.clone();
        async move {
            if let Some(p) = &progress {
                p(format!(
                    "▶ **{}**{} starting…\n",
                    req.name,
                    if req.role.is_empty() { String::new() } else { format!(" ({})", req.role) },
                ));
            }
            // Precedence: what the node names > the roster entry for its role
            // > the session default. An explicit per-node choice is never
            // overridden by the roster.
            let roster = crate::server::data::model_for_role(&ctx.model_pool, &req.role);
            let pick = |node: String, from_roster: Option<&str>, session: &str| -> String {
                if !node.is_empty() {
                    return node;
                }
                match from_roster.filter(|v| !v.is_empty()) {
                    Some(v) => v.to_string(),
                    None => session.to_string(),
                }
            };
            let api_key = pick(req.api_key, roster.map(|r| r.api_key.as_str()), &ctx.api_key);
            let api_url = pick(req.api_url, roster.map(|r| r.api_url.as_str()), &ctx.api_url);
            let model = pick(req.model, roster.map(|r| r.model.as_str()), &ctx.model);
            let effort = pick(req.effort, roster.map(|r| r.effort.as_str()), "");

            let system = format!(
                "You are the '{}' node of a multi-agent workflow{}. Return only the content this \
node is responsible for — no preamble about being an AI or describing your role.",
                req.name,
                if req.role.is_empty() { String::new() } else { format!(" acting as the {}", req.role) },
            );

            let sub_agent = SubAgentConfig {
                api_key: api_key.clone(),
                api_url: api_url.clone(),
                model: model.clone(),
                effort,
                session_id: ctx.session_id.clone(),
                agent_id: req.name.clone(),
                agent_role: req.role.clone(),
                ..Default::default()
            };

            let messages = vec![serde_json::json!({ "role": "user", "content": req.prompt })];
            let result = call_with_tools(
                &api_key,
                &api_url,
                &model,
                messages,
                Some(system),
                &ctx.sandbox_dir,
                |_| {},
                sub_agent,
            )
            .await;

            if result.content.trim().is_empty() {
                if let Some(p) = &progress {
                    p(format!("✗ **{}** returned nothing\n", req.name));
                }
                Err(format!("node '{}' returned no content", req.name))
            } else {
                if let Some(p) = &progress {
                    p(format!("✓ **{}** done ({} chars)\n", req.name, result.content.len()));
                }
                Ok(result.content)
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn simple() -> WorkflowProfile {
        WorkflowProfile {
            name: "t".into(),
            nodes: vec![
                node("a", "", &[], "A: {{task}}"),
                node("b", "", &[], "B: {{task}}"),
                node("c", "", &["a", "b"], "C sees {{a}} and {{b}}"),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn independent_nodes_share_a_level_and_dependents_follow() {
        let levels = simple().levels().unwrap();
        assert_eq!(levels.len(), 2, "a+b run together, then c");
        assert_eq!(levels[0].len(), 2);
        assert_eq!(levels[1], vec![2]);
    }

    #[test]
    fn terminal_nodes_are_the_ones_nothing_depends_on() {
        assert_eq!(simple().terminal_nodes(), vec!["c".to_string()]);
    }

    #[test]
    fn a_cycle_is_rejected_by_name_rather_than_deadlocking() {
        let p = WorkflowProfile {
            nodes: vec![
                node("a", "", &["b"], ""),
                node("b", "", &["a"], ""),
            ],
            ..Default::default()
        };
        let err = p.levels().unwrap_err();
        assert!(err.contains("cycle"), "got: {err}");
    }

    #[test]
    fn unknown_input_is_rejected() {
        let p = WorkflowProfile { nodes: vec![node("a", "", &["ghost"], "")], ..Default::default() };
        assert!(p.levels().unwrap_err().contains("unknown input"));
    }

    #[test]
    fn duplicate_node_names_are_rejected() {
        let p = WorkflowProfile {
            nodes: vec![node("a", "", &[], ""), node("a", "", &[], "")],
            ..Default::default()
        };
        assert!(p.levels().unwrap_err().contains("duplicate"));
    }

    #[test]
    fn templates_resolve_task_and_node_outputs() {
        let mut st = HashMap::new();
        st.insert("a".to_string(), "ALPHA".to_string());
        assert_eq!(render_template("{{task}} / {{a}}", "T", &st), "T / ALPHA");
    }

    #[test]
    fn unknown_placeholder_is_left_visible_instead_of_blanked() {
        let st = HashMap::new();
        assert_eq!(render_template("x {{nope}} y", "T", &st), "x {{nope}} y");
    }

    #[tokio::test]
    async fn executes_in_dependency_order_and_feeds_parent_output_downstream() {
        let p = simple();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let s2 = seen.clone();
        let run = |r: NodeRun| {
            let s = s2.clone();
            async move {
                s.lock().unwrap().push(r.name.clone());
                Ok(format!("out-{}", r.name))
            }
        };
        let res = execute(&p, "TASK", run).await.unwrap();
        assert!(res.ok);
        // c must be last, and must have seen both parents' outputs.
        let order = seen.lock().unwrap().clone();
        assert_eq!(order.last().unwrap(), "c");
        let c = res.outcomes.iter().find(|o| o.name == "c").unwrap();
        assert_eq!(c.output, "out-c");
        assert_eq!(res.final_output, "out-c");
    }

    #[tokio::test]
    async fn a_failed_node_is_reported_and_its_error_reaches_dependents() {
        let p = simple();
        let run = |r: NodeRun| async move {
            if r.name == "a" { Err("boom".to_string()) } else { Ok(r.prompt) }
        };
        let res = execute(&p, "TASK", run).await.unwrap();
        assert!(!res.ok);
        let c = res.outcomes.iter().find(|o| o.name == "c").unwrap();
        assert!(c.output.contains("boom"), "dependent should see the failure: {}", c.output);
    }

    #[tokio::test]
    async fn loop_until_done_stops_once_the_checker_reports_nothing_new() {
        let p = build_pattern(PATTERN_LOOP_UNTIL_DONE, 2).unwrap();
        let run = |r: NodeRun| async move {
            if r.name == "findings" && r.round >= 1 {
                Ok("NO_NEW_FINDINGS".to_string())
            } else {
                Ok(format!("round {} finding", r.round))
            }
        };
        let res = execute(&p, "TASK", run).await.unwrap();
        assert_eq!(res.rounds, 2, "should stop on the round that reports nothing new");
    }

    #[tokio::test]
    async fn loop_until_done_respects_its_round_ceiling() {
        let mut p = build_pattern(PATTERN_LOOP_UNTIL_DONE, 2).unwrap();
        p.loop_ = Some(LoopKnobs { max_rounds: Some(2), ..Default::default() });
        let run = |_r: NodeRun| async move { Ok("still finding things".to_string()) };
        let res = execute(&p, "TASK", run).await.unwrap();
        assert_eq!(res.rounds, 2, "must stop at max_rounds even when never done");
    }

    #[test]
    fn every_built_in_pattern_builds_a_valid_dag() {
        for (name, _) in pattern_catalog() {
            let p = build_pattern(name, 4).unwrap_or_else(|e| panic!("{name}: {e}"));
            let levels = p.levels().unwrap_or_else(|e| panic!("{name} topology: {e}"));
            assert!(!levels.is_empty(), "{name} produced no levels");
            assert!(!p.terminal_nodes().is_empty(), "{name} has no terminal node");
        }
    }

    #[test]
    fn fanout_runs_all_workers_concurrently_then_synthesises() {
        let p = build_pattern(PATTERN_FANOUT_SYNTHESIZE, 5).unwrap();
        let levels = p.levels().unwrap();
        assert_eq!(levels.len(), 2);
        assert_eq!(levels[0].len(), 5, "all workers belong to one level");
        assert_eq!(p.terminal_nodes(), vec!["synthesis".to_string()]);
    }

    #[test]
    fn tournament_halves_the_field_each_round_to_one_winner() {
        let p = build_pattern(PATTERN_TOURNAMENT, 4).unwrap();
        // 4 attempts -> 2 bouts -> 1 bout -> winner
        assert_eq!(p.nodes.iter().filter(|n| n.role == "judge").count(), 3);
        assert_eq!(p.terminal_nodes(), vec!["winner".to_string()]);
    }

    #[test]
    fn tournament_gives_a_bye_when_the_field_is_odd() {
        let p = build_pattern(PATTERN_TOURNAMENT, 3).unwrap();
        assert_eq!(p.terminal_nodes(), vec!["winner".to_string()]);
        p.levels().expect("odd bracket must still be a valid DAG");
    }

    #[test]
    fn unknown_pattern_is_rejected() {
        assert!(build_pattern("nonsense", 3).is_err());
    }

    // --- scheduling -------------------------------------------------------

    /// The test that distinguishes dataflow from level barriers.
    ///
    /// Two independent branches: `slow → slow_child` and `fast → fast_child`.
    /// `slow` refuses to finish until `fast_child` has *started*. Under
    /// dataflow that resolves fine — `fast_child`'s only dependency is `fast`,
    /// which finished immediately. Under level barriers it deadlocks, because
    /// `fast_child` sits in level 1 and level 1 cannot begin until `slow`
    /// finishes in level 0. The timeout turns that deadlock into a readable
    /// failure instead of a hung test run.
    #[tokio::test]
    async fn a_ready_node_starts_without_waiting_for_an_unrelated_slow_branch() {
        let p = WorkflowProfile {
            nodes: vec![
                node("slow", "", &[], ""),
                node("fast", "", &[], ""),
                node("slow_child", "", &["slow"], ""),
                node("fast_child", "", &["fast"], ""),
            ],
            ..Default::default()
        };

        let gate = std::sync::Arc::new(tokio::sync::Notify::new());
        let g = gate.clone();
        let run = move |r: NodeRun| {
            let gate = g.clone();
            async move {
                match r.name.as_str() {
                    "slow" => gate.notified().await,
                    "fast_child" => gate.notify_one(),
                    _ => {}
                }
                Ok(format!("out-{}", r.name))
            }
        };

        let res = tokio::time::timeout(std::time::Duration::from_secs(10), execute(&p, "T", run))
            .await
            .expect(
                "a node waited on an unrelated branch — the scheduler is barriered, not dataflow",
            )
            .unwrap();
        assert!(res.ok);
        assert_eq!(res.outcomes.len(), 4);
    }

    /// Completion order is not report order. `a` cannot finish until `b` has,
    /// so the graph completes b→a→c, but the transcript must still read a,b,c.
    #[tokio::test]
    async fn outcomes_are_reported_in_topological_order_not_completion_order() {
        let p = simple();
        let gate = std::sync::Arc::new(tokio::sync::Notify::new());
        let finished = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let (g, f) = (gate.clone(), finished.clone());
        let run = move |r: NodeRun| {
            let (gate, fin) = (g.clone(), f.clone());
            async move {
                match r.name.as_str() {
                    "a" => gate.notified().await,
                    "b" => gate.notify_one(),
                    _ => {}
                }
                fin.lock().unwrap().push(r.name.clone());
                Ok(format!("out-{}", r.name))
            }
        };

        let res = tokio::time::timeout(std::time::Duration::from_secs(10), execute(&p, "T", run))
            .await
            .expect("deadlock")
            .unwrap();

        assert_eq!(
            finished.lock().unwrap().clone(),
            vec!["b", "a", "c"],
            "precondition: nodes really did finish out of graph order"
        );
        let reported: Vec<&str> = res.outcomes.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(
            reported,
            vec!["a", "b", "c"],
            "the report follows the graph, not the clock"
        );
    }

    #[tokio::test]
    async fn max_parallel_caps_nodes_in_flight_across_the_whole_graph() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

        let p = WorkflowProfile {
            nodes: (1..=4).map(|i| node(&format!("n{i}"), "", &[], "")).collect(),
            max_parallel: Some(2),
            ..Default::default()
        };

        let live = std::sync::Arc::new(AtomicUsize::new(0));
        let peak = std::sync::Arc::new(AtomicUsize::new(0));
        let (l, pk) = (live.clone(), peak.clone());
        let run = move |_r: NodeRun| {
            let (live, peak) = (l.clone(), pk.clone());
            async move {
                let now = live.fetch_add(1, SeqCst) + 1;
                peak.fetch_max(now, SeqCst);
                tokio::task::yield_now().await;
                live.fetch_sub(1, SeqCst);
                Ok("ok".to_string())
            }
        };

        let res = execute(&p, "T", run).await.unwrap();
        assert!(res.ok);
        assert_eq!(res.outcomes.len(), 4, "every node still runs");
        assert_eq!(
            peak.load(SeqCst),
            2,
            "four independent roots must run two at a time — no more, and no fewer"
        );
    }

    /// A prompt naming a node that is not one of its ancestors is reading
    /// across rounds, not sideways. It must see the previous round's value and
    /// never whatever a concurrently-running sibling happens to have produced,
    /// or the same graph would render different prompts run to run.
    #[tokio::test]
    async fn a_non_ancestor_reference_reads_the_previous_round_not_a_racing_sibling() {
        let p = WorkflowProfile {
            nodes: vec![node("x", "", &[], "plain"), node("y", "", &[], "saw:{{x}}")],
            loop_: Some(LoopKnobs {
                max_rounds: Some(2),
                check_node: Some("y".into()),
                stop_marker: Some("NEVER_APPEARS".into()),
            }),
            ..Default::default()
        };

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let s = seen.clone();
        let run = move |r: NodeRun| {
            let s = s.clone();
            async move {
                if r.name == "y" {
                    s.lock().unwrap().push(r.prompt.clone());
                }
                Ok(format!("{}-r{}", r.name, r.round))
            }
        };

        execute(&p, "T", run).await.unwrap();

        let prompts = seen.lock().unwrap().clone();
        assert_eq!(prompts.len(), 2, "both rounds should have run");
        assert_eq!(prompts[0], "saw:{{x}}", "round 0 has no previous round to read");
        assert_eq!(prompts[1], "saw:x-r0", "round 1 reads round 0's x, not round 1's");
    }

    // --- the ported swarm topologies --------------------------------------

    #[test]
    fn debate_panellists_read_the_whole_previous_round_then_a_moderator_rules() {
        let p = build_pattern(PATTERN_DEBATE, 3).unwrap();
        let levels = p.levels().unwrap();
        assert_eq!(levels.len(), 3, "round 1, round 2, moderator");
        assert_eq!(levels[0].len(), 3, "round 1 is independent");
        assert_eq!(levels[1].len(), 3, "round 2 runs as a layer");
        assert_eq!(p.terminal_nodes(), vec!["moderator".to_string()]);

        // Each round-2 panellist must see every round-1 panellist, or it is not
        // a debate — it is three monologues.
        let r2 = p.nodes.iter().find(|n| n.name == "panelist_1_r2").unwrap();
        for i in 1..=3 {
            assert!(
                r2.inputs.contains(&format!("panelist_{i}_r1")),
                "round 2 must read all of round 1: {:?}",
                r2.inputs
            );
        }
    }

    #[test]
    fn hierarchical_returns_to_the_director_for_integration() {
        let p = build_pattern(PATTERN_HIERARCHICAL, 4).unwrap();
        assert_eq!(p.terminal_nodes(), vec!["integration".to_string()]);
        let integ = p.nodes.iter().find(|n| n.name == "integration").unwrap();
        assert!(
            integ.inputs.contains(&"director".to_string()),
            "the integrator must see the original plan to notice a sub-task that came back empty"
        );
        assert_eq!(p.nodes.iter().filter(|n| n.role == "worker").count(), 4);
    }

    #[test]
    fn sequential_pipeline_is_a_chain_with_no_parallelism() {
        let p = build_pattern(PATTERN_SEQUENTIAL_PIPELINE, 5).unwrap();
        let levels = p.levels().unwrap();
        assert_eq!(levels.len(), 5, "a relay has one node per level");
        assert!(levels.iter().all(|l| l.len() == 1));
        assert_eq!(p.terminal_nodes(), vec!["stage_5".to_string()]);
    }

    #[test]
    fn mixture_of_agents_refiners_each_read_every_proposal() {
        let p = build_pattern(PATTERN_MIXTURE_OF_AGENTS, 3).unwrap();
        let levels = p.levels().unwrap();
        assert_eq!(levels.len(), 3, "propose, refine, aggregate");
        assert_eq!(p.terminal_nodes(), vec!["aggregator".to_string()]);
        let refiner = p.nodes.iter().find(|n| n.name == "refiner_2").unwrap();
        assert_eq!(
            refiner.inputs.len(),
            3,
            "a refiner that reads only one proposal is just a fan-out stage"
        );
    }

    /// The catalog is what the chat route and both UIs dispatch from, so a
    /// pattern that builds but is not listed is unreachable.
    #[test]
    fn every_pattern_constant_is_listed_in_the_catalog() {
        let listed: Vec<&str> = pattern_catalog().iter().map(|(id, _)| *id).collect();
        for id in [
            PATTERN_CLASSIFY_AND_ACT, PATTERN_FANOUT_SYNTHESIZE, PATTERN_ADVERSARIAL_VERIFY,
            PATTERN_GENERATE_AND_FILTER, PATTERN_TOURNAMENT, PATTERN_LOOP_UNTIL_DONE,
            PATTERN_DEBATE, PATTERN_HIERARCHICAL, PATTERN_SEQUENTIAL_PIPELINE,
            PATTERN_MIXTURE_OF_AGENTS,
        ] {
            assert!(listed.contains(&id), "{id} is not reachable from any picker");
        }
    }

    /// The web UI keeps its own copy of the mode list in JavaScript, which is
    /// exactly the kind of thing that rots. A pattern missing from it is
    /// unreachable for every browser/remote user even though the backend
    /// supports it.
    #[test]
    fn the_web_ui_mode_list_covers_every_pattern() {
        const INDEX: &str = include_str!("../../../static/index.html");
        for (id, _) in pattern_catalog() {
            assert!(
                INDEX.contains(&format!("'{id}'")),
                "'{id}' is missing from agentModes in static/index.html — the backend \
                 supports it but no browser user can select it"
            );
        }
    }

    // --- handoff (conditional) edges --------------------------------------

    /// Collects the names of every node actually dispatched.
    fn recording_runner(
        reply: impl Fn(&str) -> Result<String, String> + Send + Sync + 'static,
    ) -> (
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        impl Fn(NodeRun) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>,
    ) {
        let ran = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let r = ran.clone();
        let reply = std::sync::Arc::new(reply);
        let runner = move |req: NodeRun| {
            let ran = r.clone();
            let reply = reply.clone();
            Box::pin(async move {
                ran.lock().unwrap().push(req.name.clone());
                reply(&req.name)
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        };
        (ran, runner)
    }

    #[tokio::test]
    async fn a_handoff_dispatches_only_the_branch_it_names() {
        let p = build_pattern(PATTERN_CLASSIFY_AND_ACT, 4).unwrap();
        let (ran, run) = recording_runner(|name| {
            Ok(if name == "classifier" {
                "branch_3 is best suited".to_string()
            } else {
                format!("out-{name}")
            })
        });

        let res = execute(&p, "T", run).await.unwrap();
        assert!(res.ok, "a branch that was not selected is not a failure");

        let ran = ran.lock().unwrap().clone();
        assert!(ran.contains(&"branch_3".to_string()), "the chosen branch must run: {ran:?}");
        for i in [1, 2, 4] {
            assert!(
                !ran.contains(&format!("branch_{i}")),
                "branch_{i} was passed over and must never be dispatched: {ran:?}"
            );
        }
        assert!(ran.contains(&"result".to_string()), "the collector still runs");
        assert_eq!(
            ran.len(),
            3,
            "routing to 1 of 4 should cost 3 calls (classifier, specialist, collector), not 6: {ran:?}"
        );

        let skipped: Vec<&str> =
            res.outcomes.iter().filter(|o| o.skipped).map(|o| o.name.as_str()).collect();
        assert_eq!(skipped, vec!["branch_1", "branch_2", "branch_4"]);
    }

    /// A model that ignores "name exactly one" must not silently take a branch
    /// at random, and must not stall the graph.
    #[tokio::test]
    async fn a_handoff_naming_nothing_falls_back_to_running_every_branch() {
        let p = build_pattern(PATTERN_CLASSIFY_AND_ACT, 3).unwrap();
        let (ran, run) = recording_runner(|name| {
            Ok(if name == "classifier" {
                "I am unable to decide.".to_string()
            } else {
                format!("out-{name}")
            })
        });

        let res = execute(&p, "T", run).await.unwrap();
        assert!(res.ok);
        let ran = ran.lock().unwrap().clone();
        for i in 1..=3 {
            assert!(ran.contains(&format!("branch_{i}")), "expected a safe fan-out: {ran:?}");
        }
        assert!(res.outcomes.iter().all(|o| !o.skipped));
    }

    #[tokio::test]
    async fn a_failed_handoff_leaves_every_branch_live() {
        let p = build_pattern(PATTERN_CLASSIFY_AND_ACT, 3).unwrap();
        let (ran, run) = recording_runner(|name| {
            if name == "classifier" {
                Err("boom".to_string())
            } else {
                Ok(format!("out-{name}"))
            }
        });

        let res = execute(&p, "T", run).await.unwrap();
        assert!(!res.ok, "the classifier really did fail");
        let ran = ran.lock().unwrap().clone();
        for i in 1..=3 {
            assert!(
                ran.contains(&format!("branch_{i}")),
                "losing the routing decision must cost money, not correctness: {ran:?}"
            );
        }
    }

    #[test]
    fn handoff_matching_prefers_the_longest_candidate_name() {
        let p = WorkflowProfile {
            nodes: vec![
                node("route", "", &[], ""),
                node("branch_1", "", &["route"], ""),
                node("branch_10", "", &["route"], ""),
            ],
            ..Default::default()
        };
        let candidates = vec![1usize, 2usize];
        assert_eq!(
            choose_handoff_target(&p, &candidates, "go with branch_10"),
            Some(2),
            "branch_1 must not shadow branch_10"
        );
        assert_eq!(choose_handoff_target(&p, &candidates, "go with branch_1"), Some(1));
        assert_eq!(choose_handoff_target(&p, &candidates, "no opinion"), None);
    }

    /// Skipping is transitive: a passed-over branch takes its own subtree with
    /// it, but a join node with one live parent still runs.
    #[tokio::test]
    async fn skipping_propagates_to_a_branchs_descendants_but_not_past_a_live_join() {
        let mut p = WorkflowProfile {
            nodes: vec![
                node("route", "", &[], ""),
                node("alpha", "", &["route"], ""),
                node("beta", "", &["route"], ""),
                node("alpha_child", "", &["alpha"], ""),
                node("beta_child", "", &["beta"], ""),
                node("end", "", &["alpha_child", "beta_child"], ""),
            ],
            ..Default::default()
        };
        p.nodes[0].handoff = true;

        let (ran, run) = recording_runner(|name| {
            Ok(if name == "route" { "choose alpha".to_string() } else { format!("out-{name}") })
        });

        let res = execute(&p, "T", run).await.unwrap();
        assert!(res.ok);

        let ran = ran.lock().unwrap().clone();
        assert_eq!(ran.len(), 4, "route, alpha, alpha_child, end: {ran:?}");
        assert!(ran.contains(&"alpha_child".to_string()));
        assert!(!ran.contains(&"beta".to_string()));
        assert!(!ran.contains(&"beta_child".to_string()), "the dead branch's subtree dies with it");
        assert!(ran.contains(&"end".to_string()), "a join with one live parent still runs");

        let skipped: Vec<&str> =
            res.outcomes.iter().filter(|o| o.skipped).map(|o| o.name.as_str()).collect();
        assert_eq!(skipped, vec!["beta", "beta_child"]);
    }

    // --- per-role model roster -------------------------------------------

    use crate::server::data::{model_for_role, ModelPoolEntry};

    fn entry(id: &str, model: &str, roles: &[&str]) -> ModelPoolEntry {
        ModelPoolEntry {
            id: id.into(),
            label: id.into(),
            model: model.into(),
            roles: roles.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_role_specific_entry_beats_a_wildcard_regardless_of_order() {
        // Wildcard listed first must not shadow the deliberate per-role pick.
        let pool = vec![entry("general", "cheap", &[]), entry("judges", "strong", &["judge"])];
        assert_eq!(model_for_role(&pool, "judge").unwrap().model, "strong");
        assert_eq!(model_for_role(&pool, "worker").unwrap().model, "cheap");
    }

    #[test]
    fn role_matching_ignores_case() {
        let pool = vec![entry("j", "strong", &["Judge"])];
        assert_eq!(model_for_role(&pool, "judge").unwrap().model, "strong");
    }

    #[test]
    fn no_match_and_no_wildcard_means_use_the_session_model() {
        let pool = vec![entry("j", "strong", &["judge"])];
        assert!(model_for_role(&pool, "worker").is_none());
        assert!(model_for_role(&[], "judge").is_none());
    }

    #[test]
    fn patterns_expose_roles_the_roster_can_target() {
        // The roster keys on role, so each pattern must actually label its
        // nodes — otherwise per-role model selection has nothing to bind to.
        for (name, _) in pattern_catalog() {
            let p = build_pattern(name, 3).unwrap();
            assert!(
                p.nodes.iter().all(|n| !n.role.is_empty()),
                "{name} has an unlabelled node"
            );
        }
    }
}
