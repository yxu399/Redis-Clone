use redis_clone::{Db, RespCodec, Frame, Command};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;
use futures::{SinkExt, StreamExt};
use bytes::Bytes;
use std::time::Duration;

async fn spawn_test_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let aof_path = format!("test_{}.aof", uuid::Uuid::new_v4());
    let db = Db::new(&aof_path).await.unwrap();

    tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let db = db.clone();
            tokio::spawn(async move {
                let mut framed = Framed::new(socket, RespCodec);
                while let Some(Ok(frame)) = framed.next().await {
                    let response = match Command::from_frame(frame) {
                        Ok(cmd) => cmd.apply(&db),
                        Err(e) => Frame::Error(e.to_string()),
                    };
                    let _ = framed.send(response).await;
                }
            });
        }
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    addr
}

#[tokio::test]
async fn test_del_multiple_keys() {
    let addr = spawn_test_server().await;
    let stream = TcpStream::connect(&addr).await.unwrap();
    let mut client = Framed::new(stream, RespCodec);

    // Set multiple keys
    for i in 1..=5 {
        let cmd = Frame::Array(vec![
            Frame::Bulk(Bytes::from("SET")),
            Frame::Bulk(Bytes::from(format!("key{}", i))),
            Frame::Bulk(Bytes::from(format!("val{}", i))),
        ]);
        client.send(cmd).await.unwrap();
        client.next().await.unwrap().unwrap(); // Consume OK
    }

    // Delete some of them
    let cmd = Frame::Array(vec![
        Frame::Bulk(Bytes::from("DEL")),
        Frame::Bulk(Bytes::from("key1")),
        Frame::Bulk(Bytes::from("key3")),
        Frame::Bulk(Bytes::from("key5")),
        Frame::Bulk(Bytes::from("nonexistent")),
    ]);
    client.send(cmd).await.unwrap();
    
    let response = client.next().await.unwrap().unwrap();
    assert_eq!(response, Frame::Integer(3)); // 3 keys deleted

    // Verify remaining keys
    for (key, should_exist) in [("key1", false), ("key2", true), ("key3", false), ("key4", true), ("key5", false)] {
        let cmd = Frame::Array(vec![
            Frame::Bulk(Bytes::from("GET")),
            Frame::Bulk(Bytes::from(key)),
        ]);
        client.send(cmd).await.unwrap();
        let response = client.next().await.unwrap().unwrap();
        
        if should_exist {
            assert!(matches!(response, Frame::Bulk(_)), "Key {} should exist", key);
        } else {
            assert_eq!(response, Frame::Null, "Key {} should not exist", key);
        }
    }
}

#[tokio::test]
async fn test_lru_eviction_with_expiration() {
    let aof = format!("test_lru_exp_{}.aof", uuid::Uuid::new_v4());
    let db = Db::new(&aof).await.unwrap();
    
    // Fill up one shard (shard 0 has capacity 10000, but we'll just test the concept)
    // We'll use keys that hash to the same shard
    let test_key = "test_key_";
    
    for i in 0..5 {
        let key = format!("{}{}", test_key, i);
        db.set(key.clone(), Bytes::from(format!("value{}", i)));
        
        // Set expiration on some keys
        if i % 2 == 0 {
            db.set_expires(key, Duration::from_secs(3600));
        }
    }
    
    // Give background task time to run
    tokio::time::sleep(Duration::from_millis(1500)).await;
    
    // All keys should still exist
    for i in 0..5 {
        let key = format!("{}{}", test_key, i);
        assert!(db.get(&key).is_some(), "Key {} should exist", key);
    }
    
    std::fs::remove_file(&aof).ok();
}

