use crate::raft::raft_service_client::RaftServiceClient;
use crate::raft::raft_service_server::RaftService;
use crate::raft::{
    AppendEntriesRequest, AppendEntriesResponse, LogEntry, VoteRequest, VoteResponse,
};
use crate::raft_storage::RaftStorage;
use rand::Rng;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use tonic::{Request, Response, Status};

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum NodeState {
    Follower,
    Candidate,
    Leader,
}

#[derive(Debug)]
struct PeerState {
    next_index: u64,
    match_index: u64,
}

#[derive(Clone)]
pub struct RaftNode {
    // Persistent state
    pub current_term: Arc<Mutex<u64>>,
    pub voted_for: Arc<Mutex<Option<u64>>>,
    pub log: Arc<Mutex<Vec<LogEntry>>>,

    // Volatile state
    pub commit_index: Arc<Mutex<u64>>,
    pub state: Arc<Mutex<NodeState>>,

    // Configuration
    pub id: u64,
    pub last_heartbeat: Arc<Mutex<Instant>>,
    peers: Arc<Mutex<HashMap<String, PeerState>>>,

    // Channel to send committed commands to DB
    commit_tx: UnboundedSender<Vec<u8>>,

    // Storage Engine
    storage: Arc<RaftStorage>,
}

impl RaftNode {
    pub fn new(id: u64, peer_addrs: Vec<String>, commit_tx: UnboundedSender<Vec<u8>>) -> Self {
        let storage = RaftStorage::new(id);

        // 1. LOAD PERSISTENT STATE
        let (term, voted_for) = match storage.load_metadata() {
            Ok(m) => (m.current_term, m.voted_for),
            Err(e) => {
                eprintln!(
                    "Node {}: Failed to load metadata (starting fresh): {}",
                    id, e
                );
                (0, None)
            }
        };

        let loaded_log = match storage.load_log() {
            Ok(l) => l,
            Err(e) => {
                eprintln!("Node {}: Failed to load log: {}", id, e);
                Vec::new()
            }
        };

        println!(
            "Node {}: Loaded state -> Term: {}, VotedFor: {:?}, Log Size: {}",
            id,
            term,
            voted_for,
            loaded_log.len()
        );

        let mut peer_map = HashMap::new();
        for addr in peer_addrs {
            peer_map.insert(
                addr,
                PeerState {
                    next_index: 1,
                    match_index: 0,
                },
            );
        }

        Self {
            current_term: Arc::new(Mutex::new(term)),
            voted_for: Arc::new(Mutex::new(voted_for)),
            log: Arc::new(Mutex::new(loaded_log)),
            commit_index: Arc::new(Mutex::new(0)),
            state: Arc::new(Mutex::new(NodeState::Follower)),
            id,
            last_heartbeat: Arc::new(Mutex::new(Instant::now())),
            peers: Arc::new(Mutex::new(peer_map)),
            commit_tx,
            storage: Arc::new(storage),
        }
    }

    fn save_metadata(&self, new_term: u64, new_vote: Option<u64>) {
        let mut term_guard = self.current_term.lock().unwrap();
        let mut vote_guard = self.voted_for.lock().unwrap();

        *term_guard = new_term;
        *vote_guard = new_vote;

        if let Err(e) = self.storage.save_metadata(new_term, new_vote) {
            eprintln!("CRITICAL: Failed to save Raft metadata: {}", e);
        }
    }

    pub fn propose(&self, command: Vec<u8>) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        if *state != NodeState::Leader {
            return Err("Not Leader".to_string());
        }

        let mut log = self.log.lock().unwrap();
        let term = *self.current_term.lock().unwrap();
        let index = log.len() as u64 + 1;

        let entry = LogEntry {
            term,
            index,
            command,
        };

        if let Err(e) = self.storage.append_log_entry(&entry) {
            return Err(format!("Persistence failure: {}", e));
        }

