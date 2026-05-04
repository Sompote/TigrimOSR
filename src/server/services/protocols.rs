/// Inter-agent communication protocols: TCP, Bus, Queue, Blackboard
///
/// Direct Rust port of tiger_cowork/server/services/protocols.ts
///
///  - TCP:        Point-to-point bidirectional channel via tokio TCP on localhost
///  - Bus:        In-process pub/sub event bus (topics) with history
///  - Queue:      FIFO message queue with persistence per channel
///  - Blackboard: Shared workspace for P2P/Contract-Net task coordination

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex as TokioMutex, Notify};
use tracing::{info, warn};

// ─── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolMessage {
    pub from: String,
    pub to: Option<String>,
    pub topic: String,
    pub payload: Value,
    pub timestamp: String,
}

impl ProtocolMessage {
    pub fn now(from: &str, to: Option<&str>, topic: &str, payload: Value) -> Self {
        Self {
            from: from.to_string(),
            to: to.map(|s| s.to_string()),
            topic: topic.to_string(),
            payload,
            timestamp: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        }
    }
}

// ─── TCP Protocol ─────────────────────────────────────────────────────────────
// Each agent-pair gets an ephemeral localhost TCP server.

struct TcpChannel {
    port: u16,
    buffer: Vec<ProtocolMessage>,
    session_id: Option<String>,
    /// Broadcast sender — every accepted client gets a receiver clone
    tx: broadcast::Sender<String>,
}

fn tcp_channel_key(a: &str, b: &str) -> String {
    let mut v = vec![a, b];
    v.sort_unstable();
    format!("{}<->{}", v[0], v[1])
}

type TcpChannels = TokioMutex<HashMap<String, TcpChannel>>;

fn tcp_channels() -> &'static TcpChannels {
    static INSTANCE: OnceLock<TcpChannels> = OnceLock::new();
    INSTANCE.get_or_init(|| TokioMutex::new(HashMap::new()))
}

/// Open a bidirectional TCP channel between two agents. Returns (port, channel_id).
pub async fn tcp_open(
    agent_a: &str,
    agent_b: &str,
    session_id: Option<&str>,
) -> Result<(u16, String), String> {
    let key = tcp_channel_key(agent_a, agent_b);
    {
        let channels = tcp_channels().lock().await;
        if let Some(ch) = channels.get(&key) {
            return Ok((ch.port, key));
        }
    }

    // Bind to a random ephemeral port on localhost
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("TCP bind failed: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| e.to_string())?
        .port();

    let (tx, _) = broadcast::channel::<String>(512);
    let tx_clone = tx.clone();
    let key_clone = key.clone();

    // Spawn a task that accepts connections and relays messages
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((socket, _)) => {
                    let tx2 = tx_clone.clone();
                    let key2 = key_clone.clone();
                    tokio::spawn(async move {
                        handle_tcp_connection(socket, tx2, key2).await;
                    });
                }
                Err(e) => {
                    warn!("[Protocol:TCP] Accept error on {}: {e}", key_clone);
                    break;
                }
            }
        }
    });

    let channel = TcpChannel {
        port,
        buffer: Vec::new(),
        session_id: session_id.map(|s| s.to_string()),
        tx,
    };

    tcp_channels().lock().await.insert(key.clone(), channel);
    info!("[Protocol:TCP] Channel {} opened on port {}", key, port);
    Ok((port, key))
}

async fn handle_tcp_connection(
    socket: TcpStream,
    tx: broadcast::Sender<String>,
    key: String,
) {
    let (reader, mut writer) = socket.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut rx = tx.subscribe();

    // Forward incoming broadcast messages to this client
    let writer_task = tokio::spawn(async move {
        while let Ok(line) = rx.recv().await {
            let _ = writer.write_all(format!("{line}\n").as_bytes()).await;
        }
    });

    // Read incoming lines, buffer them, and rebroadcast
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(mut msg) = serde_json::from_str::<ProtocolMessage>(&line) {
            if msg.timestamp.is_empty() {
                msg.timestamp = chrono::Utc::now()
                    .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                    .to_string();
            }
            // Store in channel buffer
            let serialized = serde_json::to_string(&msg).unwrap_or_default();
            {
                let mut channels = tcp_channels().lock().await;
                if let Some(ch) = channels.get_mut(&key) {
                    ch.buffer.push(msg);
                    // Persist to agent history if session available
                    if let Some(sid) = &ch.session_id {
                        let _ = append_agent_history(sid, "tcp.jsonl", &serialized).await;
                    }
                }
            }
            // Broadcast to other clients (they will receive it through their writer task)
            let _ = tx.send(serialized);
        }
    }

    writer_task.abort();
}

