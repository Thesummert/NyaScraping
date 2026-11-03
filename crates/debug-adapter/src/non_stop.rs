//! Single-connection non-stop facade for probe-rs' all-stop GDB stub.
//! probe-rs accepts memory packets while running, but does not advertise QNonStop.
//! Terminate packet acknowledgements here and translate execution replies to
//! stop notifications. Never open a second probe session or synthesize a resume.
use nya_domain::{Error, Result};
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const MAX_PACKET: usize = 65536;

pub struct NonStopBridge {
    address: SocketAddr,
    quit: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl NonStopBridge {
    pub fn start(backend: SocketAddr) -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(io_error)?;
        listener.set_nonblocking(true).map_err(io_error)?;
        let address = listener.local_addr().map_err(io_error)?;
        let quit = Arc::new(AtomicBool::new(false));
        let stop = quit.clone();
        let worker = thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((front, _)) => {
                        if !stop.load(Ordering::Relaxed) {
                            if let Ok(back) =
                                TcpStream::connect_timeout(&backend, Duration::from_millis(500))
                            {
                                let _ = relay(front, back, stop.clone());
                            }
                        }
                        break;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            address,
            quit,
            worker: Some(worker),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for NonStopBridge {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn io_error(e: std::io::Error) -> Error {
    Error(format!("probe-rs non-stop 桥接：{e}"))
}

#[derive(Debug)]
enum Packet {
    Ack,
    Nack,
    Interrupt,
    Frame(Vec<u8>),
    Invalid,
}

#[derive(Default)]
struct Decoder {
    buffer: Vec<u8>,
}

impl Decoder {
    fn next(&mut self, byte: u8) -> Result<Option<Packet>> {
        if self.buffer.is_empty() {
            return Ok(match byte {
                b'+' => Some(Packet::Ack),
                b'-' => Some(Packet::Nack),
                3 => Some(Packet::Interrupt),
                b'$' | b'%' => {
                    self.buffer.push(byte);
                    None
                }
                _ => None,
            });
        }
        self.buffer.push(byte);
        if self.buffer.len() > MAX_PACKET {
            return Err(Error("RSP 报文超长".into()));
        }
        let len = self.buffer.len();
        if len >= 4 && self.buffer[len - 3] == b'#' {
            let frame = std::mem::take(&mut self.buffer);
            let checksum = frame[1..len - 3]
                .iter()
                .fold(0u8, |sum, b| sum.wrapping_add(*b));
            let received = std::str::from_utf8(&frame[len - 2..])
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok());
            return Ok(Some(if received == Some(checksum) {
                Packet::Frame(frame[1..len - 3].to_vec())
            } else {
                Packet::Invalid
            }));
        }
        Ok(None)
    }
}

fn frame(prefix: u8, body: &[u8]) -> Vec<u8> {
    let checksum = body.iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
    let mut packet = Vec::with_capacity(body.len() + 4);
    packet.push(prefix);
    packet.extend_from_slice(body);
    packet.extend_from_slice(format!("#{checksum:02x}").as_bytes());
    packet
}

fn read_packets(
    mut socket: TcpStream,
    front: bool,
    tx: mpsc::SyncSender<(bool, Option<Packet>)>,
    quit: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut decoder = Decoder::default();
        let mut bytes = [0u8; 8192];
        while !quit.load(Ordering::Relaxed) {
            match socket.read(&mut bytes) {
                Ok(0) => break,
                Ok(n) => {
                    for byte in &bytes[..n] {
                        match decoder.next(*byte) {
                            Ok(Some(packet)) => {
                                if tx.try_send((front, Some(packet))).is_err() {
                                    quit.store(true, Ordering::Relaxed);
                                    return;
                                }
                            }
                            Ok(None) => {}
                            Err(_) => {
                                let _ = tx.try_send((front, None));
                                return;
                            }
                        }
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => break,
            }
        }
        let _ = tx.try_send((front, None));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mi::Client;
    use nya_domain::SessionState;
    use std::{path::Path, process::Command};

    fn receive(stream: &mut TcpStream, decoder: &mut Decoder) -> Option<Packet> {
        loop {
            let mut byte = [0];
            if stream.read_exact(&mut byte).is_err() {
                return None;
            }
            if let Some(packet) = decoder.next(byte[0]).unwrap() {
                return Some(packet);
            }
        }
    }

    fn response(stream: &mut TcpStream, decoder: &mut Decoder) -> Vec<u8> {
        loop {
            match receive(stream, decoder) {
                Some(Packet::Frame(body)) => return body,
                Some(Packet::Ack) => {}
                other => panic!("unexpected RSP response: {other:?}"),
            }
        }
    }

    #[test]
    fn bridge_reads_while_running_and_preserves_stop_notifications() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let bridge = NonStopBridge::start(listener.local_addr().unwrap()).unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut decoder = Decoder::default();
            let mut reads = 0;
            let mut interrupts = 0;
            while let Some(packet) = receive(&mut socket, &mut decoder) {
                match packet {
                    Packet::Frame(body) => {
                        socket.write_all(b"+").unwrap();
                        match body.as_slice() {
                            b"qSupported" => socket
                                .write_all(&frame(b'$', b"PacketSize=4000;QStartNoAckMode+"))
                                .unwrap(),
                            b"vCont;c" => {}
                            b"qfThreadInfo" => socket.write_all(&frame(b'$', b"m1,2")).unwrap(),
                            b"m20000000,4" => {
                                if reads == 0 {
                                    socket.write_all(&frame(b'$', b"T05thread:1;")).unwrap();
                                }
                                reads += 1;
                                socket.write_all(&frame(b'$', b"2a000000")).unwrap();
                            }
                            other => panic!("sampling must not send control commands: {other:?}"),
                        }
                    }
                    Packet::Interrupt => {
                        interrupts += 1;
                        socket.write_all(&frame(b'$', b"T02thread:1;")).unwrap();
                    }
                    Packet::Ack => {}
                    other => panic!("unexpected packet {other:?}"),
                }
            }
            (reads, interrupts)
        });
        let mut front = TcpStream::connect(bridge.address()).unwrap();
        front
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut decoder = Decoder::default();
        for (command, expected) in [
            (
                b"qSupported".as_slice(),
                b"PacketSize=4000;QNonStop+;QStartNoAckMode+".as_slice(),
            ),
            (b"QNonStop:1", b"OK"),
            (b"QStartNoAckMode", b"OK"),
            (b"qfThreadInfo", b"m1,2"),
            (b"vCont;c", b"OK"),
        ] {
            front.write_all(&frame(b'$', command)).unwrap();
            assert_eq!(response(&mut front, &mut decoder), expected);
        }
        front.write_all(&frame(b'$', b"m20000000,4")).unwrap();
        assert_eq!(response(&mut front, &mut decoder), b"Stop:T05thread:1;");
        assert_eq!(response(&mut front, &mut decoder), b"2a000000");
        front.write_all(&frame(b'$', b"vStopped")).unwrap();
        // probe-rs halts every exposed core on a stop. GDB must see each thread stop.
        assert_eq!(response(&mut front, &mut decoder), b"T02thread:2;");
        front.write_all(&frame(b'$', b"vStopped")).unwrap();
        assert_eq!(response(&mut front, &mut decoder), b"OK");
        front.write_all(&frame(b'$', b"m20000000,4")).unwrap();
        assert_eq!(response(&mut front, &mut decoder), b"2a000000");
        // Only a user-requested pause emits Ctrl-C, then delivers its actual stop event.
        front.write_all(&frame(b'$', b"vCont;t")).unwrap();
        assert_eq!(response(&mut front, &mut decoder), b"OK");
        assert_eq!(response(&mut front, &mut decoder), b"Stop:T02thread:1;");
        drop(front);
        drop(bridge);
        assert_eq!(server.join().unwrap(), (2, 1));
    }

    #[test]
    fn decoder_validates_checksums_and_bounds_partial_frames() {
        let mut decoder = Decoder::default();
        let valid = frame(b'$', b"m20000000,4");
        for byte in &valid[..valid.len() - 1] {
            assert!(decoder.next(*byte).unwrap().is_none());
        }
        assert!(matches!(
            decoder.next(*valid.last().unwrap()).unwrap(),
            Some(Packet::Frame(_))
        ));
        for byte in b"$x#00" {
            if let Some(packet) = decoder.next(*byte).unwrap() {
                assert!(matches!(packet, Packet::Invalid));
            }
        }
        decoder.next(b'$').unwrap();
        for _ in 1..MAX_PACKET {
            decoder.next(b'x').unwrap();
        }
        assert!(decoder.next(b'x').is_err());
    }

    fn target_xml() -> Vec<u8> {
        let mut xml = String::from("<?xml version=\"1.0\"?><!DOCTYPE target SYSTEM \"gdb-target.dtd\"><target><architecture>arm</architecture><feature name=\"org.gnu.gdb.arm.m-profile\">");
        for i in 0..13 {
            xml.push_str(&format!(
                "<reg name=\"r{i}\" bitsize=\"32\" regnum=\"{i}\"/>"
            ));
        }
        xml.push_str("<reg name=\"sp\" bitsize=\"32\" regnum=\"13\" type=\"data_ptr\"/><reg name=\"lr\" bitsize=\"32\" regnum=\"14\"/><reg name=\"pc\" bitsize=\"32\" regnum=\"15\" type=\"code_ptr\"/><reg name=\"xpsr\" bitsize=\"32\" regnum=\"25\"/></feature></target>");
        xml.into_bytes()
    }

    #[test]
    fn real_gdb_non_stop_reads_use_the_same_rsp_connection() {
        let gdb = std::env::var("NYA_TEST_GDB").unwrap_or_else(|_| "arm-none-eabi-gdb".into());
        if Command::new(&gdb).arg("--version").output().is_err() {
            eprintln!("real GDB non-stop test skipped: requires arm-none-eabi-gdb or NYA_TEST_GDB");
            return;
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let bridge = NonStopBridge::start(listener.local_addr().unwrap()).unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut decoder = Decoder::default();
            let xml = target_xml();
            let mut running = false;
            let mut reads = 0u32;
            while let Some(packet) = receive(&mut socket, &mut decoder) {
                match packet {
                    Packet::Frame(body) => {
                        socket.write_all(b"+").unwrap();
                        let command = String::from_utf8_lossy(&body);
                        let reply = if command.starts_with("qSupported") {
                            b"PacketSize=4000;qXfer:features:read+".to_vec()
                        } else if let Some(range) =
                            command.strip_prefix("qXfer:features:read:target.xml:")
                        {
                            let (offset, size) = range.split_once(',').unwrap();
                            let offset = usize::from_str_radix(offset, 16).unwrap();
                            let size = usize::from_str_radix(size, 16).unwrap();
                            let end = (offset + size).min(xml.len());
                            let mut response = vec![if end == xml.len() { b'l' } else { b'm' }];
                            response.extend_from_slice(&xml[offset..end]);
                            response
                        } else if command == "?" {
                            b"T05thread:1;".to_vec()
                        } else if command == "qfThreadInfo" {
                            b"m1".to_vec()
                        } else if command == "qsThreadInfo" {
                            b"l".to_vec()
                        } else if command == "qC" {
                            b"QC1".to_vec()
                        } else if command == "qAttached" {
                            b"1".to_vec()
                        } else if command.starts_with('H') {
                            b"OK".to_vec()
                        } else if command == "vCont?" {
                            b"vCont;c;s".to_vec()
                        } else if command.starts_with("vCont;c") {
                            running = true;
                            continue;
                        } else if let Some(range) = command.strip_prefix('m') {
                            let (address, size) = range.split_once(',').unwrap();
                            let address = u64::from_str_radix(address, 16).unwrap();
                            let size = usize::from_str_radix(size, 16).unwrap();
                            if address == 0x20000000 {
                                assert!(running, "RAM reads must happen while the core runs");
                                reads += 1;
                                if reads == 2 {
                                    running = false;
                                    socket.write_all(&frame(b'$', b"T05thread:1;")).unwrap();
                                }
                                reads
                                    .to_le_bytes()
                                    .iter()
                                    .map(|b| format!("{b:02x}"))
                                    .collect::<String>()
                                    .into_bytes()
                            } else {
                                "00".repeat(size).into_bytes()
                            }
                        } else if command == "g" {
                            let mut registers = [0u8; 68];
                            registers[60..64].copy_from_slice(&0x08000000u32.to_le_bytes());
                            registers[64..].copy_from_slice(&0x01000000u32.to_le_bytes());
                            registers
                                .iter()
                                .map(|b| format!("{b:02x}"))
                                .collect::<String>()
                                .into_bytes()
                        } else if command.starts_with('p') {
                            b"00000000".to_vec()
                        } else if command == "D" {
                            b"OK".to_vec()
                        } else {
                            Vec::new()
                        };
                        if socket.write_all(&frame(b'$', &reply)).is_err() {
                            break;
                        }
                    }
                    Packet::Ack => {}
                    Packet::Interrupt => {
                        // In all-stop-on-non-stop mode an actual breakpoint may
                        // prompt GDB to stop remaining threads; reads never do.
                        assert!(
                            !running,
                            "memory sampling must not interrupt a running target"
                        );
                        socket.write_all(&frame(b'$', b"S02")).unwrap();
                    }
                    _ => {}
                }
            }
            reads
        });
        let mut client = Client::spawn(Path::new(&gdb)).unwrap();
        for command in [
            "-gdb-set pagination off",
            "-gdb-set confirm off",
            "-gdb-set mi-async on",
            "-interpreter-exec console \"maintenance set target-non-stop on\"",
        ] {
            client.command(command).unwrap();
        }
        client
            .command(&format!("-target-select remote {}", bridge.address()))
            .unwrap();
        client.command("-exec-continue").unwrap();
        for value in [1, 2] {
            let response = client
                .command("-data-read-memory-bytes 0x20000000 4")
                .unwrap();
            assert_eq!(
                response.get("memory").unwrap().list()[0].field("contents"),
                format!("{value:02x}000000")
            );
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while client.state != SessionState::Halted {
            assert!(
                std::time::Instant::now() < deadline,
                "a stop racing with memory reads must reach GDB"
            );
            client.poll().unwrap();
            thread::sleep(Duration::from_millis(1));
        }
        drop(client);
        drop(bridge);
        assert_eq!(server.join().unwrap(), 2);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Query,
    Supported,
    VCont,
    Resume,
    StopReason,
}

struct Relay {
    front: TcpStream,
    back: TcpStream,
    no_ack: bool,
    non_stop: bool,
    queue: VecDeque<Vec<u8>>,
    pending: Option<Kind>,
    last_front: Vec<u8>,
    last_back: Vec<u8>,
    retries: u8,
    stops: VecDeque<Vec<u8>>,
    current_thread: Vec<u8>,
    threads: Vec<Vec<u8>>,
}

impl Relay {
    fn reply(&mut self, prefix: u8, body: &[u8]) -> Result<()> {
        self.last_front = frame(prefix, body);
        self.front.write_all(&self.last_front).map_err(io_error)
    }

    fn dispatch(&mut self) -> Result<()> {
        if self.pending.is_some() {
            return Ok(());
        }
        let Some(body) = self.queue.pop_front() else {
            return Ok(());
        };
        self.pending = Some(if body.starts_with(b"qSupported") {
            Kind::Supported
        } else if body == b"vCont?" {
            Kind::VCont
        } else if body == b"?" {
            Kind::StopReason
        } else if self.non_stop
            && (body.starts_with(b"vCont;c")
                || body.starts_with(b"vCont;s")
                || body.starts_with(b"vCont;C")
                || body.starts_with(b"vCont;S")
                || matches!(body.first(), Some(b'c' | b's' | b'C' | b'S')))
        {
            Kind::Resume
        } else {
            Kind::Query
        });
        self.last_back = frame(b'$', &body);
        self.retries = 0;
        self.back.write_all(&self.last_back).map_err(io_error)
    }

    fn front(&mut self, packet: Packet) -> Result<()> {
        match packet {
            Packet::Frame(body) => {
                if body.starts_with(b"Hg")
                    && body.len() > 2
                    && body[2..] != *b"0"
                    && body[2..] != *b"-1"
                {
                    self.current_thread = body[2..].to_vec();
                }
                if !self.no_ack {
                    self.front.write_all(b"+").map_err(io_error)?;
                }
                if body == b"QStartNoAckMode" {
                    self.reply(b'$', b"OK")?;
                    self.no_ack = true;
                } else if body == b"QNonStop:1" || body == b"QNonStop:0" {
                    self.non_stop = body.ends_with(b"1");
                    self.reply(b'$', b"OK")?;
                } else if self.non_stop && body == b"vStopped" {
                    self.stops.pop_front();
                    let response = self
                        .stops
                        .front()
                        .cloned()
                        .unwrap_or_else(|| b"OK".to_vec());
                    self.reply(b'$', &response)?;
                } else if self.non_stop && body.starts_with(b"vCont;t") {
                    // Only an explicit GDB pause becomes an interrupt.
                    self.back.write_all(&[3]).map_err(io_error)?;
                    self.reply(b'$', b"OK")?;
                } else {
                    if self.queue.len() >= 16 {
                        return Err(Error("RSP 请求队列超出限制".into()));
                    }
                    self.queue.push_back(body);
                }
            }
            Packet::Interrupt => self.back.write_all(&[3]).map_err(io_error)?,
            Packet::Invalid => {
                if !self.no_ack {
                    self.front.write_all(b"-").map_err(io_error)?;
                }
            }
            Packet::Nack => self.front.write_all(&self.last_front).map_err(io_error)?,
            Packet::Ack => {}
        }
        self.dispatch()
    }

    fn back(&mut self, packet: Packet) -> Result<()> {
        match packet {
            Packet::Ack if self.pending == Some(Kind::Resume) => {
                // An all-stop resume has no synchronous result beyond this ACK.
                self.pending = None;
                self.reply(b'$', b"OK")?;
            }
            Packet::Frame(mut body) => {
                self.back.write_all(b"+").map_err(io_error)?;
                let stop = matches!(body.first(), Some(b'T' | b'S' | b'W' | b'X'));
                if self.non_stop
                    && matches!(body.first(), Some(b'S' | b'T'))
                    && !body.windows(7).any(|w| w == b"thread:")
                {
                    if body.first() == Some(&b'S') {
                        body[0] = b'T';
                    }
                    if body.len() > 3 && !body.ends_with(b";") {
                        body.push(b';');
                    }
                    body.extend_from_slice(b"thread:");
                    body.extend_from_slice(&self.current_thread);
                    body.push(b';');
                }
                if self.non_stop && stop && self.pending != Some(Kind::StopReason) {
                    if self.stops.len() >= 64 {
                        return Err(Error("RSP 停止事件超出限制".into()));
                    }
                    if self.stops.is_empty() {
                        let mut notification = b"Stop:".to_vec();
                        notification.extend_from_slice(&body);
                        self.reply(b'%', &notification)?;
                    }
                    let others: Vec<Vec<u8>> = if body.first() == Some(&b'T') {
                        let thread = body.windows(7).position(|s| s == b"thread:").map(|offset| {
                            body[offset + 7..]
                                .split(|b| *b == b';')
                                .next()
                                .unwrap_or_default()
                        });
                        self.threads
                            .iter()
                            .filter(|tid| Some(tid.as_slice()) != thread)
                            .map(|tid| {
                                let mut reply = b"T02thread:".to_vec();
                                reply.extend_from_slice(tid);
                                reply.push(b';');
                                reply
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    if self.stops.len() + 1 + others.len() > 64 {
                        return Err(Error("RSP 停止事件超出限制".into()));
                    }
                    self.stops.push_back(body);
                    self.stops.extend(others);
                } else if body.starts_with(b"O") && body != b"OK" {
                    self.reply(b'$', &body)?;
                } else {
                    if self.pending == Some(Kind::Supported) {
                        let text = std::str::from_utf8(&body)
                            .map_err(|_| Error("无效的 qSupported 响应".into()))?;
                        let features: Vec<_> = text
                            .split(';')
                            .filter(|f| {
                                !f.starts_with("QNonStop") && !f.starts_with("QStartNoAckMode")
                            })
                            .collect();
                        body = format!("{};QNonStop+;QStartNoAckMode+", features.join(";"))
                            .into_bytes();
                    } else if self.pending == Some(Kind::VCont)
                        && !body.split(|b| *b == b';').any(|s| s == b"t")
                    {
                        body.extend_from_slice(b";t");
                    }
                    if self.last_back.starts_with(b"$qC#") && body.starts_with(b"QC") {
                        self.current_thread = body[2..].to_vec();
                    } else if (self.last_back.starts_with(b"$qfThreadInfo#")
                        || self.last_back.starts_with(b"$qsThreadInfo#"))
                        && body.starts_with(b"m")
                    {
                        if self.last_back.starts_with(b"$qfThreadInfo#") {
                            self.threads.clear();
                        }
                        self.threads.extend(
                            body[1..]
                                .split(|b| *b == b',')
                                .filter(|s| !s.is_empty())
                                .map(|s| s.to_vec()),
                        );
                        if self.threads.len() > 64 {
                            return Err(Error("RSP 线程数超出限制".into()));
                        }
                        if let Some(tid) = self.threads.first() {
                            self.current_thread = tid.clone();
                        }
                    }
                    self.pending = None;
                    self.reply(b'$', &body)?;
                }
            }
            Packet::Invalid => self.back.write_all(b"-").map_err(io_error)?,
            Packet::Nack => {
                self.retries += 1;
                if self.retries > 2 {
                    return Err(Error("RSP 重传次数超出限制".into()));
                }
                self.back.write_all(&self.last_back).map_err(io_error)?;
            }
            Packet::Interrupt | Packet::Ack => {}
        }
        self.dispatch()
    }
}

fn relay(front: TcpStream, back: TcpStream, quit: Arc<AtomicBool>) -> Result<()> {
    for socket in [&front, &back] {
        socket.set_nodelay(true).map_err(io_error)?;
        socket
            .set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(io_error)?;
        socket
            .set_write_timeout(Some(Duration::from_millis(500)))
            .map_err(io_error)?;
    }
    let (tx, rx) = mpsc::sync_channel(256);
    let front_reader = read_packets(
        front.try_clone().map_err(io_error)?,
        true,
        tx.clone(),
        quit.clone(),
    );
    let back_reader = read_packets(back.try_clone().map_err(io_error)?, false, tx, quit.clone());
    let mut relay = Relay {
        front,
        back,
        no_ack: false,
        non_stop: false,
        queue: VecDeque::new(),
        pending: None,
        last_front: Vec::new(),
        last_back: Vec::new(),
        retries: 0,
        stops: VecDeque::new(),
        current_thread: b"1".to_vec(),
        threads: vec![b"1".to_vec()],
    };
    let result = (|| {
        while !quit.load(Ordering::Relaxed) {
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok((true, Some(packet))) => relay.front(packet)?,
                Ok((false, Some(packet))) => relay.back(packet)?,
                Ok((_, None)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        Ok(())
    })();
    quit.store(true, Ordering::Relaxed);
    let _ = relay.front.shutdown(Shutdown::Both);
    let _ = relay.back.shutdown(Shutdown::Both);
    let _ = front_reader.join();
    let _ = back_reader.join();
    result
}
