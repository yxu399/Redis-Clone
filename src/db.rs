use crate::aof::Aof;
use crate::codec::RespCodec;
use crate::frame::Frame;
use bytes::Bytes;
use futures::StreamExt;
use lru::LruCache;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime}; // <--- Need Duration
use tokio::fs::File;
use tokio_util::codec::FramedRead;

const SHARD_COUNT: usize = 16;
const SHARD_CAPACITY: usize = 10000;

struct DbShard {
    storage: LruCache<String, Bytes>,
    expirations: HashMap<String, SystemTime>,
}

impl DbShard {
    fn new(capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity).unwrap();
        Self {
            storage: LruCache::new(cap),
            expirations: HashMap::new(),
        }
    }
}

#[derive(Clone)]
pub struct Db {
    shards: Arc<Vec<Mutex<DbShard>>>,
    aof: Arc<Aof>,
}

impl Db {
    pub async fn new(aof_path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let mut shards = Vec::with_capacity(SHARD_COUNT);
        for _ in 0..SHARD_COUNT {
            shards.push(Mutex::new(DbShard::new(SHARD_CAPACITY)));
        }

        // CREATE AOF BUT DON'T START BACKGROUND TASK YET
        let (aof, activator) = Aof::new_inactive(&aof_path).await?;

        let db = Self {
            shards: Arc::new(shards),
            aof: Arc::new(aof),
        };

        // RECOVER FIRST (AOF won't write during this)
        db.recover(&aof_path).await?;

        // NOW activate the background writer
        activator.activate();

        // Start background cleaner
        let db_clone = db.clone();
        tokio::spawn(async move {
            db_clone.background_purge_task().await;
        });

        Ok(db)
    }

    fn get_shard_index(&self, key: &str) -> usize {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        (hasher.finish() as usize) % SHARD_COUNT
    }