/// Send a message via TCP to another agent (connects as client, sends, disconnects).
pub async fn tcp_send(
    from: &str,
    to: &str,
    topic: &str,
    payload: Value,
) -> bool {
    let key = tcp_channel_key(from, to);
    let port = {
        let channels = tcp_channels().lock().await;
        channels.get(&key).map(|ch| ch.port)
    };
    let port = match port {
        Some(p) => p,
        None => {
            warn!("[Protocol:TCP] Channel {} not open", key);
            return false;
        }
    };

    let msg = ProtocolMessage::now(from, Some(to), topic, payload);
    let line = match serde_json::to_string(&msg) {
        Ok(s) => s,
        Err(_) => return false,
    };

    match TcpStream::connect(format!("127.0.0.1:{port}")).await {
        Ok(mut stream) => {
            let _ = stream
                .write_all(format!("{line}\n").as_bytes())
                .await;
            true
        }
        Err(e) => {
            warn!("[Protocol:TCP] Send failed to {}: {e}", key);
            false
        }
    }
}

/// Read all buffered messages from a TCP channel.
pub async fn tcp_read(agent_a: &str, agent_b: &str) -> Vec<ProtocolMessage> {
    let key = tcp_channel_key(agent_a, agent_b);
    let channels = tcp_channels().lock().await;
    channels
        .get(&key)
        .map(|ch| ch.buffer.clone())
        .unwrap_or_default()
}

/// Close a TCP channel.
pub async fn tcp_close(agent_a: &str, agent_b: &str) {
    let key = tcp_channel_key(agent_a, agent_b);
    if tcp_channels().lock().await.remove(&key).is_some() {
        info!("[Protocol:TCP] Channel {} closed", key);
    }
}

// ─── Bus Protocol ─────────────────────────────────────────────────────────────

const BUS_MAX_HISTORY: usize = 500;

pub struct AgentBus {
    history: Vec<ProtocolMessage>,
    /// Indices (into history) that have been consumed via consumeFromHistory
    consumed_idx: std::collections::HashSet<usize>,
    /// Next history offset — monotonically increasing, used to detect new messages
    next_offset: usize,
    /// Broadcast channel for live subscriptions
    tx: broadcast::Sender<ProtocolMessage>,
    /// Notify for busWaitForMessage
    notify: Arc<Notify>,
}

impl AgentBus {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            history: Vec::new(),
            consumed_idx: std::collections::HashSet::new(),
            next_offset: 0,
            tx,
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn publish(&mut self, msg: ProtocolMessage) {
        if self.history.len() >= BUS_MAX_HISTORY {
            // Remove oldest: shift consumed_idx
            self.history.remove(0);
            // Adjust consumed indices down by 1
            self.consumed_idx = self.consumed_idx
                .iter()
                .filter_map(|&i| if i == 0 { None } else { Some(i - 1) })
                .collect();
            if self.next_offset > 0 {
                self.next_offset -= 1;
            }
        }
        self.history.push(msg.clone());
        let _ = self.tx.send(msg);
        self.notify.notify_waiters();
    }

    /// Return the first unconsumed message on this topic (FIFO), marking it consumed.
    pub fn consume_from_history(&mut self, topic: &str) -> Option<ProtocolMessage> {
        for (i, msg) in self.history.iter().enumerate() {
            if msg.topic == topic && !self.consumed_idx.contains(&i) {
                self.consumed_idx.insert(i);
                return Some(msg.clone());
            }
        }
        None
    }

