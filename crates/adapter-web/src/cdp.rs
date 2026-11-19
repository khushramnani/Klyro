//! Minimal sync CDP client over one WebSocket (flattened sessions).

use klyro_core::{Error, Result};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::ErrorKind;
use std::net::TcpStream;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};

const CALL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_EVENTS: usize = 1000;

pub fn engine_err(e: impl std::fmt::Display) -> Error {
    Error::Engine(e.to_string())
}

pub struct Cdp {
    ws: WebSocket<TcpStream>,
    next: u64,
    /// Sent with every call once attached to a page.
    pub session: Option<String>,
    events: VecDeque<Value>,
    /// Calls we stopped waiting for that the browser may still be running.
    abandoned: Vec<u64>,
}

impl Cdp {
    pub fn connect(url: &str) -> Result<Self> {
        let addr = url
            .strip_prefix("ws://")
            .and_then(|r| r.split('/').next())
            .ok_or_else(|| engine_err(format!("bad CDP url {url}")))?;
        let tcp = TcpStream::connect(addr).map_err(engine_err)?;
        tcp.set_read_timeout(Some(CALL_TIMEOUT)).map_err(engine_err)?;
        let (ws, _) = tungstenite::client(url, tcp).map_err(|e| engine_err(format!("CDP handshake: {e}")))?;
        // After the handshake: short read timeout so waits can check their deadline;
        // tungstenite buffers partial frames across timeouts.
        ws.get_ref().set_read_timeout(Some(Duration::from_millis(200))).map_err(engine_err)?;
        Ok(Self { ws, next: 0, session: None, events: VecDeque::new(), abandoned: vec![] })
    }

    /// One message, or None on read timeout.
    fn read(&mut self) -> Result<Option<Value>> {
        match self.ws.read() {
            Ok(Message::Text(t)) => serde_json::from_str(t.as_str()).map(Some).map_err(engine_err),
            Ok(_) => Ok(None),
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
            Err(e) => Err(engine_err(format!("browser connection lost: {e}"))),
        }
    }

    fn keep_event(&mut self, v: Value) {
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(v);
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.call_timeout(method, params, CALL_TIMEOUT)
    }

    /// Like `call`, with its own deadline. A reply that arrives later is ignored.
    pub fn call_timeout(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let end = Instant::now() + timeout;
        // Chrome can stall a call sent while an abandoned one still runs (a live-view frame
        // that timed out, then a screenshot): give those a few seconds to finish first. A reply
        // may never come (the page went away), so then forget them rather than wait every time.
        let drain = (Instant::now() + Duration::from_secs(5)).min(end);
        while !self.abandoned.is_empty() && Instant::now() < drain {
            if let Some(v) = self.read()? {
                self.note(v);
            }
        }
        self.abandoned.clear();
        self.next += 1;
        let id = self.next;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = &self.session {
            msg["sessionId"] = json!(s);
        }
        self.ws.send(Message::text(msg.to_string())).map_err(engine_err)?;
        while Instant::now() < end {
            let Some(v) = self.read()? else { continue };
            if v["id"] == id {
                return match v.get("error") {
                    Some(e) => Err(Error::Engine(format!("{method}: {}", e["message"].as_str().unwrap_or("error")))),
                    None => Ok(v["result"].clone()),
                };
            }
            self.note(v);
        }
        self.abandoned.push(id);
        Err(Error::Engine(format!("{method}: timed out")))
    }

    /// A message that is not the reply being waited for: an event, or a late reply.
    fn note(&mut self, v: Value) {
        if v.get("method").is_some() {
            self.keep_event(v);
        } else if let Some(id) = v["id"].as_u64() {
            self.abandoned.retain(|a| *a != id);
        }
    }

    /// Drop buffered events, so a following wait only sees new ones.
    pub fn clear_events(&mut self) {
        self.events.clear();
    }

    /// First event (buffered or new) matching `pred`, or None after `timeout`.
    pub fn wait_for(&mut self, pred: impl Fn(&Value) -> bool, timeout: Duration) -> Result<Option<Value>> {
        if let Some(i) = self.events.iter().position(&pred) {
            return Ok(self.events.remove(i));
        }
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            match self.read()? {
                Some(v) if v.get("method").is_some() && pred(&v) => return Ok(Some(v)),
                Some(v) if v.get("method").is_some() => self.keep_event(v),
                _ => {}
            }
        }
        Ok(None)
    }
}
