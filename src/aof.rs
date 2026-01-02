use crate::codec::RespCodec;
use crate::frame::Frame;
use anyhow::Result;
use futures::sink::SinkExt; // for .send()
use std::path::Path;
use tokio::fs::OpenOptions;
use tokio::sync::mpsc;
use tokio_util::codec::FramedWrite;

pub struct Aof {
    // We only need the Sender to push generic frames (like SET k v)
    tx: mpsc::Sender<Frame>,
}

impl Aof {
    pub async fn new_inactive(path: impl AsRef<Path>) -> Result<(Self, AofActivator)> {
        let path = path.as_ref().to_owned();

        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .append(true)
            .open(&path)
            .await?;

        let (tx, rx) = mpsc::channel(1000);

        // Return both the Aof and the receiver to activate later
        Ok((Aof { tx }, AofActivator { rx, file }))
    }

    pub fn log(&self, frame: Frame) {
        let _ = self.tx.try_send(frame);
    }
}

pub struct AofActivator {
    rx: mpsc::Receiver<Frame>,
    file: tokio::fs::File,
}

impl AofActivator {
    // Activate the background writer after recovery is complete
    pub fn activate(mut self) {
        tokio::spawn(async move {
            let mut writer = FramedWrite::new(self.file, RespCodec);

            while let Some(frame) = self.rx.recv().await {
                if let Err(e) = writer.send(frame).await {
                    tracing::error!("AOF Write Error: {:?}", e);
                }
            }
        });
    }
}