    pub fn get_history(&self, topic: Option<&str>) -> Vec<ProtocolMessage> {
        match topic {
            Some(t) => self.history.iter().filter(|m| m.topic == t).cloned().collect(),
            None => self.history.clone(),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ProtocolMessage> {
        self.tx.subscribe()
    }

    pub fn notify_handle(&self) -> Arc<Notify> {
        self.notify.clone()
    }
}

type BusInstances = TokioMutex<HashMap<String, AgentBus>>;

fn bus_instances() -> &'static BusInstances {
    static INSTANCE: OnceLock<BusInstances> = OnceLock::new();
    INSTANCE.get_or_init(|| TokioMutex::new(HashMap::new()))
}

pub async fn bus_get_notify(session_id: &str) -> Arc<Notify> {
    let mut buses = bus_instances().lock().await;
    buses
        .entry(session_id.to_string())
        .or_insert_with(AgentBus::new)
        .notify_handle()
}

pub async fn bus_publish(session_id: &str, from: &str, topic: &str, payload: Value) {
    let msg = ProtocolMessage::now(from, None, topic, payload.clone());
    {
        let mut buses = bus_instances().lock().await;
        buses
            .entry(session_id.to_string())
            .or_insert_with(AgentBus::new)
            .publish(msg.clone());
    }
    // Persist
    if let Ok(s) = serde_json::to_string(&msg) {
        let _ = append_agent_history(session_id, "bus.jsonl", &s).await;
    }
}

pub async fn bus_history(session_id: &str, topic: Option<&str>) -> Vec<ProtocolMessage> {
    let buses = bus_instances().lock().await;
    buses
        .get(session_id)
        .map(|b| b.get_history(topic))
        .unwrap_or_default()
}

pub async fn bus_consume_from_history(
    session_id: &str,
    topic: &str,
) -> Option<ProtocolMessage> {
    let mut buses = bus_instances().lock().await;
    buses
        .entry(session_id.to_string())
        .or_insert_with(AgentBus::new)
        .consume_from_history(topic)
}

/// Wait for the next message on a bus topic.
/// Checks history first (pick up messages that arrived while nobody was waiting).
pub async fn bus_wait_for_message(
    session_id: &str,
    topic: &str,
    timeout_secs: u64,
) -> Option<ProtocolMessage> {
    // Check history first
    if let Some(msg) = bus_consume_from_history(session_id, topic).await {
        return Some(msg);
    }

    // Subscribe to live messages
    let (mut rx, _notify) = {
        let buses = bus_instances().lock().await;
        let bus = match buses.get(session_id) {
            Some(b) => b,
            None => return None,
        };
        (bus.subscribe(), bus.notify_handle())
    };

    let deadline = tokio::time::Instant::now()
        + tokio::time::Duration::from_secs(if timeout_secs == 0 { 120 } else { timeout_secs });

    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(msg) if msg.topic == topic => return Some(msg),
                    Ok(_) => continue,
                    Err(_) => return None,
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                // Check history one more time before timing out
                return bus_consume_from_history(session_id, topic).await;
            }
        }
    }
}

/// Wait for a message on any of the given topics.
pub async fn bus_wait_for_any(
    session_id: &str,
    topics: &[(&str, &str)], // (topic, kind)
    timeout_secs: u64,
) -> Option<(String, ProtocolMessage)> {
    // Priority scan of history first
    {
        let mut buses = bus_instances().lock().await;
        if let Some(bus) = buses.get_mut(session_id) {
            for (topic, kind) in topics {
                if let Some(msg) = bus.consume_from_history(topic) {
                    return Some((kind.to_string(), msg));
                }
            }
        }
    }

    // Subscribe and race
    let (mut rx, _notify) = {
        let buses = bus_instances().lock().await;
        let bus = match buses.get(session_id) {
            Some(b) => b,
            None => return None,
        };
        (bus.subscribe(), bus.notify_handle())
    };

    let topic_set: Vec<(String, String)> = topics
        .iter()
        .map(|(t, k)| (t.to_string(), k.to_string()))
        .collect();

    let deadline = tokio::time::Instant::now()
        + tokio::time::Duration::from_secs(if timeout_secs == 0 { 120 } else { timeout_secs });

    loop {
        tokio::select! {
            result = rx.recv() => {
                match result {
                    Ok(msg) => {
                        if let Some((_, kind)) = topic_set.iter().find(|(t, _)| t == &msg.topic) {
                            return Some((kind.clone(), msg));
                        }
                        continue;
                    }
                    Err(_) => return None,
                }
            }
            _ = tokio::time::sleep_until(deadline) => return None,
        }
    }
}

pub async fn bus_destroy(session_id: &str) {
    if bus_instances().lock().await.remove(session_id).is_some() {
        info!("[Protocol:Bus] Destroyed bus for session {}", session_id);
    }
}

// ─── Queue Protocol ────────────────────────────────────────────────────────────

fn queue_key(from: &str, to: &str, topic: Option<&str>) -> String {
    match topic {
        Some(t) => format!("{}->{}:{}", from, to, t),
        None => format!("{}->{}", from, to),
    }
}

