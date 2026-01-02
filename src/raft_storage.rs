use crate::raft::LogEntry;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;

#[derive(Serialize, Deserialize)]
struct DiskLogEntry {
    term: u64,
    index: u64,
    command: Vec<u8>,
}

impl From<&LogEntry> for DiskLogEntry {
    fn from(e: &LogEntry) -> Self {
        Self {
            term: e.term,
            index: e.index,
            command: e.command.clone(),
        }
    }
}

impl Into<LogEntry> for DiskLogEntry {
    fn into(self) -> LogEntry {
        LogEntry {
            term: self.term,
            index: self.index,
            command: self.command,
        }
    }
}

// The metadata we must atomically save
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RaftMetadata {
    pub current_term: u64,
    pub voted_for: Option<u64>,
}

pub struct RaftStorage {
    metadata_path: String,
    log_path: String,
}

impl RaftStorage {
    pub fn new(node_id: u64) -> Self {
        Self {
            metadata_path: format!("node_{}_metadata.json", node_id),
            log_path: format!("node_{}_raft.log", node_id),
        }
    }

    // --- METADATA OPERATIONS ---

    pub fn save_metadata(&self, term: u64, voted_for: Option<u64>) -> io::Result<()> {
        let meta = RaftMetadata {
            current_term: term,
            voted_for,
        };
        let json = serde_json::to_string(&meta)?;
        fs::write(&self.metadata_path, json)
    }

    pub fn load_metadata(&self) -> io::Result<RaftMetadata> {
        if !Path::new(&self.metadata_path).exists() {
            return Ok(RaftMetadata {
                current_term: 0,
                voted_for: None,
            });
        }
        let content = fs::read_to_string(&self.metadata_path)?;
        let meta: RaftMetadata = serde_json::from_str(&content)?;
        Ok(meta)
    }

    // --- LOG OPERATIONS ---

    pub fn append_log_entry(&self, entry: &LogEntry) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)?;

        // Convert to DiskLogEntry before serializing
        let disk_entry = DiskLogEntry::from(entry);
        let json = serde_json::to_string(&disk_entry)?;
        writeln!(file, "{}", json)?;
        Ok(())
    }

    pub fn load_log(&self) -> io::Result<Vec<LogEntry>> {
        if !Path::new(&self.log_path).exists() {
            return Ok(Vec::new());
        }

        let file = fs::File::open(&self.log_path)?;
        let reader = BufReader::new(file);
        let mut logs = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if !line.is_empty() {
                // Deserialize to DiskLogEntry first
                let disk_entry: DiskLogEntry = serde_json::from_str(&line)?;
                logs.push(disk_entry.into());
            }
        }
        Ok(logs)
    }
}
