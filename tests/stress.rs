use bytes::Bytes;
use redis_clone::Db;
use std::sync::Arc;

#[tokio::test]
async fn test_concurrent_operations() {
    let aof = format!("test_concurrent_{}.aof", uuid::Uuid::new_v4());
    let db = Arc::new(Db::new(&aof).await.unwrap());

    let mut handles = vec![];

    // Spawn 10 concurrent writers
    for i in 0..10 {
        let db = Arc::clone(&db);
        let handle = tokio::spawn(async move {
            for j in 0..100 {
                let key = format!("key_{}_{}", i, j);
                let value = format!("value_{}_{}", i, j);
                db.set(key, Bytes::from(value));
            }
        });
        handles.push(handle);
    }

    // Wait for all writers
    for handle in handles {
        handle.await.unwrap();
    }

    // Verify all keys exist
    for i in 0..10 {
        for j in 0..100 {
            let key = format!("key_{}_{}", i, j);
            let expected = format!("value_{}_{}", i, j);
            assert_eq!(db.get(&key), Some(Bytes::from(expected)));
        }
    }

    std::fs::remove_file(&aof).ok();
}

#[tokio::test]
async fn test_concurrent_read_write() {
    let aof = format!("test_rw_{}.aof", uuid::Uuid::new_v4());
    let db = Arc::new(Db::new(&aof).await.unwrap());

    // Pre-populate
    for i in 0..100 {
        db.set(format!("key{}", i), Bytes::from(format!("value{}", i)));
    }

    let mut handles = vec![];

    // Spawn readers
    for _ in 0..5 {
        let db = Arc::clone(&db);
        let handle = tokio::spawn(async move {
            for i in 0..100 {
                let key = format!("key{}", i);
                let _ = db.get(&key);
            }
        });
        handles.push(handle);
    }

    // Spawn writers
    for i in 0..5 {
        let db = Arc::clone(&db);
        let handle = tokio::spawn(async move {
            for j in 0..100 {
                let key = format!("newkey_{}_{}", i, j);
                db.set(key, Bytes::from("newvalue"));
            }
        });
        handles.push(handle);
    }

    // Wait for completion
    for handle in handles {
        handle.await.unwrap();
    }

    std::fs::remove_file(&aof).ok();
}