const QUEUE_MAX_SIZE: usize = 200;

type Queues = TokioMutex<HashMap<String, VecDeque<ProtocolMessage>>>;

fn queues() -> &'static Queues {
    static INSTANCE: OnceLock<Queues> = OnceLock::new();
    INSTANCE.get_or_init(|| TokioMutex::new(HashMap::new()))
}

pub async fn queue_enqueue(
    from: &str,
    to: &str,
    topic: &str,
    payload: Value,
    session_id: Option<&str>,
) -> usize {
    let key = queue_key(from, to, Some(topic));
    let msg = ProtocolMessage::now(from, Some(to), topic, payload.clone());
    let depth = {
        let mut qs = queues().lock().await;
        let q = qs.entry(key.clone()).or_insert_with(VecDeque::new);
        q.push_back(msg.clone());
        if q.len() > QUEUE_MAX_SIZE {
            q.pop_front();
        }
        q.len()
    };
    if let Some(sid) = session_id {
        if let Ok(s) = serde_json::to_string(&msg) {
            let _ = append_agent_history(sid, "queue.jsonl", &s).await;
        }
    }
    info!("[Protocol:Queue] Enqueued {} (depth={})", key, depth);
    depth
}

pub async fn queue_dequeue(
    from: &str,
    to: &str,
    topic: Option<&str>,
) -> Option<ProtocolMessage> {
    let key = queue_key(from, to, topic);
    let mut qs = queues().lock().await;
    qs.get_mut(&key)?.pop_front()
}

pub async fn queue_peek(
    from: &str,
    to: &str,
    topic: Option<&str>,
    count: usize,
) -> Vec<ProtocolMessage> {
    let key = queue_key(from, to, topic);
    let qs = queues().lock().await;
    qs.get(&key)
        .map(|q| q.iter().take(count).cloned().collect())
        .unwrap_or_default()
}

pub async fn queue_depth(from: &str, to: &str, topic: Option<&str>) -> usize {
    let key = queue_key(from, to, topic);
    queues().lock().await.get(&key).map(|q| q.len()).unwrap_or(0)
}

pub async fn queue_drain(from: &str, to: &str, topic: Option<&str>) -> Vec<ProtocolMessage> {
    let key = queue_key(from, to, topic);
    let mut qs = queues().lock().await;
    match qs.get_mut(&key) {
        Some(q) => q.drain(..).collect(),
        None => vec![],
    }
}

