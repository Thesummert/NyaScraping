//! Sampling owns its socket on a dedicated thread; UI polls bounded shared buffers.
use nya_debug_adapter::live_memory::OpenOcdMemoryReader;
use nya_domain::waveform::{LiveMemoryReader, MemoryChannel, ReadPlan, Sample, SampleBuffer};
use std::{
    net::SocketAddr,
    num::NonZeroUsize,
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

pub const CAPACITY: usize = 32768;

pub struct ChannelData {
    pub id: usize,
    pub memory: MemoryChannel,
    pub samples: SampleBuffer,
    pub invalid: u64,
}

pub struct State {
    pub channels: Vec<ChannelData>,
    pub endpoint: Option<SocketAddr>,
    pub status: String,
    pub revision: u64,
    pub sequence: u64,
    pub epoch: Instant,
    pub elapsed: Duration,
    pub sweeps: u64,
    pub quit: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            channels: Vec::new(),
            endpoint: None,
            status: "采集未启动".into(),
            revision: 0,
            sequence: 0,
            epoch: Instant::now(),
            elapsed: Duration::ZERO,
            sweeps: 0,
            quit: false,
        }
    }
}

#[derive(Default)]
pub struct Acquisition {
    pub state: Mutex<State>,
    changed: Condvar,
}

impl Acquisition {
    pub fn set_targets(&self, targets: Vec<(usize, MemoryChannel)>) {
        let mut state = self.state.lock().expect("acquisition lock");
        if state
            .channels
            .iter()
            .map(|c| (c.id, &c.memory))
            .eq(targets.iter().map(|(id, m)| (*id, m)))
        {
            return;
        }
        let mut previous = std::mem::take(&mut state.channels);
        state.channels = targets
            .into_iter()
            .map(|(id, memory)| {
                if let Some(index) = previous
                    .iter()
                    .position(|c| c.id == id && c.memory == memory)
                {
                    previous.swap_remove(index)
                } else {
                    ChannelData {
                        id,
                        memory,
                        samples: SampleBuffer::new(NonZeroUsize::new(CAPACITY).unwrap()),
                        invalid: 0,
                    }
                }
            })
            .collect();
        state.revision += 1;
        state.sequence += 1;
        self.changed.notify_one();
    }

    pub fn start(&self, endpoint: SocketAddr) {
        let mut state = self.state.lock().expect("acquisition lock");
        state.endpoint = Some(endpoint);
        state.status = "正在连接 OpenOCD…".into();
        state.revision += 1;
        state.sequence += 1;
        state.epoch = Instant::now();
        state.elapsed = Duration::ZERO;
        state.sweeps = 0;
        for channel in &mut state.channels {
            channel.samples.clear();
            channel.invalid = 0;
        }
        self.changed.notify_one();
    }

    pub fn stop(&self) {
        let mut state = self.state.lock().expect("acquisition lock");
        state.endpoint = None;
        state.status = "采集已停止，保留已有波形".into();
        state.revision += 1;
        state.sequence += 1;
        self.changed.notify_one();
    }

    pub fn shutdown(&self) {
        let mut state = self.state.lock().expect("acquisition lock");
        state.quit = true;
        self.changed.notify_one();
    }

