//! Sampling owns its socket on a dedicated thread; UI polls bounded shared buffers.
use nya_debug_adapter::live_memory::OpenOcdMemoryReader;
use nya_domain::waveform::{LiveMemoryReader, MemoryChannel, ReadPlan, Sample, SampleBuffer};
use std::{
    net::SocketAddr,
    num::NonZeroUsize,
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

// Keep sub-millisecond precision when converting fractional custom frequencies.
pub fn sampling_interval_micros(index: i32, custom_hz: &str) -> Result<u64, &'static str> {
    const PRESETS_MS: [u64; 10] = [10, 20, 50, 100, 200, 500, 1000, 2000, 5000, 10000];
    if index == PRESETS_MS.len() as i32 {
        let hz = custom_hz
            .trim()
            .parse::<f64>()
            .map_err(|_| "请输入 0.1–1000 Hz 的有效频率")?;
        if !hz.is_finite() || !(0.1..=1000.0).contains(&hz) {
            return Err("自定义频率须在 0.1–1000 Hz 范围内");
        }
        Ok((1_000_000.0 / hz).round() as u64)
    } else {
        usize::try_from(index)
            .ok()
            .and_then(|i| PRESETS_MS.get(i))
            .map(|ms| ms * 1000)
            .ok_or("请选择有效的采样频率")
    }
}

pub const CAPACITY: usize = 32768;
pub const MAX_MEMORY_CHANNELS: usize = 64;

pub struct ChannelData {
    pub id: usize,
    pub memory: MemoryChannel,
    pub waveform: bool,
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
    pub monitor_interval: Duration,
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
            monitor_interval: Duration::from_millis(500),
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
        self.configure(
            targets
                .into_iter()
                .map(|(id, memory)| (id, memory, true))
                .collect(),
            Duration::from_millis(500),
        );
    }

    pub fn configure(
        &self,
        targets: Vec<(usize, MemoryChannel, bool)>,
        monitor_interval: Duration,
    ) {
        let mut state = self.state.lock().expect("acquisition lock");
        let monitor_interval = monitor_interval.max(Duration::from_millis(1));
        if state.monitor_interval == monitor_interval
            && state
                .channels
                .iter()
                .map(|c| (c.id, &c.memory, c.waveform))
                .eq(targets.iter().map(|(id, m, wave)| (*id, m, *wave)))
        {
            return;
        }
        state.monitor_interval = monitor_interval;
        let mut previous = std::mem::take(&mut state.channels);
        state.channels = targets
            .into_iter()
            .take(MAX_MEMORY_CHANNELS)
            .map(|(id, memory, waveform)| {
                if let Some(index) = previous
                    .iter()
                    .position(|c| c.id == id && c.memory == memory)
                {
                    let mut channel = previous.swap_remove(index);
                    if waveform && !channel.waveform {
                        let mut samples = SampleBuffer::new(NonZeroUsize::new(CAPACITY).unwrap());
                        if let Some(sample) = channel.samples.latest() {
                            let _ = samples.push(*sample);
                        }
                        channel.samples = samples;
                    }
                    channel.waveform = waveform;
                    channel
                } else {
                    ChannelData {
                        id,
                        memory,
                        waveform,
                        samples: SampleBuffer::new(
                            NonZeroUsize::new(if waveform { CAPACITY } else { 1 }).unwrap(),
                        ),
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
            let (revision, endpoint, epoch, channels, wave_indices, monitor_interval) = {
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
                    state
                        .channels
                        .iter()
                        .enumerate()
                        .filter_map(|(i, c)| c.waveform.then_some(i))
                        .collect::<Vec<_>>(),
                    state.monitor_interval,
                )
            };
            let result = (|| {
                let full_plan = ReadPlan::new(&channels)?;
                let wave_plan = ReadPlan::new(
                    &wave_indices
                        .iter()
                        .map(|i| channels[*i].clone())
                        .collect::<Vec<_>>(),
                )?;
                let all_indices: Vec<_> = (0..channels.len()).collect();
                let mut next_monitor = Instant::now();
                if reader.is_none() || connection != Some(endpoint) {
                    reader = Some(OpenOcdMemoryReader::connect(endpoint)?);
                    connection = Some(endpoint);
                }
                loop {
                    {
                        let mut state = self.state.lock().expect("acquisition lock");
                        if state.quit {
                            return Ok(());
                        }
                        if state.revision != revision {
                            break;
                        }
                        if wave_indices.is_empty() && Instant::now() < next_monitor {
                            state = self
                                .changed
                                .wait_timeout(
                                    state,
                                    next_monitor.saturating_duration_since(Instant::now()),
                                )
                                .expect("acquisition lock")
                                .0;
                            if state.quit {
                                return Ok(());
                            }
                            if state.revision != revision {
                                break;
                            }
                            continue;
                        }
                    }
                    let include_monitors =
                        wave_indices.len() != channels.len() && Instant::now() >= next_monitor;
                    let (indices, plan) = if include_monitors {
                        (&all_indices, &full_plan)
                    } else {
                        (&wave_indices, &wave_plan)
                    };
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
                    for (plan_index, index) in indices.iter().enumerate() {
                        let channel = &mut state.channels[*index];
                        let (block, offset) = plan.locations[plan_index];
                        let value = channel
                            .memory
                            .decode(&blocks[block][offset..offset + channel.memory.byte_size])?;
                        if channel.samples.push(Sample { elapsed, value }).is_err() {
                            channel.invalid += 1;
                        }
                    }
                    if include_monitors {
                        next_monitor = Instant::now() + monitor_interval;
                    }
                    state.sweeps += 1;
                    state.elapsed = elapsed;
                    state.sequence += 1;
                    if state.sweeps == 1 {
                        state.status = "运行中内存采集 · 无暂停".into();
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

    #[test]
    fn monitor_only_sampling_waits_and_waveform_promotion_preserves_history() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut stream = BufReader::new(stream);
            let mut value = 1;
            loop {
                let mut request = Vec::new();
                match stream.read_until(0x1a, &mut request) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if write!(stream.get_mut(), "{value}\x1a").is_err() {
                            break;
                        }
                        value += 1;
                    }
                }
            }
        });
        let acquisition = Arc::new(Acquisition::default());
        acquisition.configure(
            vec![(0, channel(0x20000000), false)],
            Duration::from_secs(10),
        );
        acquisition.start(endpoint);
        let sampler = acquisition.clone();
        let worker = std::thread::spawn(move || sampler.run());
        let deadline = Instant::now() + Duration::from_secs(5);
        while acquisition.state.lock().unwrap().sweeps == 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(20));
        let epoch = {
            let state = acquisition.state.lock().unwrap();
            assert_eq!(
                state.sweeps, 1,
                "monitor-only reads must honor the configured interval"
            );
            assert_eq!(state.channels[0].samples.latest().unwrap().value, 1.0);
            state.epoch
        };
        acquisition.configure(
            vec![(0, channel(0x20000000), true)],
            Duration::from_secs(10),
        );
        while acquisition.state.lock().unwrap().sweeps < 10 {
            assert!(
                Instant::now() < deadline,
                "adding a waveform must wake the monitor wait"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        acquisition.shutdown();
        worker.join().unwrap();
        server.join().unwrap();
        let state = acquisition.state.lock().unwrap();
        assert_eq!(state.epoch, epoch);
        assert_eq!(
            state.channels[0].samples.samples().next().unwrap().value,
            1.0
        );
        assert!(state.channels[0].samples.len() >= 10);
    }
}