// ─── Blackboard Protocol (P2P / Contract Net) ────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlackboardBid {
    pub agent_id: String,
    pub confidence: f64,
    pub cost: Option<f64>,
    pub reasoning: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlackboardVote {
    pub agent_id: String,
    pub vote: String, // "approve" | "reject" | "abstain"
    pub weight: Option<f64>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlackboardTask {
    pub task_id: String,
    pub description: String,
    pub status: String, // "open" | "bidding" | "awarded" | "in_progress" | "completed" | "failed"
    pub proposed_by: String,
    pub proposed_at: String,
    pub bids: Vec<BlackboardBid>,
    pub awarded_to: Option<String>,
    pub awarded_at: Option<String>,
    pub result: Option<Value>,
    pub completed_at: Option<String>,
    pub votes: Vec<BlackboardVote>,
    pub consensus_mechanism: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlackboardEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub entry_type: String, // "proposal" | "bid" | "vote" | "result" | "status"
    pub task_id: String,
    pub agent_id: String,
    pub timestamp: String,
    pub payload: Value,
}

pub struct Blackboard {
    tasks: HashMap<String, BlackboardTask>,
    log: Vec<BlackboardEntry>,
    task_counter: usize,
}

impl Blackboard {
    fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            log: Vec::new(),
            task_counter: 0,
        }
    }

    fn now() -> String {
        chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    fn append_log(&mut self, entry: BlackboardEntry) {
        self.log.push(entry);
        if self.log.len() > 1000 {
            self.log.remove(0);
        }
    }

    /// Propose a new task. Returns (task, skipped).
    pub fn propose(
        &mut self,
        agent_id: &str,
        description: &str,
        task_id: Option<&str>,
    ) -> (BlackboardTask, bool) {
        self.task_counter += 1;
        let id = task_id
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("T{}_{}", self.task_counter, &Self::now()[..10]));

        // Guard: don't re-propose completed/in_progress tasks
        if let Some(existing) = self.tasks.get(&id) {
            match existing.status.as_str() {
                "completed" => {
                    info!("[Blackboard] Task \"{}\" already completed — skipping", id);
                    return (existing.clone(), true);
                }
                "in_progress" | "awarded" => {
                    info!("[Blackboard] Task \"{}\" already {} — skipping", id, existing.status);
                    return (existing.clone(), true);
                }
                "bidding" | "open" => {
                    // Allow re-broadcast — clone before the borrow ends
                    let existing_clone = existing.clone();
                    self.append_log(BlackboardEntry {
                        id: format!("e_{}", chrono::Utc::now().timestamp_millis()),
                        entry_type: "proposal".to_string(),
                        task_id: id.clone(),
                        agent_id: agent_id.to_string(),
                        timestamp: Self::now(),
                        payload: json!({ "description": description, "rebroadcast": true }),
                    });
                    return (existing_clone, false);
                }
                "failed" => {
                    if let Some(t) = self.tasks.get_mut(&id) {
                        t.status = "open".to_string();
                        t.bids.clear();
                        t.awarded_to = None;
                        t.awarded_at = None;
                        t.result = None;
                        t.completed_at = None;
                    }
                    let t = self.tasks.get(&id).unwrap().clone();
                    return (t, false);
                }
                _ => {}
            }
        }

        let ts = Self::now();
        let task = BlackboardTask {
            task_id: id.clone(),
            description: description.to_string(),
            status: "open".to_string(),
            proposed_by: agent_id.to_string(),
            proposed_at: ts.clone(),
            bids: Vec::new(),
            awarded_to: None,
            awarded_at: None,
            result: None,
            completed_at: None,
            votes: Vec::new(),
            consensus_mechanism: None,
        };
        self.tasks.insert(id.clone(), task.clone());
        self.append_log(BlackboardEntry {
            id: format!("e_{}", chrono::Utc::now().timestamp_millis()),
            entry_type: "proposal".to_string(),
            task_id: id,
            agent_id: agent_id.to_string(),
            timestamp: ts,
            payload: json!({ "description": description }),
        });
        (task, false)
    }

    /// Submit a bid.
    pub fn bid(
        &mut self,
        agent_id: &str,
        task_id: &str,
        confidence: f64,
        cost: Option<f64>,
        reasoning: Option<&str>,
    ) -> Result<(), String> {
        let task = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("Task \"{task_id}\" not found"))?;
        if task.status != "open" && task.status != "bidding" {
            return Err(format!(
                "Task \"{task_id}\" is {}, not accepting bids",
                task.status
            ));
        }
        if task.bids.iter().any(|b| b.agent_id == agent_id) {
            return Err(format!(
                "Agent \"{agent_id}\" already bid on \"{task_id}\""
            ));
        }
        let ts = Self::now();
        task.status = "bidding".to_string();
        task.bids.push(BlackboardBid {
            agent_id: agent_id.to_string(),
            confidence,
            cost,
            reasoning: reasoning.map(|s| s.to_string()),
            timestamp: ts.clone(),
        });
        self.append_log(BlackboardEntry {
            id: format!("e_{}", chrono::Utc::now().timestamp_millis()),
            entry_type: "bid".to_string(),
            task_id: task_id.to_string(),
            agent_id: agent_id.to_string(),
            timestamp: ts,
            payload: json!({ "confidence": confidence, "cost": cost, "reasoning": reasoning }),
        });
        Ok(())
    }

    /// Award a task to the best bidder (or a specified agent).
    pub fn award(
        &mut self,
        task_id: &str,
        award_to: Option<&str>,
        mechanism: Option<&str>,
    ) -> Result<String, String> {
        let task = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("Task \"{task_id}\" not found"))?;
        if task.bids.is_empty() {
            return Err(format!("No bids on task \"{task_id}\""));
        }

        let winner = if let Some(forced) = award_to {
            if !task.bids.iter().any(|b| b.agent_id == forced) {
                return Err(format!(
                    "Agent \"{forced}\" did not bid on \"{task_id}\""
                ));
            }
            forced.to_string()
        } else {
            task.bids
                .iter()
                .max_by(|a, b| a.confidence.partial_cmp(&b.confidence).unwrap())
                .map(|b| b.agent_id.clone())
                .unwrap()
        };

        let ts = Self::now();
        task.awarded_to = Some(winner.clone());
        task.awarded_at = Some(ts.clone());
        task.status = "awarded".to_string();
        task.consensus_mechanism = Some(mechanism.unwrap_or("weighted_max").to_string());

        let bids_info: Vec<Value> = task
            .bids
            .iter()
            .map(|b| json!({ "agent": b.agent_id, "confidence": b.confidence }))
            .collect();
        self.append_log(BlackboardEntry {
            id: format!("e_{}", chrono::Utc::now().timestamp_millis()),
            entry_type: "status".to_string(),
            task_id: task_id.to_string(),
            agent_id: winner.clone(),
            timestamp: ts,
            payload: json!({
                "event": "BID_ACCEPTED",
                "awarded_to": winner,
                "competing_bids": bids_info,
                "consensus_mechanism": mechanism.unwrap_or("weighted_max"),
            }),
        });
        Ok(winner)
    }

    /// Mark task as in_progress.
    pub fn start_task(&mut self, agent_id: &str, task_id: &str) -> Result<(), String> {
        let task = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("Task \"{task_id}\" not found"))?;
        if task.awarded_to.as_deref() != Some(agent_id) {
            return Err(format!(
                "Task \"{task_id}\" not awarded to \"{agent_id}\""
            ));
        }
        task.status = "in_progress".to_string();
        self.append_log(BlackboardEntry {
            id: format!("e_{}", chrono::Utc::now().timestamp_millis()),
            entry_type: "status".to_string(),
            task_id: task_id.to_string(),
            agent_id: agent_id.to_string(),
            timestamp: Self::now(),
            payload: json!({ "event": "TASK_STARTED" }),
        });
        Ok(())
    }

    /// Complete a task with a result.
    pub fn complete_task(
        &mut self,
        agent_id: &str,
        task_id: &str,
        result: Value,
    ) -> Result<(), String> {
        let task = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("Task \"{task_id}\" not found"))?;
        let ts = Self::now();
        task.status = "completed".to_string();
        task.result = Some(result.clone());
        task.completed_at = Some(ts.clone());
        self.append_log(BlackboardEntry {
            id: format!("e_{}", chrono::Utc::now().timestamp_millis()),
            entry_type: "result".to_string(),
            task_id: task_id.to_string(),
            agent_id: agent_id.to_string(),
            timestamp: ts,
            payload: json!({ "result": result }),
        });
        Ok(())
    }

    pub fn get_task(&self, task_id: &str) -> Option<&BlackboardTask> {
        self.tasks.get(task_id)
    }

    pub fn get_tasks(&self, status: Option<&str>) -> Vec<&BlackboardTask> {
        match status {
            Some(s) => self.tasks.values().filter(|t| t.status == s).collect(),
            None => self.tasks.values().collect(),
        }
    }

    pub fn get_log(&self, limit: Option<usize>) -> Vec<&BlackboardEntry> {
        match limit {
            Some(n) => {
                let start = self.log.len().saturating_sub(n);
                self.log[start..].iter().collect()
            }
            None => self.log.iter().collect(),
        }
    }

    pub fn clear(&mut self) {
        self.tasks.clear();
        self.log.clear();
        self.task_counter = 0;
    }
}

