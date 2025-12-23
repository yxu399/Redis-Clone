use crate::frame::Frame;
use crate::codec::RespCodec;
use tokio::fs::OpenOptions;
use tokio::sync::mpsc;
use tokio_util::codec::FramedWrite;
use futures::sink::SinkExt; // for .send()
use std::path::Path;
use anyhow::Result;

pub struct Aof {
    // We only need the Sender to push generic frames (like SET k v)
    tx: mpsc::Sender<Frame>,
}

impl Aof {
    pub async fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_owned();
        
        // 1. Open the file (create if missing, append mode)
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .append(true)
            .open(&path)
            .await?;

        // 2. Create a channel for buffering writes
        // Buffer size 1000 means we can queue 1000 ops before slowing down
        let (tx, mut rx) = mpsc::channel(1000);

        // 3. Spawn the background writer task
        tokio::spawn(async move {
            // FramedWrite wraps the file and uses our Codec to encode frames to bytes
            let mut writer = FramedWrite::new(file, RespCodec);

            while let Some(frame) = rx.recv().await {
                if let Err(_e) = writer.send(frame).await {
                    // eprintln!("AOF Write Error: {:?}", e);
                    // In a real DB, you might panic here or switch to read-only mode
                }
            }
            // Ensure data hits the disk when the channel closes
            // In production, you'd also want periodic fsync (e.g., every 1s)
        });

        Ok(Aof { tx })
    }

    /// Fire and forget: sends the frame to the background task
    pub fn log(&self, frame: Frame) {
        // We use try_send to avoid blocking the main request threads.
        // If the disk is too slow and channel is full, we drop the log (or you could await).
        // For high-performance, we usually accept the risk of dropping logs under extreme load
        // rather than blocking the server.
        let _ = self.tx.try_send(frame); 
    }
}