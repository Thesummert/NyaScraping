//! Bounded GDB/MI transport. Result tokens and async execution records are independent.
use crate::error;
use nya_domain::{Error, Result, SessionState};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    Tuple(Vec<(String, Value)>),
    List(Vec<Value>),
}
impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Tuple(fields) => fields.iter().find(|(name, _)| name == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn text(&self) -> &str {
        if let Self::Text(value) = self {
            value
        } else {
            ""
        }
    }
    pub fn field(&self, key: &str) -> &str {
        self.get(key).map(Self::text).unwrap_or_default()
    }
    pub fn list(&self) -> &[Value] {
        if let Self::List(items) = self {
            items
        } else {
            &[]
        }
    }
}
#[derive(Debug)]
pub struct Record {
    pub token: Option<u64>,
    pub kind: u8,
    pub class: String,
    pub data: Value,
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl Parser<'_> {
    fn peek(&self) -> u8 {
        self.bytes.get(self.pos).copied().unwrap_or(0)
    }
    fn word(&mut self) -> String {
        let start = self.pos;
        while self.peek().is_ascii_alphanumeric() || matches!(self.peek(), b'-' | b'_' | b'.') {
            self.pos += 1;
        }
        String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned()
    }
    fn string(&mut self) -> Result<String> {
        if self.peek() != b'"' {
            return Err(Error("Expected MI string".into()));
        }
        self.pos += 1;
        let mut bytes = Vec::new();
        loop {
            let c = self.peek();
            self.pos += 1;
            match c {
                0 => return Err(Error("Unterminated MI string".into())),
                b'"' => break,
                b'\\' => {
                    let escape = self.peek();
                    self.pos += 1;
                    bytes.push(match escape {
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'b' => 8,
                        b'f' => 12,
                        b'v' => 11,
                        b'a' => 7,
                        b'0'..=b'7' => {
                            let mut value = (escape - b'0') as u16;
                            for _ in 0..2 {
                                if matches!(self.peek(), b'0'..=b'7') {
                                    value = value * 8 + (self.peek() - b'0') as u16;
                                    self.pos += 1;
                                } else {
                                    break;
                                }
                            }
                            value as u8
                        }
                        0 => return Err(Error("Unterminated MI escape".into())),
                        other => other,
                    });
                }
                other => bytes.push(other),
            }
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
    fn fields(&mut self, end: u8, depth: usize) -> Result<Vec<(String, Value)>> {
        let mut fields = Vec::new();
        while self.peek() != end && self.peek() != 0 {
            let key = self.word();
            if key.is_empty() || self.peek() != b'=' {
                return Err(Error("Invalid MI result".into()));
            }
            self.pos += 1;
            fields.push((key, self.value(depth + 1)?));
            if self.peek() != b',' {
                break;
            }
            self.pos += 1;
        }
        Ok(fields)
    }
    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth > 64 {
            return Err(Error("MI nesting limit exceeded".into()));
        }
        match self.peek() {
            b'"' => Ok(Value::Text(self.string()?)),
            b'{' => {
                self.pos += 1;
                let fields = self.fields(b'}', depth)?;
                if self.peek() != b'}' {
                    return Err(Error("Unterminated MI tuple".into()));
                }
                self.pos += 1;
                Ok(Value::Tuple(fields))
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.peek() != b']' {
                    if self.peek() == 0 {
                        return Err(Error("Unterminated MI list".into()));
                    }
                    if matches!(self.peek(), b'"' | b'{' | b'[') {
                        items.push(self.value(depth + 1)?);
                    } else {
                        let key = self.word();
                        if key.is_empty() || self.peek() != b'=' {
                            return Err(Error("Invalid MI list result".into()));
                        }
                        self.pos += 1;
                        items.push(Value::Tuple(vec![(key, self.value(depth + 1)?)]));
                    }
                    if self.peek() != b',' {
                        break;
                    }
                    self.pos += 1;
                }
                if self.peek() != b']' {
                    return Err(Error("Unterminated MI list".into()));
                }
                self.pos += 1;
                Ok(Value::List(items))
            }
            _ => Err(Error("Invalid MI value".into())),
        }
    }
}

pub fn parse(line: &str) -> Result<Option<Record>> {
    let mut p = Parser {
        bytes: line.trim_end().as_bytes(),
        pos: 0,
    };
    if p.bytes == b"(gdb)" || p.bytes.is_empty() {
        return Ok(None);
    }
    let start = p.pos;
    while p.peek().is_ascii_digit() {
        p.pos += 1;
    }
    let token = if p.pos > start {
        Some(line[start..p.pos].parse().map_err(error)?)
    } else {
        None
    };
    let kind = p.peek();
    p.pos += 1;
    if matches!(kind, b'~' | b'@' | b'&') {
        return Ok(Some(Record {
            token,
            kind,
            class: String::new(),
            data: Value::Text(p.string()?),
        }));
    }
    if !matches!(kind, b'^' | b'*' | b'=' | b'+') {
        return Ok(None);
    }
    let class = p.word();
    let data = if p.peek() == b',' {
        p.pos += 1;
        // GDB emits download status as +download,{section="...",...},
        // with an anonymous tuple instead of the usual list of results.
        if kind == b'+' && p.peek() == b'{' {
            p.value(0)?
        } else {
            Value::Tuple(p.fields(0, 0)?)
        }
    } else {
        Value::Tuple(vec![])
    };
    if p.pos != p.bytes.len() {
        return Err(Error("Trailing MI data".into()));
    }
    Ok(Some(Record {
        token,
        kind,
        class,
        data,
    }))
}

pub fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for byte in text.bytes() {
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0..=31 | 127 => out.push_str(&format!("\\{byte:03o}")),
            128..=255 => out.push_str(&format!("\\{byte:03o}")),
            other => out.push(other as char),
        }
    }
    out.push('"');
    out
}