type Blackboards = TokioMutex<HashMap<String, Blackboard>>;

fn blackboards() -> &'static Blackboards {
    static INSTANCE: OnceLock<Blackboards> = OnceLock::new();
    INSTANCE.get_or_init(|| TokioMutex::new(HashMap::new()))
}

pub async fn blackboard_propose(
    session_id: &str,
    agent_id: &str,
    description: &str,
    task_id: Option<&str>,
) -> (BlackboardTask, bool) {
    let mut bbs = blackboards().lock().await;
    bbs.entry(session_id.to_string())
        .or_insert_with(Blackboard::new)
        .propose(agent_id, description, task_id)
}

pub async fn blackboard_bid(
    session_id: &str,
    agent_id: &str,
    task_id: &str,
    confidence: f64,
    cost: Option<f64>,
    reasoning: Option<&str>,
) -> Result<(), String> {
    let mut bbs = blackboards().lock().await;
    bbs.entry(session_id.to_string())
        .or_insert_with(Blackboard::new)
        .bid(agent_id, task_id, confidence, cost, reasoning)
}

pub async fn blackboard_award(
    session_id: &str,
    task_id: &str,
    award_to: Option<&str>,
    mechanism: Option<&str>,
) -> Result<String, String> {
    let mut bbs = blackboards().lock().await;
    bbs.entry(session_id.to_string())
        .or_insert_with(Blackboard::new)
        .award(task_id, award_to, mechanism)
}

