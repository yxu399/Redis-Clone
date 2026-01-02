use clap::Parser;
use futures::{SinkExt, StreamExt};
use redis_clone::raft::raft_service_server::RaftServiceServer;
use redis_clone::{Command, Db, Frame, RaftNode, RespCodec};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;
use tonic::transport::Server;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Unique ID of this node (1, 2, 3...)
    #[arg(long, default_value_t = 1)]
    id: u64,

    /// Address to listen for Raft gRPC traffic
    #[arg(long, default_value = "127.0.0.1:50051")]
    grpc_addr: String,

    /// Address to listen for Redis TCP traffic
    #[arg(long, default_value = "127.0.0.1:6379")]
    redis_addr: String,

    /// Comma-separated list of peers
    #[arg(long, default_value = "")]
    peers: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let peer_addrs: Vec<String> = if args.peers.is_empty() {
        Vec::new()
    } else {
        args.peers.split(',').map(|s| s.to_string()).collect()
    };

    println!("Node {} starting. Peers: {:?}", args.id, peer_addrs);

    // 1. Initialize DB
    let aof_path = format!("node_{}.aof", args.id);
    let db = Db::new(&aof_path).await?;

    // 2. Channel & Raft Node
    let (tx, mut rx) = mpsc::unbounded_channel();
    let raft_node = RaftNode::new(args.id, peer_addrs, tx);

    // 3. Spawn Raft gRPC Server (WITH ERROR LOGGING)
    let grpc_addr: SocketAddr = args.grpc_addr.parse()?;
    let raft_service = RaftServiceServer::new(raft_node.clone());

    println!("Starting Raft Server on {}", grpc_addr);
    tokio::spawn(async move {
        if let Err(e) = Server::builder()
            .add_service(raft_service)
            .serve(grpc_addr)
            .await
        {
            // THIS IS THE CRITICAL FIX: Print why we failed!
            eprintln!(
                "CRITICAL ERROR: Raft Server failed to bind/start on {}: {}",
                grpc_addr, e
            );
        }
    });

    // 4. Start Election Loop
    let node_for_election = raft_node.clone();
    tokio::spawn(async move {
        // Sleep briefly to let server start
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        node_for_election.run_election_loop().await;
    });

    // 5. Applier Task
    let db_for_applier = db.clone();
    tokio::spawn(async move {
        println!("Applier Task Started");
        while let Some(cmd_bytes) = rx.recv().await {
            let cmd_str = String::from_utf8(cmd_bytes).unwrap();
            let parts: Vec<&str> = cmd_str.split_whitespace().collect();

            if parts.len() >= 3 && parts[0] == "SET" {
                let key = parts[1].to_string();
                let val = parts[2].to_string().into_bytes();
                let set_cmd = Command::Set(key.clone(), val.into());
                set_cmd.apply(&db_for_applier);
                println!("APPLIED TO DB: SET {}", key);
            }
        }
    });

    // 6. Redis Listener
    println!("Redis Server listening on {}", args.redis_addr);
    let listener = TcpListener::bind(&args.redis_addr).await?;

    loop {
        let (socket, _) = listener.accept().await?;
        let db_handle = db.clone();
        let raft_handle = raft_node.clone();

        tokio::spawn(async move {
            let mut framed = Framed::new(socket, RespCodec);

            while let Some(Ok(frame)) = framed.next().await {
                if let Ok(cmd) = Command::from_frame(frame.clone()) {
                    match cmd {
                        Command::Set(key, val) => {
                            let payload = format!("SET {} {:?}", key, val).into_bytes();
                            match raft_handle.propose(payload) {
                                Ok(_) => {
                                    let _ = framed
                                        .send(Frame::Simple("OK (Raft Proposed)".to_string()))
                                        .await;
                                }
                                Err(e) => {
                                    let _ =
                                        framed.send(Frame::Error(format!("Redirect: {}", e))).await;
                                }
                            }
                        }
                        _ => {
                            let response = cmd.apply(&db_handle);
                            let _ = framed.send(response).await;
                        }
                    }
                }
            }
        });
    }
}
