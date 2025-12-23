use criterion::{black_box, criterion_group, criterion_main, Criterion};
use redis_clone::Db;
use bytes::Bytes;
use std::sync::Arc;

/// Benchmarks write performance (SET operations)
/// Measures time to perform 1000 sequential writes
fn benchmark_write(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    
    c.bench_function("write_1000", |b| {
        let db = rt.block_on(async {
            let aof_path = format!("bench_write_{}.aof", std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis());
            Db::new(aof_path).await.unwrap()
        });
        
        let mut counter = 0;
        
        b.to_async(&rt).iter(|| {
            let db = db.clone();
            let start = counter;
            counter += 1000;
            
            async move {
                for i in start..start + 1000 {
                    let key = format!("key{}", i);
                    let value = format!("value{}", i);
                    db.set(black_box(key), black_box(Bytes::from(value)));
                }
            }
        });
    });
}

/// Benchmarks read performance (GET operations)  
/// Pre-populates 10k keys and measures read time
fn benchmark_read(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    
    let db = rt.block_on(async {
        let aof_path = format!("bench_read_{}.aof", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis());
        let db = Db::new(aof_path).await.unwrap();
        
        // Pre-populate with data
        for i in 0..10_000 {
            db.set(format!("key{}", i), Bytes::from(format!("value{}", i)));
        }
        
        Arc::new(db)
    });
    
    c.bench_function("read_1000", |b| {
        let mut counter = 0;
        
        b.to_async(&rt).iter(|| {
            let db = Arc::clone(&db);
            let start = counter % 10_000;
            counter += 1000;
            
            async move {
                for i in start..start + 1000 {
                    let key = format!("key{}", i);
                    let _ = db.get(black_box(&key));
                }
            }
        });
    });
}

/// Benchmarks realistic workload (70% reads, 30% writes)
/// Simulates typical web application access pattern
fn benchmark_mixed(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    
    let db = rt.block_on(async {
        let aof_path = format!("bench_mixed_{}.aof", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis());
        let db = Db::new(aof_path).await.unwrap();
        
        // Pre-populate
        for i in 0..10_000 {
            db.set(format!("key{}", i), Bytes::from(format!("value{}", i)));
        }
        
        Arc::new(db)
    });
    
    c.bench_function("mixed_1000", |b| {
        let mut counter = 0;
        
        b.to_async(&rt).iter(|| {
            let db = Arc::clone(&db);
            let start = counter;
            counter += 1000;
            
            async move {
                for i in start..start + 1000 {
                    let read_key = format!("key{}", i % 10_000);
                    let write_key = format!("newkey{}", i);
                    
                    // 70% reads, 30% writes (typical web workload)
                    if i % 10 < 7 {
                        let _ = db.get(black_box(&read_key));
                    } else {
                        db.set(black_box(write_key), black_box(Bytes::from("value")));
                    }
                }
            }
        });
    });
}

criterion_group!(benches, benchmark_write, benchmark_read, benchmark_mixed);
criterion_main!(benches);