pub async fn blackboard_start_task(
    session_id: &str,
    agent_id: &str,
    task_id: &str,
) -> Result<(), String> {
    let mut bbs = blackboards().lock().await;
    bbs.entry(session_id.to_string())
        .or_insert_with(Blackboard::new)
        .start_task(agent_id, task_id)
}

pub async fn blackboard_complete_task(
    session_id: &str,
    agent_id: &str,
    task_id: &str,
    result: Value,
) -> Result<(), String> {
    let mut bbs = blackboards().lock().await;
    bbs.entry(session_id.to_string())
        .or_insert_with(Blackboard::new)
        .complete_task(agent_id, task_id, result)
}

pub async fn blackboard_get_task(
    session_id: &str,
    task_id: &str,
) -> Option<BlackboardTask> {
    let bbs = blackboards().lock().await;
    bbs.get(session_id)?.get_task(task_id).cloned()
}

pub async fn blackboard_get_tasks(
    session_id: &str,
    status: Option<&str>,
) -> Vec<BlackboardTask> {
    let bbs = blackboards().lock().await;
    match bbs.get(session_id) {
        Some(bb) => bb.get_tasks(status).into_iter().cloned().collect(),
        None => vec![],
    }
}

pub async fn blackboard_get_log(
    session_id: &str,
    limit: Option<usize>,
) -> Vec<BlackboardEntry> {
    let bbs = blackboards().lock().await;
    match bbs.get(session_id) {
        Some(bb) => bb.get_log(limit).into_iter().cloned().collect(),
        None => vec![],
    }
}

pub async fn blackboard_destroy(session_id: &str) {
    if blackboards().lock().await.remove(session_id).is_some() {
        info!("[Protocol:Blackboard] Destroyed blackboard for session {}", session_id);
    }
}

// ─── Cleanup ──────────────────────────────────────────────────────────────────

pub async fn cleanup_session_protocols(session_id: &str) {
    bus_destroy(session_id).await;
    blackboard_destroy(session_id).await;
    // Close TCP channels that belong to this session
    let keys_to_remove: Vec<String> = {
        let channels = tcp_channels().lock().await;
        channels
            .iter()
            .filter(|(_, ch)| ch.session_id.as_deref() == Some(session_id))
            .map(|(k, _)| k.clone())
            .collect()
    };
    let mut channels = tcp_channels().lock().await;
    for key in keys_to_remove {
        if channels.remove(&key).is_some() {
            info!("[Protocol:TCP] Closed channel {} (session cleanup)", key);
        }
    }
}

// ─── Status / Debug ───────────────────────────────────────────────────────────

pub async fn get_protocol_status() -> Value {
    let tcp_info: Vec<Value> = {
        let channels = tcp_channels().lock().await;
        channels
            .iter()
            .map(|(id, ch)| {
                json!({ "id": id, "port": ch.port, "buffered": ch.buffer.len() })
            })
            .collect()
    };
    let bus_info: Vec<Value> = {
        let buses = bus_instances().lock().await;
        buses
            .iter()
            .map(|(session, bus)| {
                json!({ "session": session, "history": bus.history.len() })
            })
            .collect()
    };
    let queue_info: Vec<Value> = {
        let qs = queues().lock().await;
        qs.iter()
            .map(|(id, q)| json!({ "id": id, "depth": q.len() }))
            .collect()
    };
    let bb_info: Vec<Value> = {
        let bbs = blackboards().lock().await;
        bbs.iter()
            .map(|(session, bb)| {
                json!({ "session": session, "tasks": bb.tasks.len(), "log": bb.log.len() })
            })
            .collect()
    };

    json!({
        "tcp": { "channels": tcp_info.len(), "details": tcp_info },
        "bus": { "sessions": bus_info.len(), "details": bus_info },
        "queue": { "channels": queue_info.len(), "details": queue_info },
        "blackboard": { "sessions": bb_info.len(), "details": bb_info },
    })
}

// ─── Agent history persistence helper ────────────────────────────────────────

async fn append_agent_history(session_id: &str, filename: &str, line: &str) -> std::io::Result<()> {
    let dir = crate::server::data::data_dir().join("agent_history").join(session_id);
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join(filename);
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?;
    use tokio::io::AsyncWriteExt;
    f.write_all(format!("{}\n", line).as_bytes()).await
}
