use crate::model::{CAPTURE_METADATA, CaptureField, MAX_CAPTURE_FIELDS, Settings, Value};
use chrono::Utc;
use crossbeam_queue::ArrayQueue;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{
            AtomicBool, AtomicU64,
            Ordering::{Acquire, Relaxed, Release},
        },
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
struct Sample {
    elapsed: u64,
    index: u64,
    generation: u64,
    sequence: u32,
    cip_sequence: u16,
    values: [Value; MAX_CAPTURE_FIELDS],
}
pub struct Sink {
    start: Instant,
    utc: String,
    queue: ArrayQueue<Sample>,
    fields: Vec<CaptureField>,
    header: String,
    stop: AtomicBool,
    failed: AtomicBool,
    pub seen: AtomicU64,
    pub overrun: AtomicU64,
    pub written: AtomicU64,
    pub error: Mutex<Option<String>>,
    pub prefix: PathBuf,
}
pub type Slot = Arc<RwLock<Option<Arc<Sink>>>>;

impl Sink {
    pub fn record(
        &self,
        stamp: Instant,
        generation: u64,
        sequence: u32,
        cip_sequence: u16,
        input: &[u8],
    ) {
        if stamp < self.start {
            return;
        }
        let index = self.seen.fetch_add(1, Relaxed);
        if self.failed.load(Relaxed) {
            return;
        }
        let mut values = [Value::Unsigned(0); MAX_CAPTURE_FIELDS];
        for (dest, field) in values.iter_mut().zip(&self.fields) {
            let Some(value) = field.selection.decode(input) else {
                return;
            };
            *dest = value;
        }
        if self
            .queue
            .push(Sample {
                elapsed: stamp.duration_since(self.start).as_micros() as u64,
                index,
                generation,
                sequence,
                cip_sequence,
                values,
            })
            .is_err()
        {
            self.overrun.fetch_add(1, Relaxed);
        }
    }
}

pub struct Capture {
    pub sink: Arc<Sink>,
    worker: Option<JoinHandle<()>>,
}
pub struct Options {
    pub directory: PathBuf,
    pub capacity: usize,
    pub rotate_bytes: u64,
    pub rotate_seconds: u64,
}
impl Capture {
    pub fn start(slot: &Slot, opts: Options, settings: &Settings) -> io::Result<Self> {
        Self::start_inner(slot, opts, settings, Duration::ZERO)
    }
    fn start_inner(
        slot: &Slot,
        opts: Options,
        settings: &Settings,
        delay: Duration,
    ) -> io::Result<Self> {
        let mut current = slot.write().unwrap();
        if current.is_some() || !(1..=1_000_000).contains(&opts.capacity) {
            return Err(io::Error::other(
                "Capture already running or capacity outside 1–1,000,000",
            ));
        }
        let fields = settings
            .csv_fields()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let mut header = CAPTURE_METADATA.join(",");
        for field in &fields {
            header.push(',');
            header.push_str(&field.name);
        }
        header.push_str("\r\n");
        fs::create_dir_all(&opts.directory)?;
        let start = Instant::now();
        let now = Utc::now();
        let prefix = opts
            .directory
            .join(now.format("capture-%Y%m%dT%H%M%S%.9fZ").to_string());
        let mut first = Part::new(&prefix, 1, &header)?;
        first.file.flush()?;
        let sink = Arc::new(Sink {
            start,
            utc: now.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            queue: ArrayQueue::new(opts.capacity),
            fields,
            header,
            stop: false.into(),
            failed: false.into(),
            seen: 0.into(),
            overrun: 0.into(),
            written: 0.into(),
            error: Mutex::new(None),
            prefix,
        });
        let writer = sink.clone();
        let worker = thread::Builder::new()
            .name("csv-writer".into())
            .spawn(move || {
                if let Err(e) = drain(&writer, opts, first, delay) {
                    writer.failed.store(true, Relaxed);
                    *writer.error.lock().unwrap() = Some(e.to_string());
                }
            })?;
        *current = Some(sink.clone());
        Ok(Self {
            sink,
            worker: Some(worker),
        })
    }
    pub fn stop(&mut self, slot: &Slot) {
        if self.worker.is_some() {
            *slot.write().unwrap() = None;
            self.sink.stop.store(true, Release);
            if self.worker.take().unwrap().join().is_err() {
                *self.sink.error.lock().unwrap() = Some("CSV writer panicked".into());
            }
        }
    }
}

