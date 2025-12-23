use crate::frame::Frame;
use crate::db::Db;
use bytes::Bytes;
use anyhow::{anyhow, Result};

// The executable actions our server supports
pub enum Command {
    Get(String),
    Set(String, Bytes),
    Expire(String, u64),
    Ping(Option<String>),
    Unknown(String),
}

impl Command {
    // Parse a Frame into a Command
    pub fn from_frame(frame: Frame) -> Result<Command> {
        let mut array = match frame {
            Frame::Array(array) => array,
            _ => return Err(anyhow!("Invalid command format")),
        };

        // Extract the command name (e.g., "SET")
        let command_frame = array.pop_front_frame().ok_or(anyhow!("Empty command"))?;
        let command_name = match command_frame {
            Frame::Bulk(b) => String::from_utf8(b.to_vec())?.to_uppercase(),
            Frame::Simple(s) => s.to_uppercase(),
            _ => return Err(anyhow!("Invalid command name type")),
        };

        match command_name.as_str() {
            "GET" => {
                let key_frame = array.pop_front_frame().ok_or(anyhow!("GET requires key"))?;
                let key = frame_to_string(key_frame)?;
                Ok(Command::Get(key))
            }
            "SET" => {
                let key_frame = array.pop_front_frame().ok_or(anyhow!("SET requires key"))?;
                let val_frame = array.pop_front_frame().ok_or(anyhow!("SET requires value"))?;
                
                let key = frame_to_string(key_frame)?;
                let val = match val_frame {
                    Frame::Bulk(b) => b,
                    Frame::Simple(s) => Bytes::from(s),
                    _ => return Err(anyhow!("Invalid value type")),
                };
                
                Ok(Command::Set(key, val))
            }
            "PING" => {
                // Check if there is an argument (PING vs PING "hello")
                match array.pop_front_frame() {
                    Some(frame) => {
                        let msg = frame_to_string(frame)?;
                        Ok(Command::Ping(Some(msg)))
                    },
                    None => Ok(Command::Ping(None)),
                }
            }
            "EXPIRE" => {
                let key_frame = array.pop_front_frame().ok_or(anyhow!("EXPIRE requires key"))?;
                let sec_frame = array.pop_front_frame().ok_or(anyhow!("EXPIRE requires seconds"))?;
                
                let key = frame_to_string(key_frame)?;
                let secs_str = frame_to_string(sec_frame)?;
                let secs = secs_str.parse::<u64>().map_err(|_| anyhow!("Invalid integer for seconds"))?;
                
                Ok(Command::Expire(key, secs))
            }
            unknown => Ok(Command::Unknown(unknown.to_string())),
        }
    }

    // Execute the command against the DB
    pub fn apply(self, db: &Db) -> Frame {
        match self {
            Command::Get(key) => {
                if let Some(val) = db.get(&key) {
                    Frame::Bulk(val)
                } else {
                    Frame::Null
                }
            }
            Command::Set(key, val) => {
                db.set(key, val);
                Frame::Simple("OK".to_string())
            }
            Command::Ping(msg) => {
                match msg {
                    Some(s) => Frame::Bulk(Bytes::from(s)),
                    None => Frame::Simple("PONG".to_string()),
                }
            }
            Command::Unknown(cmd) => {
                Frame::Error(format!("unknown command '{}'", cmd))
            }
            Command::Expire(key, secs) => {
                let success = db.set_expires(key, std::time::Duration::from_secs(secs));
                if success {
                    Frame::Integer(1)
                } else {
                    Frame::Integer(0)
                }
            }
        }
    }
}

// Helper: Vec<Frame> doesn't have pop_front, so we use a simple utility
// Or we could reverse iter logic, but let's add a helper extension for clean code.
trait ArrayExt {
    fn pop_front_frame(&mut self) -> Option<Frame>;
}

impl ArrayExt for Vec<Frame> {
    fn pop_front_frame(&mut self) -> Option<Frame> {
        if self.is_empty() {
            None
        } else {
            // This is O(N) but fine for short command arrays (length 2 or 3)
            Some(self.remove(0)) 
        }
    }
}

fn frame_to_string(frame: Frame) -> Result<String> {
    match frame {
        Frame::Simple(s) => Ok(s),
        Frame::Bulk(b) => Ok(String::from_utf8(b.to_vec())?),
        _ => Err(anyhow!("Invalid string format")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_frame_set() {
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from("SET")),
            Frame::Bulk(Bytes::from("key")),
            Frame::Bulk(Bytes::from("value")),
        ]);
        
        let cmd = Command::from_frame(frame).unwrap();
        match cmd {
            Command::Set(k, v) => {
                assert_eq!(k, "key");
                assert_eq!(v, "value");
            },
            _ => panic!("Expected Set command"),
        }
    }

    #[test]
    fn test_from_frame_get_case_insensitive() {
        let frame = Frame::Array(vec![
            Frame::Bulk(Bytes::from("gEt")), // Mixed case
            Frame::Bulk(Bytes::from("key")),
        ]);
        
        let cmd = Command::from_frame(frame).unwrap();
        match cmd {
            Command::Get(k) => assert_eq!(k, "key"),
            _ => panic!("Expected Get command"),
        }
    }

    #[test]
    fn test_invalid_command() {
        let frame = Frame::Simple("NotAnArray".to_string());
        assert!(Command::from_frame(frame).is_err());
    }
}