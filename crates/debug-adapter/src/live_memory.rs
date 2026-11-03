//! Persistent OpenOCD Tcl RPC connection. No GDB, halt, reset, writes or target code.
use nya_domain::{
    waveform::{LiveMemoryReader, ReadBlock},
    Error, Result,
};
use std::{
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};

pub struct OpenOcdMemoryReader {
    stream: BufReader<TcpStream>,
}

impl OpenOcdMemoryReader {
    pub fn connect(endpoint: SocketAddr) -> Result<Self> {
        let stream = TcpStream::connect_timeout(&endpoint, Duration::from_millis(500))
            .map_err(|e| Error(format!("连接 OpenOCD Tcl {endpoint} 失败：{e}")))?;
        stream.set_nodelay(true).map_err(io_error)?;
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .map_err(io_error)?;
        stream
            .set_write_timeout(Some(Duration::from_millis(500)))
            .map_err(io_error)?;
        Ok(Self {
            stream: BufReader::new(stream),
        })
    }
}

fn io_error(error: std::io::Error) -> Error {
    Error(format!("OpenOCD 采集通信失败：{error}"))
}

impl LiveMemoryReader for OpenOcdMemoryReader {
    fn read_blocks(&mut self, blocks: &[ReadBlock]) -> Result<Vec<Vec<u8>>> {
        if blocks.is_empty() {
            return Ok(Vec::new());
        }
        if blocks.len() > 16
            || blocks.iter().any(|b| {
                b.byte_size == 0
                    || b.byte_size > 1024
                    || b.address.checked_add(b.byte_size as u64).is_none()
            })
        {
            return Err(Error("采集读取块超出限制".into()));
        }
        // One request/response per sweep, even for noncontiguous addresses.
        // Only validated integers are interpolated into Tcl.
        let mut command = String::from("join [list");
        for block in blocks {
            let width = block.width_bytes();
            command.push_str(&format!(
                " [join [read_memory 0x{:X} {} {}] ,]",
                block.address,
                width * 8,
                block.byte_size / width
            ));
        }
        command.push_str("] |\x1a");
        self.stream
            .get_mut()
            .write_all(command.as_bytes())
            .map_err(io_error)?;
        let response = self.read_frame()?;
        parse_response(&response, blocks)
    }
}

impl OpenOcdMemoryReader {
    fn read_frame(&mut self) -> Result<Vec<u8>> {
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut response = Vec::new();
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| Error("OpenOCD 响应超时".into()))?;
            self.stream
                .get_ref()
                .set_read_timeout(Some(remaining))
                .map_err(io_error)?;
            let bytes = self.stream.fill_buf().map_err(io_error)?;
            if bytes.is_empty() {
                return Err(Error("OpenOCD 连接已关闭".into()));
            }
            let end = bytes.iter().position(|b| *b == 0x1a);
            let length = end.unwrap_or(bytes.len());
            if response.len() + length > 65536 {
                return Err(Error("OpenOCD 响应超长".into()));
            }
            response.extend_from_slice(&bytes[..length]);
            self.stream.consume(length + usize::from(end.is_some()));
            if end.is_some() {
                return Ok(response);
            }
        }
    }
}

fn parse_response(response: &[u8], blocks: &[ReadBlock]) -> Result<Vec<Vec<u8>>> {
    let text =
        std::str::from_utf8(response).map_err(|_| Error("OpenOCD 返回非 UTF-8 数据".into()))?;
    let lists: Vec<_> = text.trim().split('|').collect();
    let invalid = || {
        Error(format!(
            "OpenOCD 内存读取失败：{}",
            text.chars().take(200).collect::<String>()
        ))
    };
    if lists.len() != blocks.len() {
        return Err(invalid());
    }
    lists
        .iter()
        .zip(blocks)
        .map(|(list, block)| {
            let width = block.width_bytes();
            let mut bytes = Vec::with_capacity(block.byte_size);
            for word in list.split(',') {
                let word = word.trim();
                let value = if let Some(hex) =
                    word.strip_prefix("0x").or_else(|| word.strip_prefix("0X"))
                {
                    u64::from_str_radix(hex, 16)
                } else {
                    word.parse::<u64>()
                }
                .map_err(|_| invalid())?;
                if value >= (1u64 << (width * 8)) {
                    return Err(invalid());
                }
                let raw = if block.little_endian {
                    value.to_le_bytes()
                } else {
                    value.to_be_bytes()
                };
                bytes.extend_from_slice(if block.little_endian {
                    &raw[..width]
                } else {
                    &raw[8 - width..]
                });
            }
            if bytes.len() != block.byte_size {
                return Err(invalid());
            }
            Ok(bytes)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn persistent_rpc_batches_reads_and_handles_fragmented_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut stream = BufReader::new(stream);
            for _ in 0..2 {
                let mut request = Vec::new();
                stream.read_until(0x1a, &mut request).unwrap();
                assert_eq!(String::from_utf8(request).unwrap(), "join [list [join [read_memory 0x20000000 32 2] ,] [join [read_memory 0x20000010 16 1] ,]] |\x1a");
                stream.get_mut().write_all(b"0x12345678,").unwrap();
                stream.get_mut().write_all(b"0xffffffff|513\x1a").unwrap();
            }
        });
        let blocks = [
            ReadBlock {
                address: 0x20000000,
                byte_size: 8,
                little_endian: true,
            },
            ReadBlock {
                address: 0x20000010,
                byte_size: 2,
                little_endian: false,
            },
        ];
        let mut reader = OpenOcdMemoryReader::connect(endpoint).unwrap();
        for _ in 0..2 {
            assert_eq!(
                reader.read_blocks(&blocks).unwrap(),
                [vec![0x78, 0x56, 0x34, 0x12, 255, 255, 255, 255], vec![2, 1]]
            );
        }
        server.join().unwrap();
    }

    #[test]
    fn rejects_target_errors_short_reads_and_out_of_range_words() {
        let blocks = [ReadBlock {
            address: 0x20000000,
            byte_size: 4,
            little_endian: true,
        }];
        for response in ["target not halted", "", "0x100000000", "1,2", "1|2"] {
            assert!(parse_response(response.as_bytes(), &blocks).is_err());
        }
    }
}