    pub fn run(&self) {
        let mut reader: Option<OpenOcdMemoryReader> = None;
        let mut connection = None;
        loop {
            let (revision, endpoint, epoch, channels) = {
                let mut state = self.state.lock().expect("acquisition lock");
                while !state.quit && (state.endpoint.is_none() || state.channels.is_empty()) {
                    reader = None;
                    connection = None;
                    state = self.changed.wait(state).expect("acquisition lock");
                }
                if state.quit {
                    return;
                }
                (
                    state.revision,
                    state.endpoint.unwrap(),
                    state.epoch,
                    state
                        .channels
                        .iter()
                        .map(|c| c.memory.clone())
                        .collect::<Vec<_>>(),
                )
            };
            let result = (|| {
                let plan = ReadPlan::new(&channels)?;
                if reader.is_none() || connection != Some(endpoint) {
                    reader = Some(OpenOcdMemoryReader::connect(endpoint)?);
                    connection = Some(endpoint);
                }
                loop {
                    let begin = epoch.elapsed();
                    let blocks = reader.as_mut().unwrap().read_blocks(&plan.blocks)?;
                    let finish = epoch.elapsed();
                    let elapsed = begin + (finish - begin) / 2;
                    let mut state = self.state.lock().expect("acquisition lock");
                    // A changed selection, firmware or connection invalidates in-flight reads.
                    if state.quit {
                        return Ok(());
                    }
                    if state.revision != revision {
                        break;
                    }
                    for (index, channel) in state.channels.iter_mut().enumerate() {
                        let (block, offset) = plan.locations[index];
                        let value = channel
                            .memory
                            .decode(&blocks[block][offset..offset + channel.memory.byte_size])?;
                        if channel.samples.push(Sample { elapsed, value }).is_err() {
                            channel.invalid += 1;
                        }
                    }
                    state.sweeps += 1;
                    state.elapsed = elapsed;
                    state.sequence += 1;
                    if state.sweeps == 1 {
                        state.status = "运行中内存采集 · 全速".into();
                    }
                    // Blocking RPC provides natural backpressure; no fixed polling sleep.
                    drop(state);
                }
                Ok::<_, nya_domain::Error>(())
            })();
            if let Err(error) = result {
                reader = None;
                connection = None;
                let mut state = self.state.lock().expect("acquisition lock");
                if state.revision == revision {
                    state.status = error.to_string();
                    state.endpoint = None;
                    state.revision += 1;
                    state.sequence += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nya_domain::waveform::ScalarType;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::{mpsc, Arc},
    };

    fn channel(address: u64) -> MemoryChannel {
        MemoryChannel {
            address,
            byte_size: 4,
            scalar_type: ScalarType::Unsigned,
            little_endian: true,
        }
    }

    #[test]
    fn changed_targets_and_stop_discard_in_flight_results() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let (pending_tx, pending_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut stream = BufReader::new(stream);
            for address in [0x20000000u64, 0x20000004] {
                let mut request = Vec::new();
                stream.read_until(0x1a, &mut request).unwrap();
                assert!(String::from_utf8(request)
                    .unwrap()
                    .contains(&format!("read_memory 0x{address:X}")));
                pending_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                stream.get_mut().write_all(b"99\x1a").unwrap();
            }
        });
        let acquisition = Arc::new(Acquisition::default());
        acquisition.set_targets(vec![(0, channel(0x20000000))]);
        acquisition.start(endpoint);
        let sampler = acquisition.clone();
        let worker = std::thread::spawn(move || sampler.run());
        pending_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        acquisition.set_targets(vec![(0, channel(0x20000004))]);
        release_tx.send(()).unwrap();
        pending_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(acquisition.state.lock().unwrap().channels[0]
            .samples
            .is_empty());
        acquisition.stop();
        release_tx.send(()).unwrap();
        server.join().unwrap();
        acquisition.shutdown();
        worker.join().unwrap();
        let state = acquisition.state.lock().unwrap();
        assert!(state.channels[0].samples.is_empty());
        assert_eq!(state.sweeps, 0);
        assert!(state.endpoint.is_none());
    }

    #[test]
    fn socket_failure_stops_instead_of_retrying_or_fabricating_samples() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut stream = BufReader::new(stream);
            let mut request = Vec::new();
            stream.read_until(0x1a, &mut request).unwrap();
            stream
                .get_mut()
                .write_all(b"target not halted\x1a")
                .unwrap();
        });
        let acquisition = Arc::new(Acquisition::default());
        acquisition.set_targets(vec![(0, channel(0x20000000))]);
        acquisition.start(endpoint);
        let sampler = acquisition.clone();
        let worker = std::thread::spawn(move || sampler.run());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if acquisition.state.lock().unwrap().endpoint.is_none() {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        acquisition.shutdown();
        worker.join().unwrap();
        server.join().unwrap();
        let state = acquisition.state.lock().unwrap();
        assert!(state.status.contains("target not halted"));
        assert!(state.channels[0].samples.is_empty());
    }
}
