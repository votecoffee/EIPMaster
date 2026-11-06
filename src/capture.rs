use crate::model::{
    CAPTURE_METADATA, CaptureField, CaptureMode, MAX_ASSEMBLY_BYTES, Settings, Value,
};
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

mod field_summary;
use field_summary::FieldStats;
pub use field_summary::FieldSummary;

struct Sample {
    elapsed: u64,
    index: u64,
    generation: u64,
    sequence: u32,
    cip_sequence: u16,
    input: [u8; MAX_ASSEMBLY_BYTES],
}

#[derive(Default)]
struct ChangeState {
    previous: Option<(u64, [u8; MAX_ASSEMBLY_BYTES])>,
}
pub struct Sink {
    start: Instant,
    utc: String,
    queue: ArrayQueue<Sample>,
    fields: Vec<CaptureField>,
    input_size: usize,
    changes: Option<Mutex<ChangeState>>,
    header: String,
    stop: AtomicBool,
    failed: AtomicBool,
    pub seen: AtomicU64,
    pub unchanged: AtomicU64,
    next_index: AtomicU64,
    pub overrun: AtomicU64,
    pub written: AtomicU64,
    parts: AtomicU64,
    pub error: Mutex<Option<String>>,
    field_summaries: Mutex<Vec<FieldSummary>>,
    pub prefix: PathBuf,
}
pub type Slot = Arc<RwLock<Option<Arc<Sink>>>>;

impl Sink {
    pub fn start_utc(&self) -> &str {
        &self.utc
    }

    pub fn mode(&self) -> CaptureMode {
        if self.changes.is_some() {
            CaptureMode::OnChange
        } else {
            CaptureMode::EveryPacket
        }
    }

    pub fn field_summaries(&self) -> Vec<FieldSummary> {
        self.field_summaries.lock().unwrap().clone()
    }

    pub fn part_count(&self) -> u64 {
        self.parts.load(Relaxed)
    }