#[tokio::test]
async fn test_expire_then_set() {
    let addr = spawn_test_server().await;
    let stream = TcpStream::connect(&addr).await.unwrap();
    let mut client = Framed::new(stream, RespCodec);

    // Set a key
    client.send(Frame::Array(vec![
        Frame::Bulk(Bytes::from("SET")),
        Frame::Bulk(Bytes::from("key")),
        Frame::Bulk(Bytes::from("original")),
    ])).await.unwrap();
    client.next().await.unwrap().unwrap();

    // Set expiration
    client.send(Frame::Array(vec![
        Frame::Bulk(Bytes::from("EXPIRE")),
        Frame::Bulk(Bytes::from("key")),
        Frame::Bulk(Bytes::from("1")),
    ])).await.unwrap();
    let res = client.next().await.unwrap().unwrap();
    assert_eq!(res, Frame::Integer(1));

    // Wait for expiration
    tokio::time::sleep(Duration::from_millis(1100)).await;

    // Verify it's gone
    client.send(Frame::Array(vec![
        Frame::Bulk(Bytes::from("GET")),
        Frame::Bulk(Bytes::from("key")),
    ])).await.unwrap();
    let res = client.next().await.unwrap().unwrap();
    assert_eq!(res, Frame::Null);

    // Set it again
    client.send(Frame::Array(vec![
        Frame::Bulk(Bytes::from("SET")),
        Frame::Bulk(Bytes::from("key")),
        Frame::Bulk(Bytes::from("new_value")),
    ])).await.unwrap();
    client.next().await.unwrap().unwrap();

    // Should have new value with no expiration
    client.send(Frame::Array(vec![
        Frame::Bulk(Bytes::from("GET")),
        Frame::Bulk(Bytes::from("key")),
    ])).await.unwrap();
    let res = client.next().await.unwrap().unwrap();
    assert_eq!(res, Frame::Bulk(Bytes::from("new_value")));

    // Wait again - should still be there (no expiration anymore)
    tokio::time::sleep(Duration::from_millis(1100)).await;
    client.send(Frame::Array(vec![
        Frame::Bulk(Bytes::from("GET")),
        Frame::Bulk(Bytes::from("key")),
    ])).await.unwrap();
    let res = client.next().await.unwrap().unwrap();
    assert_eq!(res, Frame::Bulk(Bytes::from("new_value")));
}

#[tokio::test]
async fn test_persistence_with_del() {
    let aof = format!("test_persist_del_{}.aof", uuid::Uuid::new_v4());
    
    // Create DB and write some data
    {
        let db = Db::new(&aof).await.unwrap();
        db.set("keep1".to_string(), Bytes::from("value1"));
        db.set("delete_me".to_string(), Bytes::from("value2"));
        db.set("keep2".to_string(), Bytes::from("value3"));
        
        // Delete one
        db.del(vec!["delete_me".to_string()]);
        
        // Give AOF time to flush
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    
    // Recover from AOF
    let db = Db::new(&aof).await.unwrap();
    
    // Verify state
    assert_eq!(db.get("keep1"), Some(Bytes::from("value1")));
    assert_eq!(db.get("delete_me"), None);
    assert_eq!(db.get("keep2"), Some(Bytes::from("value3")));
    
    std::fs::remove_file(&aof).ok();
}

#[tokio::test]
async fn test_persistence_with_expiration() {
    let aof = format!("test_persist_exp_{}.aof", uuid::Uuid::new_v4());
    
    {
        let db = Db::new(&aof).await.unwrap();
        db.set("permanent".to_string(), Bytes::from("forever"));
        db.set("temporary".to_string(), Bytes::from("soon_gone"));
        db.set_expires("temporary".to_string(), Duration::from_secs(1));
        
        // Wait for expiration + background task
        tokio::time::sleep(Duration::from_millis(1500)).await;
        
        // Temporary should be gone, permanent should remain
        assert_eq!(db.get("permanent"), Some(Bytes::from("forever")));
        assert_eq!(db.get("temporary"), None);
        
        // Wait for AOF flush
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    
    // Recover - should only have permanent
    let db = Db::new(&aof).await.unwrap();
    assert_eq!(db.get("permanent"), Some(Bytes::from("forever")));
    assert_eq!(db.get("temporary"), None);
    
    std::fs::remove_file(&aof).ok();
}