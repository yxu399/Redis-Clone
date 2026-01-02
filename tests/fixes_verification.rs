use bytes::Bytes;
use redis_clone::Db;
use std::time::Duration;

#[tokio::test]
async fn test_del_command() {
    let aof = format!("test_del_{}.aof", uuid::Uuid::new_v4());
    let db = Db::new(&aof).await.unwrap();

    // Set some keys
    db.set("key1".to_string(), Bytes::from("val1"));
    db.set("key2".to_string(), Bytes::from("val2"));
    db.set("key3".to_string(), Bytes::from("val3"));

    // Delete key1 and key2
    let deleted = db.del(vec![
        "key1".to_string(),
        "key2".to_string(),
        "nonexistent".to_string(),
    ]);
    assert_eq!(deleted, 2);

    // Verify
    assert_eq!(db.get("key1"), None);
    assert_eq!(db.get("key2"), None);
    assert_eq!(db.get("key3"), Some(Bytes::from("val3")));

    // Cleanup
    std::fs::remove_file(&aof).ok();
}

#[tokio::test]
async fn test_aof_recovery_race_fixed() {
    let aof = format!("test_race_{}.aof", uuid::Uuid::new_v4());

    // Create DB and immediately write
    {
        let db = Db::new(&aof).await.unwrap();
        db.set("key1".to_string(), Bytes::from("value1"));
        // Drop to flush
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Recover - should see key1
    let db = Db::new(&aof).await.unwrap();
    assert_eq!(db.get("key1"), Some(Bytes::from("value1")));

    // Cleanup
    std::fs::remove_file(&aof).ok();
}