        log.push(entry);
        println!(
            "Node {}: Appended entry at index {} (Term {})",
            self.id, index, term
        );
        Ok(())
    }

    pub async fn run_election_loop(&self) {
        loop {
            let state = *self.state.lock().unwrap();
            if state == NodeState::Leader {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }

            let timeout_ms = rand::thread_rng().gen_range(1000..1500);
            tokio::time::sleep(Duration::from_millis(timeout_ms)).await;

            let last_heartbeat = *self.last_heartbeat.lock().unwrap();
            if last_heartbeat.elapsed() > Duration::from_millis(timeout_ms) {
                self.start_election().await;
            }
        }
    }

    async fn start_election(&self) {
        println!("Node {}: Election Timeout! Starting campaign...", self.id);

        let mut term = 0;
        {
            let mut state = self.state.lock().unwrap();
            *state = NodeState::Candidate;

            let old_term = *self.current_term.lock().unwrap();
            let new_term = old_term + 1;

            self.save_metadata(new_term, Some(self.id));
            term = new_term;
        }

        let mut votes = 1;
        let peer_keys: Vec<String> = self.peers.lock().unwrap().keys().cloned().collect();
        let total = peer_keys.len() + 1;
        let majority = (total / 2) + 1;

        for addr in peer_keys {
            let url = format!("http://{}", addr);
            match RaftServiceClient::connect(url.clone()).await {
                Ok(mut client) => {
                    let req = VoteRequest {
                        term,
                        candidate_id: self.id,
                        last_log_index: 0,
                        last_log_term: 0,
                    };
                    match client.request_vote(req).await {
                        Ok(resp) => {
                            let r = resp.into_inner();
                            if r.vote_granted {
                                votes += 1;
                            } else if r.term > term {
                                println!(
                                    "Node {}: Peer {} has higher term {}, stepping down.",
                                    self.id, addr, r.term
                                );
                                self.become_follower(r.term);
                                return;
                            }
                        }
                        Err(e) => println!("Node {}: Vote RPC failed to {}: {}", self.id, addr, e),
                    }
                }
                Err(e) => println!("Node {}: Failed to connect to {}: {}", self.id, addr, e),
            }
        }

        if votes >= majority {
            println!(
                "Node {}: Won election with {} votes! Becoming LEADER.",
                self.id, votes
            );
            self.become_leader();
        } else {
            println!(
                "Node {}: Lost election (received {} votes).",
                self.id, votes
            );
        }
    }

    fn become_leader(&self) {
        {
            let mut state = self.state.lock().unwrap();
            *state = NodeState::Leader;

            let last_log_index = self.log.lock().unwrap().len() as u64;
            let mut peers = self.peers.lock().unwrap();
            for val in peers.values_mut() {
                val.next_index = last_log_index + 1;
                val.match_index = 0;
            }
        }

        println!("Node {}: Starting Heartbeat Loop...", self.id);
        let node = self.clone();
        tokio::spawn(async move {
            node.run_heartbeat_loop().await;
        });
    }

    fn become_follower(&self, term: u64) {
        let mut state = self.state.lock().unwrap();
        *state = NodeState::Follower;
        self.save_metadata(term, None);
    }

    async fn run_heartbeat_loop(&self) {
        loop {
            if *self.state.lock().unwrap() != NodeState::Leader {
                break;
            }

            let term = *self.current_term.lock().unwrap();
            let peer_keys: Vec<String> = self.peers.lock().unwrap().keys().cloned().collect();
            let log = self.log.lock().unwrap().clone();

            for addr in peer_keys {
                let node = self.clone();
                let addr_clone = addr.clone();
                let log_clone = log.clone();

                tokio::spawn(async move {
                    node.replicate_to_peer(addr_clone, term, log_clone).await;
                });
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn replicate_to_peer(&self, peer_addr: String, term: u64, log: Vec<LogEntry>) {
        let (prev_log_index, prev_log_term, entries_to_send) = {
            let mut peers = self.peers.lock().unwrap();
            let peer_state = peers.get_mut(&peer_addr).unwrap();

            let next_idx = peer_state.next_index as usize;
            let prev_idx = if next_idx > 1 { next_idx - 1 } else { 0 };
            let prev_term = 0;

            let slice = if next_idx <= log.len() {
                log[next_idx - 1..].to_vec()
            } else {
                Vec::new()
            };

            (prev_idx as u64, prev_term, slice)
        };

        let url = format!("http://{}", peer_addr);
        match RaftServiceClient::connect(url).await {
            Ok(mut client) => {
                let req = AppendEntriesRequest {
                    term,
                    leader_id: self.id,
                    prev_log_index,
                    prev_log_term,
                    leader_commit: *self.commit_index.lock().unwrap(),
                    entries: entries_to_send.clone(),
                };

                match client.append_entries(req).await {
                    Ok(resp) => {
                        let r = resp.into_inner();
                        if r.success {
                            let mut peers = self.peers.lock().unwrap();
                            if let Some(p) = peers.get_mut(&peer_addr) {
                                if !entries_to_send.is_empty() {
                                    let last_sent = entries_to_send.last().unwrap();
                                    p.next_index = last_sent.index + 1;
                                    p.match_index = last_sent.index;

                                    // Majority commit check
                                    let mut match_indices: Vec<u64> =
                                        peers.values().map(|p| p.match_index).collect();
                                    match_indices.push(self.log.lock().unwrap().len() as u64);
                                    match_indices.sort_unstable();
                                    let majority_idx = match_indices.len() / 2;
                                    let n = match_indices[majority_idx];

                                    let mut commit_index = self.commit_index.lock().unwrap();
                                    if n > *commit_index {
                                        *commit_index = n;
                                        println!("Node {}: Commit Index updated to {}", self.id, n);
                                        // SEND TO DB CHANNEL
                                        let log = self.log.lock().unwrap();
                                        if n > 0 && n <= log.len() as u64 {
                                            let cmd = log[(n - 1) as usize].command.clone();
                                            let _ = self.commit_tx.send(cmd);
                                        }
                                    }
                                }
                            }
                        } else if r.term > term {
                            println!("Node {}: Heartbeat rejected by {} (Higher Term {}), stepping down.", self.id, peer_addr, r.term);
                            self.become_follower(r.term);
                        }
                    }
                    Err(e) => {
                        // IMPORTANT: Log heartbeat failures so we know why Node 2 is timing out!
                        // println!("Node {}: Heartbeat RPC failed to {}: {}", self.id, peer_addr, e);
                    }
                }
            }
            Err(e) => {
                // IMPORTANT: Log connection failures!
                println!(
                    "Node {}: Heartbeat Connect failed to {}: {}",
                    self.id, peer_addr, e
                );
            }
        }
    }
}

// --- gRPC HANDLERS ---

#[tonic::async_trait]
impl RaftService for RaftNode {
    async fn request_vote(
        &self,
        request: Request<VoteRequest>,
    ) -> Result<Response<VoteResponse>, Status> {
        let req = request.into_inner();
        let mut current_term = self.current_term.lock().unwrap();
        let mut voted_for = self.voted_for.lock().unwrap();

        // UNCOMMENTED THIS LOG:
        println!(
            "Node {}: Vote request from {} for term {}",
            self.id, req.candidate_id, req.term
        );

        if req.term > *current_term {
            drop(voted_for);
            drop(current_term);
            self.save_metadata(req.term, None);

            current_term = self.current_term.lock().unwrap();
            voted_for = self.voted_for.lock().unwrap();
            *self.state.lock().unwrap() = NodeState::Follower;
        }

        let granted = if req.term < *current_term {
            false
        } else if voted_for.is_none() || *voted_for == Some(req.candidate_id) {
            drop(voted_for);
            drop(current_term);
            self.save_metadata(req.term, Some(req.candidate_id));

            current_term = self.current_term.lock().unwrap();
            voted_for = self.voted_for.lock().unwrap();

            *self.last_heartbeat.lock().unwrap() = Instant::now();
            true
        } else {
            false
        };

        Ok(Response::new(VoteResponse {
            term: *current_term,
            vote_granted: granted,
        }))
    }

    async fn append_entries(
        &self,
        request: Request<AppendEntriesRequest>,
    ) -> Result<Response<AppendEntriesResponse>, Status> {
        let req = request.into_inner();
        let mut current_term = self.current_term.lock().unwrap();

        if req.term < *current_term {
            return Ok(Response::new(AppendEntriesResponse {
                term: *current_term,
                success: false,
            }));
        }

        if req.term >= *current_term {
            if req.term > *current_term {
                drop(current_term);
                self.save_metadata(req.term, None);
                current_term = self.current_term.lock().unwrap();
            }

            *self.state.lock().unwrap() = NodeState::Follower;
            *self.last_heartbeat.lock().unwrap() = Instant::now();
        }

        let mut log = self.log.lock().unwrap();
        for entry in req.entries {
            if entry.index > log.len() as u64 {
                if let Err(e) = self.storage.append_log_entry(&entry) {
                    eprintln!("Failed to persist log: {}", e);
                }
                log.push(entry);
            }
        }

        let mut commit_index = self.commit_index.lock().unwrap();
        if req.leader_commit > *commit_index {
            let old_commit = *commit_index;
            *commit_index = req.leader_commit.min(log.len() as u64);

            if *commit_index > old_commit {
                println!(
                    "Node {}: Follower Commit Index updated to {}",
                    self.id, *commit_index
                );
                let idx = *commit_index;
                if idx > 0 && idx <= log.len() as u64 {
                    let cmd = log[(idx - 1) as usize].command.clone();
                    let _ = self.commit_tx.send(cmd);
                }
            }
        }

        Ok(Response::new(AppendEntriesResponse {
            term: *current_term,
            success: true,
        }))
    }
}
