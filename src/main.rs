// src/main.rs
use tokio::net::TcpListener;
use tokio_util::codec::Framed;
use futures::{SinkExt, StreamExt};
use redis_clone::{Db, RespCodec, Command, Frame}; // <--- Import from lib

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let addr = "127.0.0.1:6379";
    let listener = TcpListener::bind(addr).await?;
    println!("Listening on: {}", addr);

    let db = Db::new("redis.aof").await?;

    loop {
        let (socket, _) = listener.accept().await?;
        let db_handle = db.clone();

        tokio::spawn(async move {
            let mut framed = Framed::new(socket, RespCodec);

            loop {
                match framed.next().await {
                    Some(Ok(frame)) => {
                        let response = match Command::from_frame(frame) {
                            Ok(cmd) => cmd.apply(&db_handle),
                            Err(e) => Frame::Error(e.to_string()),
                        };
                        if let Err(_e) = framed.send(response).await {
                            // eprintln!("Send Error: {:?}", e);
                            return;
                        }
                    }
                    Some(Err(_e)) => {
                        // eprintln!("Frame decode error: {:?}", e);
                        // Send error response instead of closing connection
                        let _ = framed.send(Frame::Error("Protocol error".to_string())).await;
                        return;
                    }
                    None => return,
                }
            }
        });
    }
}