pub struct Client {
    child: Child,
    input: ChildStdin,
    output: mpsc::Receiver<String>,
    overflow: Arc<AtomicBool>,
    next_token: u64,
    pub state: SessionState,
    pub stop: Option<Value>,
    pub changed: bool,
    pub stop_generation: u64,
    pub logs: VecDeque<String>,
}

fn reader(
    mut pipe: impl Read + Send + 'static,
    sender: mpsc::SyncSender<String>,
    overflow: Arc<AtomicBool>,
    stderr: bool,
) {
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        let mut line = Vec::new();
        loop {
            let n = match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            for byte in &buffer[..n] {
                if *byte == b'\n' {
                    let text = String::from_utf8_lossy(&line);
                    let text = if stderr {
                        format!("&{}", quote(&text))
                    } else {
                        text.into_owned()
                    };
                    match sender.try_send(text) {
                        Ok(()) => {}
                        Err(mpsc::TrySendError::Disconnected(_)) => return,
                        Err(mpsc::TrySendError::Full(_)) => {
                            overflow.store(true, Ordering::Relaxed);
                            return;
                        }
                    }
                    line.clear();
                } else {
                    line.push(*byte);
                }
                if line.len() > 64 * 1024 {
                    overflow.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }
    });
}

impl Client {
    pub fn spawn(gdb: &Path) -> Result<Self> {
        let mut child = Command::new(gdb)
            .args(["--interpreter=mi2", "--nx", "--quiet"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(error)?;
        let input = child.stdin.take().expect("piped stdin");
        let (sender, output) = mpsc::sync_channel(256);
        let overflow = Arc::new(AtomicBool::new(false));
        reader(
            child.stdout.take().expect("piped stdout"),
            sender.clone(),
            overflow.clone(),
            false,
        );
        reader(
            child.stderr.take().expect("piped stderr"),
            sender,
            overflow.clone(),
            true,
        );
        Ok(Self {
            child,
            input,
            output,
            overflow,
            next_token: 1,
            state: SessionState::Attaching,
            stop: None,
            changed: true,
            stop_generation: 0,
            logs: VecDeque::new(),
        })
    }
    fn accept(&mut self, record: &Record) {
        match (record.kind, record.class.as_str()) {
            (b'*' | b'^', "running") => {
                self.state = SessionState::Running;
                self.stop = None;
                self.changed = true;
            }
            (b'*', "stopped") => {
                self.state = if record.data.field("reason").starts_with("exited") {
                    SessionState::Disconnected
                } else {
                    SessionState::Halted
                };
                self.stop_generation += 1;
                self.stop = Some(record.data.clone());
                self.changed = true;
            }
            (b'=', "thread-group-exited") => {
                self.state = SessionState::Disconnected;
                self.changed = true;
            }
            (b'~' | b'&' | b'@', _) => {
                self.changed = true;
                self.logs
                    .push_back(record.data.text().trim_end().to_owned());
                if self.logs.len() > 200 {
                    self.logs.pop_front();
                }
            }
            _ => {}
        }
    }
    fn check(&mut self) -> Result<()> {
        if self.state == SessionState::Faulted {
            return Err(Error("GDB 会话已失效，请重新连接".into()));
        }
        if self.overflow.load(Ordering::Relaxed) {
            return Err(Error("GDB output limit exceeded".into()));
        }
        if let Some(status) = self.child.try_wait().map_err(error)? {
            return Err(Error(format!("GDB exited: {status}")));
        }
        Ok(())
    }
    pub fn poll(&mut self) -> Result<bool> {
        self.check()?;
        while let Ok(line) = self.output.try_recv() {
            if let Some(record) = parse(&line)? {
                self.accept(&record);
            }
        }
        Ok(std::mem::take(&mut self.changed))
    }
    pub fn command(&mut self, command: &str) -> Result<Value> {
        self.command_timeout(command, Duration::from_secs(8))
    }
    pub fn command_timeout(&mut self, command: &str, timeout: Duration) -> Result<Value> {
        self.command_timeout_observe(command, timeout, |_| {})
    }
    pub fn command_timeout_observe(
        &mut self,
        command: &str,
        timeout: Duration,
        mut observe: impl FnMut(&Record),
    ) -> Result<Value> {
        self.check()?;
        let token = self.next_token;
        self.next_token += 1;
        writeln!(self.input, "{token}{command}").map_err(error)?;
        self.input.flush().map_err(error)?;
        let start = Instant::now();
        loop {
            let Some(remaining) = timeout.checked_sub(start.elapsed()) else {
                self.state = SessionState::Faulted;
                self.changed = true;
                return Err(Error(format!("GDB command timed out: {command}")));
            };
            let line = self
                .output
                .recv_timeout(remaining.min(Duration::from_millis(100)));
            match line {
                Ok(line) => {
                    if let Some(record) = parse(&line)? {
                        self.accept(&record);
                        observe(&record);
                        if record.kind == b'^' && record.token == Some(token) {
                            if record.class == "error" {
                                return Err(Error(record.data.field("msg").into()));
                            }
                            if !matches!(
                                record.class.as_str(),
                                "done" | "running" | "connected" | "exit"
                            ) {
                                return Err(Error(format!(
                                    "Unexpected GDB result: {}",
                                    record.class
                                )));
                            }
                            return Ok(record.data);
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => self.check()?,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Error("GDB output closed".into()))
                }
            }
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn download_status_tuple_is_parsed() {
        let record = parse(r#"+download,{section=".text",section-sent="512",section-size="6668",total-sent="512",total-size="9880"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(record.kind, b'+');
        assert_eq!(record.class, "download");
        assert_eq!(record.data.field("section"), ".text");
        assert_eq!(record.data.field("total-sent"), "512");
        assert!(parse(r#"+download,{section=".text""#).is_err());
    }
    #[test]
    fn nested_lists_duplicates_and_utf8_escapes() {
        let record = parse(r#"23^done,stack=[frame={level="0",func="main",fullname="C:\\src\\a.c",line="7"},frame={level="1",func="worker"}]"#).unwrap().unwrap();
        assert_eq!(record.token, Some(23));
        let stack = record.data.get("stack").unwrap().list();
        assert_eq!(stack.len(), 2);
        assert_eq!(
            stack[0].get("frame").unwrap().field("fullname"),
            r"C:\src\a.c"
        );
        assert_eq!(
            parse(&format!("~{}", quote("中文\n\"\\")))
                .unwrap()
                .unwrap()
                .data
                .text(),
            "中文\n\"\\"
        );
        assert!(parse(r#"1^done,value={broken"#).is_err());
        assert!(parse("(gdb) ").unwrap().is_none());
    }
}

#[cfg(all(test, unix))]
mod process_tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn timeout_poisoning_and_child_reaping() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("fake-gdb");
        fs::write(&executable, include_bytes!("../tests/fixtures/fake_gdb.py")).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let mut client = Client::spawn(&executable).unwrap();
        #[cfg(target_os = "linux")]
        let pid = client.child.id();
        assert!(client
            .command_timeout("-test-timeout", Duration::from_millis(100))
            .is_err());
        assert_eq!(client.state, SessionState::Faulted);
        assert!(client.command("-gdb-version").is_err());
        drop(client);
        #[cfg(target_os = "linux")]
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn real_gdb_symbols_breakpoints_and_expression_types() {
        if Command::new("gdb").arg("--version").output().is_err()
            || Command::new("cc").arg("--version").output().is_err()
        {
            eprintln!("real GDB test skipped: requires gdb and cc");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("test source.c");
        let binary = root.path().join("test program");
        fs::write(&source, "struct Sensor { int value; }; struct Sensor sensor = {42}; int main(void) { return sensor.value; }\n").unwrap();
        assert!(Command::new("cc")
            .args(["-g", "-O0"])
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .status()
            .unwrap()
            .success());
        let mut client = Client::spawn(Path::new("gdb")).unwrap();
        client
            .command(&format!(
                "-file-exec-and-symbols {}",
                quote(&binary.to_string_lossy())
            ))
            .unwrap();
        let bp = client
            .command(&format!("-break-insert {}", quote("main")))
            .unwrap();
        assert!(!bp.get("bkpt").unwrap().field("number").is_empty());
        let variable = client
            .command(&format!("-var-create - * {}", quote("2 + 3")))
            .unwrap();
        assert_eq!(variable.field("type"), "int");
        assert_eq!(variable.field("value"), "5");
        client
            .command(&format!("-var-delete {}", quote(variable.field("name"))))
            .unwrap();
        let value = client
            .command(&format!(
                "-data-evaluate-expression {}",
                quote("sizeof(struct Sensor)")
            ))
            .unwrap();
        let qualified = format!("sizeof('{}'::sensor)", source.to_string_lossy());
        let qualified_value = client
            .command(&format!("-data-evaluate-expression {}", quote(&qualified)))
            .unwrap();
        assert_eq!(qualified_value.field("value"), "4");
        assert_eq!(value.field("value"), "4");
        assert!(client
            .command(&format!("-var-create - * {}", quote("no_such_symbol")))
            .is_err());
        assert!(client.command("-gdb-version").is_ok());
    }
}