struct Part {
    file: BufWriter<File>,
    bytes: u64,
    samples: u64,
    first_us: Option<u64>,
}
impl Part {
    fn new(prefix: &std::path::Path, index: u64, header: &str) -> io::Result<Self> {
        let name = format!(
            "{}-{index:06}.csv",
            prefix.file_name().unwrap().to_string_lossy()
        );
        let mut file = BufWriter::with_capacity(
            128 * 1024,
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(prefix.with_file_name(name))?,
        );
        file.write_all(header.as_bytes())?;
        Ok(Self {
            file,
            bytes: header.len() as u64,
            samples: 0,
            first_us: None,
        })
    }
}
fn drain(sink: &Sink, opts: Options, mut part: Part, delay: Duration) -> io::Result<()> {
    let mut line = Vec::with_capacity(192);
    let mut number = 1;
    let mut committed = 0;
    loop {
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        let mut batch = 0;
        while batch < 512 {
            let Some(sample) = sink.queue.pop() else {
                break;
            };
            line.clear();
            write!(
                line,
                "{},{},{},{},{},{}",
                sink.utc,
                sample.elapsed,
                sample.index,
                sample.generation,
                sample.sequence,
                sample.cip_sequence,
            )?;
            for value in &sample.values[..sink.fields.len()] {
                write!(line, ",{value}")?;
            }
            line.extend_from_slice(b"\r\n");
            if part.samples > 0
                && ((opts.rotate_bytes != 0 && part.bytes + line.len() as u64 > opts.rotate_bytes)
                    || (opts.rotate_seconds != 0
                        && sample.elapsed - part.first_us.unwrap()
                            >= opts.rotate_seconds.saturating_mul(1_000_000)))
            {
                part.file.flush()?;
                committed += part.samples;
                sink.written.store(committed, Relaxed);
                number += 1;
                part = Part::new(&sink.prefix, number, &sink.header)?;
            }
            part.file.write_all(&line)?;
            part.first_us.get_or_insert(sample.elapsed);
            part.bytes += line.len() as u64;
            part.samples += 1;
            batch += 1;
        }
        if batch != 0 {
            part.file.flush()?;
            sink.written.store(committed + part.samples, Relaxed);
        } else if sink.stop.load(Acquire) && sink.queue.is_empty() {
            return part.file.flush();
        } else {
            thread::sleep(Duration::from_millis(8));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slow_writer_rotates_drains_and_preserves_indexes() {
        let dir = std::env::temp_dir().join(format!(
            "assembly-scanner-{}",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let slot = Slot::default();
        let mut settings = Settings {
            input_size: 6,
            ..Settings::default()
        };
        settings.capture_fields = [("first", 0), ("second", 2), ("third", 4)]
            .into_iter()
            .map(|(name, offset)| CaptureField {
                name: name.into(),
                selection: crate::model::Selection {
                    offset,
                    kind: crate::model::Kind::I16,
                    big_endian: true,
                    bit: None,
                },
            })
            .collect();
        let mut capture = Capture::start_inner(
            &slot,
            Options {
                directory: dir.clone(),
                capacity: 8,
                rotate_bytes: 150,
                rotate_seconds: 0,
            },
            &settings,
            Duration::from_millis(150),
        )
        .unwrap();
        for n in 0..100 {
            capture.sink.record(
                capture.sink.start + Duration::from_secs(n.into()),
                if n < 4 { 1 } else { 2 },
                n,
                n as u16,
                &[0xff, 0xfb, 0, 7, 0x01, 0x05],
            );
        }
        capture.stop(&slot);
        assert_eq!(capture.sink.overrun.load(Relaxed), 92);
        assert_eq!(capture.sink.written.load(Relaxed), 8);
        let mut parts: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        parts.sort();
        assert_eq!(parts.len(), 8);
        for (index, path) in parts.iter().enumerate() {
            let content = fs::read_to_string(path).unwrap();
            assert!(content.starts_with("capture_start_utc,elapsed_us,sample_index,connection_generation,packet_sequence,cip_sequence,first,second,third\r\n"));
            assert!(content.ends_with(&format!(
                ",{index},{},{index},{index},-5,7,261\r\n",
                if index < 4 { 1 } else { 2 }
            )));
        }
        for path in parts {
            fs::remove_file(path).unwrap();
        }
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn rotation_open_failure_reports_unwritten_rows() {
        let dir = std::env::temp_dir().join(format!(
            "assembly-scanner-rotation-{}",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let slot = Slot::default();
        let mut capture = Capture::start(
            &slot,
            Options {
                directory: dir.clone(),
                capacity: 8,
                rotate_bytes: 0,
                rotate_seconds: 1,
            },
            &Settings::default(),
        )
        .unwrap();
        let prefix = capture.sink.prefix.file_name().unwrap().to_str().unwrap();
        let first = dir.join(format!("{prefix}-000001.csv"));
        let second = dir.join(format!("{prefix}-000002.csv"));
        fs::write(&second, "existing file").unwrap();
        for n in 0..2 {
            capture.sink.record(
                capture.sink.start + Duration::from_secs(n.into()),
                1,
                n,
                n as u16,
                &5u32.to_le_bytes(),
            );
        }
        capture.stop(&slot);
        assert_eq!(capture.sink.seen.load(Relaxed), 2);
        assert_eq!(capture.sink.written.load(Relaxed), 1);
        assert_eq!(capture.sink.overrun.load(Relaxed), 0);
        assert!(capture.sink.error.lock().unwrap().is_some());
        assert_eq!(fs::read_to_string(&second).unwrap(), "existing file");
        fs::remove_file(first).unwrap();
        fs::remove_file(second).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