    pub fn record(
        &self,
        stamp: Instant,
        generation: u64,
        sequence: u32,
        cip_sequence: u16,
        input: &[u8],
    ) {
        if stamp < self.start || input.len() != self.input_size {
            return;
        }
        self.seen.fetch_add(1, Relaxed);
        let mut changes = self.changes.as_ref().map(|state| state.lock().unwrap());
        if let Some((prior_generation, prior)) =
            changes.as_ref().and_then(|state| state.previous.as_ref())
            && *prior_generation == generation
            && self.fields.iter().all(|field| {
                let a = field.selection.decode(&prior[..self.input_size]);
                let b = field.selection.decode(input);
                match (a, b) {
                    (Some(Value::Float32(a)), Some(Value::Float32(b))) => {
                        a.to_bits() == b.to_bits()
                    }
                    (Some(Value::Float64(a)), Some(Value::Float64(b))) => {
                        a.to_bits() == b.to_bits()
                    }
                    _ => a == b,
                }
            })
        {
            self.unchanged.fetch_add(1, Relaxed);
            return;
        }
        if self.failed.load(Relaxed) {
            return;
        }
        let index = self.next_index.fetch_add(1, Relaxed);
        let mut bytes = [0; MAX_ASSEMBLY_BYTES];
        bytes[..self.input_size].copy_from_slice(input);
        if self
            .queue
            .push(Sample {
                elapsed: stamp.duration_since(self.start).as_micros() as u64,
                index,
                generation,
                sequence,
                cip_sequence,
                input: bytes,
            })
            .is_err()
        {
            self.overrun.fetch_add(1, Relaxed);
        } else if let Some(state) = &mut changes {
            state.previous = Some((generation, bytes));
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
        let input_size = usize::from(settings.input_size);
        if !(1..=MAX_ASSEMBLY_BYTES).contains(&input_size) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid capture input assembly size",
            ));
        }
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
            input_size,
            changes: (settings.capture_mode == CaptureMode::OnChange).then(Mutex::default),
            header,
            stop: false.into(),
            failed: false.into(),
            seen: 0.into(),
            unchanged: 0.into(),
            next_index: 0.into(),
            overrun: 0.into(),
            written: 0.into(),
            parts: 1.into(),
            error: Mutex::new(None),
            field_summaries: Mutex::new(Vec::new()),
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
    pub fn finished(&self) -> bool {
        self.worker.as_ref().is_some_and(JoinHandle::is_finished)
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
    let mut summaries: Vec<_> = sink.fields.iter().map(|_| FieldStats::default()).collect();
    let mut previous_generation = None;
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
            let same_generation = previous_generation == Some(sample.generation);
            for (field, stats) in sink.fields.iter().zip(&mut summaries) {
                let value = field
                    .selection
                    .decode(&sample.input[..sink.input_size])
                    .ok_or_else(|| io::Error::other("Invalid CSV field selection"))?;
                match value {
                    Value::Text(bytes) => {
                        line.extend_from_slice(b",\"");
                        let text = String::from_utf8_lossy(bytes);
                        stats.record(&text, value, same_generation);
                        for (index, segment) in text.split('"').enumerate() {
                            if index != 0 {
                                line.extend_from_slice(b"\"\"");
                            }
                            line.extend_from_slice(segment.as_bytes());
                        }
                        line.push(b'"');
                    }
                    _ => {
                        let start = line.len() + 1;
                        write!(line, ",{value}")?;
                        let text = std::str::from_utf8(&line[start..]).expect("formatted value");
                        stats.record(text, value, same_generation);
                    }
                }
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
                sink.parts.store(number, Relaxed);
            }
            part.file.write_all(&line)?;
            part.first_us.get_or_insert(sample.elapsed);
            part.bytes += line.len() as u64;
            part.samples += 1;
            previous_generation = Some(sample.generation);
            batch += 1;
        }
        if batch != 0 {
            part.file.flush()?;
            sink.written.store(committed + part.samples, Relaxed);
        } else if sink.stop.load(Acquire) && sink.queue.is_empty() {
            part.file.flush()?;
            *sink.field_summaries.lock().unwrap() = summaries
                .into_iter()
                .zip(&sink.fields)
                .filter_map(|(stats, field)| stats.finish(field))
                .collect();
            return Ok(());
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
                    ..crate::model::Selection::default()
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
        let summaries = capture.sink.field_summaries();
        assert_eq!(summaries.len(), 3);
        assert_eq!(summaries[0].name, "first");
        assert_eq!(summaries[0].minimum.as_deref(), Some("-5"));
        assert_eq!(summaries[0].maximum.as_deref(), Some("-5"));
        assert_eq!(summaries[0].changes_within_generation, 0);
        let mut parts: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        parts.sort();
        assert_eq!(parts.len(), 8);
        assert_eq!(capture.sink.part_count(), 8);
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
    fn change_capture_filters_unselected_bytes_and_quotes_text_across_reconnect() {
        use crate::model::{Kind, Selection};
        let dir = std::env::temp_dir().join(format!(
            "assembly-scanner-changes-{}",
            Utc::now().timestamp_nanos_opt().unwrap()
        ));
        let settings = Settings {
            input_size: 8,
            capture_fields: vec![
                CaptureField {
                    name: "text".into(),
                    selection: Selection {
                        kind: Kind::Utf8,
                        string_length: Some(7),
                        ..Selection::default()
                    },
                },
                CaptureField {
                    name: "other".into(),
                    selection: Selection {
                        kind: Kind::U8,
                        offset: 7,
                        ..Selection::default()
                    },
                },
            ],
            log_fields: Some(vec!["text".into()]),
            capture_mode: CaptureMode::OnChange,
            ..Settings::default()
        };
        let slot = Slot::default();
        let mut capture = Capture::start(
            &slot,
            Options {
                directory: dir.clone(),
                capacity: 8,
                rotate_bytes: 0,
                rotate_seconds: 1,
            },
            &settings,
        )
        .unwrap();
        for (n, (generation, bytes)) in [
            (1, *b"a,\"b\n\0x\x01"),
            (1, *b"a,\"b\n\0y\x02"),
            (1, *b"new\0xyz\x03"),
            (2, *b"new\0xyz\x04"),
        ]
        .iter()
        .enumerate()
        {
            capture.sink.record(
                capture.sink.start + Duration::from_secs(n as u64),
                *generation,
                n as u32,
                n as u16,
                bytes,
            );
        }
        capture.stop(&slot);
        assert_eq!(capture.sink.seen.load(Relaxed), 4);
        assert_eq!(capture.sink.unchanged.load(Relaxed), 1);
        assert_eq!(capture.sink.written.load(Relaxed), 3);
        assert_eq!(capture.sink.overrun.load(Relaxed), 0);
        let summaries = capture.sink.field_summaries();
        let [summary] = summaries.as_slice() else {
            panic!("one logged field expected");
        };
        assert_eq!(summary.name, "text");
        assert_eq!(summary.first_value, "a,\"b\n");
        assert_eq!(summary.last_value, "new");
        assert!(summary.minimum.is_none() && summary.maximum.is_none());
        assert_eq!(summary.changes_within_generation, 1);
        let mut files: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        files.sort();
        assert_eq!(files.len(), 3);
        for (file, expected) in files.iter().zip([
            ",0,0,1,0,0,\"a,\"\"b\n\"\r\n",
            ",2000000,1,1,2,2,\"new\"\r\n",
            ",3000000,2,2,3,3,\"new\"\r\n",
        ]) {
            let csv = fs::read_to_string(file).unwrap();
            assert!(csv.starts_with("capture_start_utc,elapsed_us,sample_index,connection_generation,packet_sequence,cip_sequence,text\r\n"));
            assert!(csv.ends_with(expected), "{csv:?}");
            fs::remove_file(file).unwrap();
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
        assert_eq!(capture.sink.part_count(), 1);
        assert_eq!(capture.sink.overrun.load(Relaxed), 0);
        assert!(capture.sink.error.lock().unwrap().is_some());
        assert!(capture.sink.field_summaries().is_empty());
        assert_eq!(fs::read_to_string(&second).unwrap(), "existing file");
        fs::remove_file(first).unwrap();
        fs::remove_file(second).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
