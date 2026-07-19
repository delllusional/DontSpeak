use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

const MAX_RPC_LINE_BYTES: usize = 1024 * 1024;
const MAX_RPC_MESSAGES: usize = 256;

pub(super) struct Request<'a> {
    pub binary: &'a Path,
    pub arguments: &'a [&'a str],
    pub initialize_params: Value,
    /// Codex needs `initialized` after initialize.
    pub send_initialized: bool,
    pub method: &'a str,
    pub params: Value,
    pub initialize_timeout: Duration,
    pub request_timeout: Duration,
}

/// Short-lived CLI NDJSON-RPC; always reaps; errors never include raw bodies.
pub(super) fn call(request: Request<'_>) -> std::io::Result<Value> {
    let mut command = Command::new(request.binary);
    command
        .args(request.arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = ChildGuard::new(command.spawn()?);
    let Some(mut stdin) = child.get_mut().and_then(|child| child.stdin.take()) else {
        return Err(std::io::Error::other("provider stdin unavailable"));
    };
    let Some(stdout) = child.get_mut().and_then(|child| child.stdout.take()) else {
        drop(stdin);
        return Err(std::io::Error::other("provider stdout unavailable"));
    };
    let (tx, rx) = mpsc::channel::<std::io::Result<String>>();
    let reader = match std::thread::Builder::new()
        .name("ds-agent-usage-rpc".into())
        .spawn(move || read_lines(stdout, tx))
    {
        Ok(reader) => reader,
        Err(error) => {
            drop(stdin);
            return Err(error);
        }
    };

    let result = (|| {
        initialize(
            &mut stdin,
            &rx,
            request.initialize_params,
            request.initialize_timeout,
            request.send_initialized,
        )?;
        write_request(&mut stdin, 2, request.method, request.params)?;
        wait_for_id(&rx, 2, request.request_timeout)
    })();

    drop(stdin);
    child.stop();
    let _ = reader.join();
    result
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn get_mut(&mut self) -> Option<&mut Child> {
        self.0.as_mut()
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.0.take() {
            stop_child(&mut child);
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

fn initialize(
    stdin: &mut impl Write,
    rx: &mpsc::Receiver<std::io::Result<String>>,
    params: Value,
    timeout: Duration,
    send_initialized: bool,
) -> std::io::Result<()> {
    write_request(stdin, 1, "initialize", params)?;
    wait_for_id(rx, 1, timeout)?;
    if send_initialized {
        write_notification(stdin, "initialized")?;
    }
    Ok(())
}

fn read_lines(stdout: impl Read, tx: mpsc::Sender<std::io::Result<String>>) {
    let mut reader = BufReader::new(stdout);
    for _ in 0..MAX_RPC_MESSAGES {
        let mut bytes = Vec::new();
        let read = (&mut reader)
            .take((MAX_RPC_LINE_BYTES + 1) as u64)
            .read_until(b'\n', &mut bytes);
        let count = match read {
            Ok(0) => return,
            Ok(count) => count,
            Err(error) => {
                let _ = tx.send(Err(error));
                return;
            }
        };
        if count > MAX_RPC_LINE_BYTES {
            let _ = tx.send(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "provider RPC line exceeds size limit",
            )));
            return;
        }
        while matches!(bytes.last(), Some(b'\n' | b'\r')) {
            bytes.pop();
        }
        let line = String::from_utf8(bytes)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error));
        if tx.send(line).is_err() {
            return;
        }
    }
    let _ = tx.send(Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "provider RPC message limit exceeded",
    )));
}

fn write_request(
    stdin: &mut impl Write,
    id: i64,
    method: &str,
    params: Value,
) -> std::io::Result<()> {
    serde_json::to_writer(
        &mut *stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }),
    )
    .map_err(std::io::Error::other)?;
    stdin.write_all(b"\n")?;
    stdin.flush()
}

fn write_notification(stdin: &mut impl Write, method: &str) -> std::io::Result<()> {
    serde_json::to_writer(
        &mut *stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
        }),
    )
    .map_err(std::io::Error::other)?;
    stdin.write_all(b"\n")?;
    stdin.flush()
}

fn wait_for_id(
    rx: &mpsc::Receiver<std::io::Result<String>>,
    id: i64,
    timeout: Duration,
) -> std::io::Result<Value> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "provider RPC timed out",
            ));
        }
        let line = rx.recv_timeout(remaining).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "provider RPC timed out")
            }
            mpsc::RecvTimeoutError::Disconnected => {
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "provider RPC closed")
            }
        })??;
        if line.is_empty() {
            continue;
        }
        let message: Value = serde_json::from_str(&line)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if message.get("id").and_then(Value::as_i64) != Some(id) {
            continue;
        }
        if message.get("error").is_some() {
            return Err(std::io::Error::other("provider RPC returned an error"));
        }
        return message.get("result").cloned().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "provider RPC result missing",
            )
        });
    }
}

fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_through_notifications_and_returns_matching_result() {
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(r#"{"method":"notice","params":{}}"#.into()))
            .unwrap();
        tx.send(Ok(r#"{"id":2,"result":{"ok":true}}"#.into()))
            .unwrap();
        assert_eq!(
            wait_for_id(&rx, 2, Duration::from_secs(1)).unwrap()["ok"],
            true
        );
    }

    #[test]
    fn initialize_emits_required_acknowledgement() {
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(r#"{"id":1,"result":{}}"#.into())).unwrap();
        let mut written = Vec::new();

        initialize(
            &mut written,
            &rx,
            serde_json::json!({"clientInfo":{"name":"test"}}),
            Duration::from_secs(1),
            true,
        )
        .unwrap();

        let messages: Vec<Value> = std::str::from_utf8(&written)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["method"], "initialize");
        assert_eq!(messages[1]["method"], "initialized");
        assert!(messages[1].get("id").is_none());
        assert!(messages[1].get("params").is_none());
    }

    #[test]
    fn provider_errors_are_sanitized() {
        let (tx, rx) = mpsc::channel();
        tx.send(Ok(
            r#"{"id":2,"error":{"message":"secret response contents"}}"#.into(),
        ))
        .unwrap();
        let error = wait_for_id(&rx, 2, Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.to_string(), "provider RPC returned an error");
        assert!(!error.to_string().contains("secret"));
    }
}
