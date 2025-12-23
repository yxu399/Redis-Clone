use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use redis_starter::Db;
use bytes::Bytes;

// Helper to generate temp filename
fn temp_aof() -> String {
    format!("chaos_{}.aof", uuid::Uuid::new_v4())
}

#[tokio::test]
async fn test_aof_corruption_handling() {
    let path = temp_aof();

    // 1. Create file and write a valid command manually
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .open(&path)
            .await
            .unwrap();
            
        // "SET valid 123"
        file.write_all(b"*3\r\n$3\r\nSET\r\n$5\r\nvalid\r\n$3\r\n123\r\n").await.unwrap();
        
        // 2. Append GARBAGE bytes (Simulate disk corruption)
        file.write_all(b"@@@GARBAGE_DATA###").await.unwrap();
        file.flush().await.unwrap();
    }

    // 3. Attempt startup
    let result = Db::new(&path).await;

    // 4. Verify: It should FAIL (Return Err)
    assert!(result.is_err(), "DB should fail to start on corrupted AOF");
    
    // Optional: Check error message contains "Corruption"
    let err_msg = result.err().unwrap().to_string();
    assert!(err_msg.contains("Corruption") || err_msg.contains("Invalid frame"));
}

#[tokio::test]
async fn test_partial_write_recovery() {
    let path = temp_aof();

    // 1. Write Valid Command + Half Command
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .open(&path)
            .await
            .unwrap();

        // Cmd 1: "SET safe 1" (Complete)
        file.write_all(b"*3\r\n$3\r\nSET\r\n$4\r\nsafe\r\n$1\r\n1\r\n").await.unwrap();

        // Cmd 2: "SET broken 2" ... CUT OFF IN MIDDLE
        // We write the array header *3, the command SET, the key broken... but NO VALUE
        file.write_all(b"*3\r\n$3\r\nSET\r\n$6\r\nbroken\r\n").await.unwrap();
        
        file.flush().await.unwrap();
    }

    // 2. Attempt startup
    let result = Db::new(&path).await;

    // 3. Verify: It should SUCCEED (ignoring the tail)
    assert!(result.is_ok(), "DB should handle truncated logs gracefully");
    let db = result.unwrap();

    // 4. Validate Data Integrity
    assert_eq!(db.get("safe"), Some(Bytes::from("1"))); // Should exist
    assert_eq!(db.get("broken"), None); // Should NOT exist (was partial)
}