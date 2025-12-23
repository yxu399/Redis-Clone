use bytes::Bytes;

#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    Simple(String),       // +OK\r\n
    Error(String),        // -Error message\r\n
    Integer(i64),         // :1000\r\n
    Bulk(Bytes),          // $6\r\nfoobar\r\n
    Null,                 // $-1\r\n (Null Bulk String)
    Array(Vec<Frame>),    // *2\r\n$3\r\nfoo\r\n$3\r\nbar\r\n
}