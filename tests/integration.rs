use tokio::net::TcpStream;
use tokio::net::TcpListener;
use tokio_util::codec::Framed;
use futures::{SinkExt, StreamExt};
use bytes::Bytes;
use std::time::Duration;
use redis_starter::{Db, RespCodec, Frame};

// --- Test Helper ---
// Spawns a real server instance on a random port.
// Returns the address (e.g., "127.0.0.1:54321")
async fn spawn_server() -> String {
    // 1. Bind to port 0 to get a random available port
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    // 2. Use a random temporary file for AOF to avoid collisions
    let aof_path = format!("test_{}.aof", uuid::Uuid::new_v4());
    
    // 3. Initialize DB
    let db = Db::new(&aof_path).await.unwrap();

    // 4. Spawn the server loop in the background
    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let db = db.clone();
            tokio::spawn(async move {
                let mut framed = Framed::new(socket, RespCodec);
                while let Some(Ok(frame)) = framed.next().await {
                    // Replicating main.rs logic here
                    let response = match redis_starter::Command::from_frame(frame) {
                        Ok(cmd) => cmd.apply(&db),
                        Err(e) => Frame::Error(e.to_string()),
                    };
                    framed.send(response).await.unwrap();
                }
            });
        }
    });

    // Give the server a split second to start
    tokio::time::sleep(Duration::from_millis(50)).await;
    
    addr
}

// --- The Tests ---

#[tokio::test]
async fn test_ping_pong() {
    let addr = spawn_server().await;
    let stream = TcpStream::connect(&addr).await.unwrap();
    let mut client = Framed::new(stream, RespCodec);

    // Send PING
    let cmd = Frame::Array(vec![Frame::Bulk(Bytes::from("PING"))]);
    client.send(cmd).await.unwrap();

    // Expect PONG
    let response = client.next().await.unwrap().unwrap();
    assert_eq!(response, Frame::Simple("PONG".to_string()));
}

#[tokio::test]
async fn test_set_get() {
    let addr = spawn_server().await;
    let stream = TcpStream::connect(&addr).await.unwrap();
    let mut client = Framed::new(stream, RespCodec);

    // SET key val
    let cmd = Frame::Array(vec![
        Frame::Bulk(Bytes::from("SET")),
        Frame::Bulk(Bytes::from("foo")),
        Frame::Bulk(Bytes::from("bar")),
    ]);
    client.send(cmd).await.unwrap();
    
    // Assert OK
    let response = client.next().await.unwrap().unwrap();
    assert_eq!(response, Frame::Simple("OK".to_string()));

    // GET key
    let cmd = Frame::Array(vec![
        Frame::Bulk(Bytes::from("GET")),
        Frame::Bulk(Bytes::from("foo")),
    ]);
    client.send(cmd).await.unwrap();

    // Assert "bar"
    let response = client.next().await.unwrap().unwrap();
    assert_eq!(response, Frame::Bulk(Bytes::from("bar")));
}

#[tokio::test]
async fn test_expiration_logic() {
    let addr = spawn_server().await;
    let stream = TcpStream::connect(&addr).await.unwrap();
    let mut client = Framed::new(stream, RespCodec);

    // 1. SET bomb "boom"
    client.send(Frame::Array(vec![
        Frame::Bulk(Bytes::from("SET")),
        Frame::Bulk(Bytes::from("bomb")),
        Frame::Bulk(Bytes::from("boom")),
    ])).await.unwrap();
    client.next().await.unwrap().unwrap(); // Consume OK

    // 2. EXPIRE bomb 1 (1 second)
    client.send(Frame::Array(vec![
        Frame::Bulk(Bytes::from("EXPIRE")),
        Frame::Bulk(Bytes::from("bomb")),
        Frame::Bulk(Bytes::from("1")),
    ])).await.unwrap();
    let res = client.next().await.unwrap().unwrap();
    assert_eq!(res, Frame::Integer(1)); // Expect 1 (success)

    // 3. GET immediately (Should exist)
    client.send(Frame::Array(vec![
        Frame::Bulk(Bytes::from("GET")),
        Frame::Bulk(Bytes::from("bomb")),
    ])).await.unwrap();
    let res = client.next().await.unwrap().unwrap();
    assert_eq!(res, Frame::Bulk(Bytes::from("boom")));

    // 4. Wait 1.1 seconds
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // 5. GET again (Should be Null)
    client.send(Frame::Array(vec![
        Frame::Bulk(Bytes::from("GET")),
        Frame::Bulk(Bytes::from("bomb")),
    ])).await.unwrap();
    let res = client.next().await.unwrap().unwrap();
    assert_eq!(res, Frame::Null);
}