    pub fn set(&self, key: String, value: Bytes) {
        self.set_inner(key.clone(), value.clone());

        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from("SET")),
            Frame::Bulk(Bytes::from(key)),
            Frame::Bulk(value),
        ]);
        self.aof.log(frame);
    }

    fn set_inner(&self, key: String, value: Bytes) {
        let idx = self.get_shard_index(&key);
        let mut shard = self.shards[idx].lock().unwrap();
        shard.storage.put(key.clone(), value);
        shard.expirations.remove(&key);
    }

    pub fn set_expires(&self, key: String, duration: Duration) -> bool {
        let idx = self.get_shard_index(&key);
        let mut shard = self.shards[idx].lock().unwrap();

        if shard.storage.contains(&key) {
            let deadline = SystemTime::now() + duration;
            shard.expirations.insert(key.clone(), deadline); // Clone key for the map

            // Log to AOF
            let frame = Frame::Array(vec![
                Frame::Bulk(Bytes::from("EXPIRE")),
                Frame::Bulk(Bytes::from(key)),
                Frame::Bulk(Bytes::from(duration.as_secs().to_string())),
            ]);
            self.aof.log(frame);

            return true;
        }

        false
    }

    pub fn get(&self, key: &str) -> Option<Bytes> {
        let idx = self.get_shard_index(key);
        let mut shard = self.shards[idx].lock().unwrap();

        if let Some(expiry) = shard.expirations.get(key) {
            if SystemTime::now() > *expiry {
                shard.storage.pop(key);
                shard.expirations.remove(key);
                return None;
            }
        }
        shard.storage.get(key).cloned()
    }

    pub fn del(&self, keys: Vec<String>) -> usize {
        let mut deleted_count = 0;

        for key in keys.iter() {
            if self.del_inner(key) {
                deleted_count += 1;
            }
        }

        // Log to AOF
        if deleted_count > 0 {
            let mut frame_parts = vec![Frame::Bulk(Bytes::from("DEL"))];
            for key in keys {
                frame_parts.push(Frame::Bulk(Bytes::from(key)));
            }
            self.aof.log(Frame::Array(frame_parts));
        }

        deleted_count
    }

    // Helper: delete a single key
    fn del_inner(&self, key: &str) -> bool {
        let idx = self.get_shard_index(key);
        let mut shard = self.shards[idx].lock().unwrap();

        let existed = shard.storage.pop(key).is_some();
        if existed {
            shard.expirations.remove(key);
        }
        existed
    }

    // --- The Garbage Collector ---
    async fn background_purge_task(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(1));

        loop {
            interval.tick().await;

            for shard in self.shards.iter() {
                // Collect frames to log INSIDE the lock
                let frames_to_log = {
                    let mut shard = shard.lock().unwrap();
                    let now = SystemTime::now();

                    // PART 1: Expire old keys
                    let expired_keys: Vec<String> = shard
                        .expirations
                        .iter()
                        .filter(|(_, &time)| now > time)
                        .map(|(k, _)| k.clone())
                        .collect();

                    let mut frames = Vec::new();

                    for key in expired_keys {
                        shard.storage.pop(&key);
                        shard.expirations.remove(&key);

                        // BUILD the frame while still holding lock
                        frames.push(Frame::Array(vec![
                            Frame::Bulk(Bytes::from("DEL")),
                            Frame::Bulk(Bytes::from(key)),
                        ]));
                    }

                    // PART 2: Clean up orphaned expirations (from LRU evictions)
                    // This is a safety net in case evictions happen outside put_with_eviction_cleanup
                    let orphaned_keys: Vec<String> = shard
                        .expirations
                        .keys()
                        .filter(|k| !shard.storage.contains(k.as_str()))
                        .cloned()
                        .collect();

                    for key in orphaned_keys {
                        shard.expirations.remove(&key);
                    }

                    frames
                }; // Lock released here

                // LOG outside the lock (safe now - we built frames inside)
                for frame in frames_to_log {
                    self.aof.log(frame);
                }
            }
        }
    }

    async fn recover(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let file = match File::open(path).await {
            Ok(f) => f,
            Err(_) => return Ok(()),
        };

        let mut reader = FramedRead::new(file, RespCodec);

        while let Some(item) = reader.next().await {
            match item {
                Ok(Frame::Array(ref cmd_parts)) => {
                    if let Some(Frame::Bulk(cmd_bytes)) = cmd_parts.get(0) {
                        let cmd_str = String::from_utf8(cmd_bytes.to_vec())?.to_uppercase();

                        match cmd_str.as_str() {
                            "SET" => {
                                if let (Some(Frame::Bulk(k)), Some(Frame::Bulk(v))) =
                                    (cmd_parts.get(1), cmd_parts.get(2))
                                {
                                    self.set_inner(String::from_utf8(k.to_vec())?, v.clone());
                                }
                            }
                            "EXPIRE" => {
                                if let (Some(Frame::Bulk(k)), Some(Frame::Bulk(sec_bytes))) =
                                    (cmd_parts.get(1), cmd_parts.get(2))
                                {
                                    let key = String::from_utf8(k.to_vec())?;
                                    let secs =
                                        String::from_utf8(sec_bytes.to_vec())?.parse::<u64>()?;

                                    let idx = self.get_shard_index(&key);
                                    let mut shard = self.shards[idx].lock().unwrap();
                                    let deadline = SystemTime::now() + Duration::from_secs(secs);
                                    shard.expirations.insert(key, deadline);
                                }
                            }
                            "DEL" => {
                                if let Some(Frame::Bulk(k)) = cmd_parts.get(1) {
                                    let key = String::from_utf8(k.to_vec())?;
                                    let idx = self.get_shard_index(&key);
                                    let mut shard = self.shards[idx].lock().unwrap();
                                    shard.storage.pop(&key);
                                    shard.expirations.remove(&key);
                                }
                            }
                            _ => {}
                        }
                    }
                }
                Err(e) => {
                    let err_msg = e.to_string();

                    // Check if this is truly an EOF/truncation scenario
                    // need to distinguish between:
                    // 1. Incomplete frame at EOF (legitimate truncation) - IGNORE
                    // 2. Invalid data that cannot be parsed (corruption) - FAIL

                    if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
                        if io_err.kind() == std::io::ErrorKind::UnexpectedEof {
                            // Incomplete frame at EOF - tolerate as truncation
                            eprintln!("Recover: Log truncated unexpectedly. Ignoring partial trailing data.");
                            break;
                        }
                    }

                    // Check for "bytes remaining" which also indicates truncation
                    if err_msg.contains("bytes remaining")
                        || err_msg.contains("remaining on stream")
                    {
                        eprintln!("Recover: Partial data at end of log. Ignoring trailing incomplete frame.");
                        break;
                    }

                    // All other errors (invalid type byte, parse errors, bad UTF-8, etc.) are corruption
                    return Err(anyhow::anyhow!("AOF Corruption detected: {}", e));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn test_lru_eviction() {
        // 1. Create a tiny shard with capacity 2
        let mut shard = DbShard::new(2);

        // 2. Fill it up
        shard.storage.put("a".to_string(), Bytes::from("1"));
        shard.storage.put("b".to_string(), Bytes::from("2"));

        // 3. Insert one more -> Should evict "a" (Least Recently Used)
        shard.storage.put("c".to_string(), Bytes::from("3"));

        // 4. Assertions
        assert_eq!(shard.storage.get("a"), None); // Gone!
        assert_eq!(shard.storage.get("b"), Some(&Bytes::from("2"))); // Still here
        assert_eq!(shard.storage.get("c"), Some(&Bytes::from("3"))); // Newest
    }

    #[test]
    fn test_passive_expiration() {
        let mut shard = DbShard::new(10);

        // 1. Set key with 10ms expiration
        let key = "temp".to_string();
        shard.storage.put(key.clone(), Bytes::from("val"));
        shard
            .expirations
            .insert(key.clone(), SystemTime::now() + Duration::from_millis(10));

        // 2. Access immediately -> Should exist
        // Note: We simulate the 'get' logic manually to avoid locking complications in unit tests
        assert!(shard.storage.get("temp").is_some());

        // 3. Sleep past expiration
        std::thread::sleep(Duration::from_millis(20));

        // 4. Check logic: simulate what Db::get does
        let now = SystemTime::now();
        if let Some(expiry) = shard.expirations.get("temp") {
            if now > *expiry {
                shard.storage.pop("temp");
                shard.expirations.remove("temp");
            }
        }

        // 5. Assert it's gone
        assert_eq!(shard.storage.get("temp"), None);
    }
}
