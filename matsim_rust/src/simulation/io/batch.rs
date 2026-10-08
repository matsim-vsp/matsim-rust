use rayon::prelude::*;
use std::io::Read;
use std::ops::Range;
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread;
use std::thread::JoinHandle;

/// Records are read in batches of about this many bytes.
const BATCH_BYTES: usize = 16 * 1024 * 1024;
/// Number of read batches which may wait for processing. This bounds the memory used for input.
const QUEUED_BATCHES: usize = 2;
/// [`ReadAhead`] reads blocks of this many bytes.
const READ_AHEAD_BYTES: usize = 1024 * 1024;
/// Number of blocks which [`ReadAhead`] may read in advance.
const QUEUED_BLOCKS: usize = 8;

/// The raw bytes of consecutive records of an input file, e.g., the encoded persons of a
/// population.
#[derive(Default)]
pub(crate) struct RecordBatch {
    bytes: Vec<u8>,
    ranges: Vec<Range<usize>>,
}

impl RecordBatch {
    pub(crate) fn len(&self) -> usize {
        self.ranges.len()
    }

    /// Returns the records of this batch in input order.
    pub(crate) fn par_records(&self) -> impl IndexedParallelIterator<Item = &[u8]> {
        self.ranges
            .par_iter()
            .map(|range| &self.bytes[range.clone()])
    }
}

/// Reads records on a separate thread and processes them in batches on the calling thread. If `process` is multithreaded,
/// it can process the batches in parallel with reading the next batch.
///
/// `open` and `read_record` run on the reader thread, so that reading and decompressing overlap
/// with processing. `read_record` appends the bytes of the next record to the given buffer and
/// returns `false` at the end of the input. `process` receives the batches in input order.
pub(crate) fn read_in_batches<Source, Open, Read, Process>(
    open: Open,
    mut read_record: Read,
    mut process: Process,
) where
    Open: FnOnce() -> Source + Send,
    Read: FnMut(&mut Source, &mut Vec<u8>) -> bool + Send,
    Process: FnMut(RecordBatch),
{
    thread::scope(|scope| {
        let (sender, receiver) = sync_channel(QUEUED_BATCHES);
        // Create a thread which reads the records and sends them in batches to the main thread.
        scope.spawn(move || {
            let mut source = open();
            let mut batch = RecordBatch::default();
            loop {
                let start = batch.bytes.len();
                if !read_record(&mut source, &mut batch.bytes) {
                    break;
                }
                batch.ranges.push(start..batch.bytes.len());

                // Send waits until there is space in the channel (main purpose is to bound memory usage).
                if batch.bytes.len() >= BATCH_BYTES
                    && sender.send(std::mem::take(&mut batch)).is_err()
                {
                    // The receiving side stopped, e.g., because processing panicked.
                    return;
                }
            }
            if batch.len() > 0 {
                // An error means that the receiving side stopped. There is nothing left to do then.
                let _ = sender.send(batch);
            }
        });

        for batch in receiver {
            // Normally, process is also multithreaded, so it can process the batches in parallel with reading the next batch.
            process(batch);
        }
    });
}

/// Reads blocks of the input on a separate thread, so that, e.g., decompressing the input overlaps with processing it.
pub(crate) struct ReadAhead {
    receiver: Receiver<Vec<u8>>,
    block: Vec<u8>,
    pos: usize,
    handle: Option<JoinHandle<()>>,
}

impl ReadAhead {
    /// Spawns a thread which reads from the reader returned by `open`. The reader is created on that thread, so it doesn't need to be `Send`.
    pub(crate) fn spawn<R: Read>(open: impl FnOnce() -> R + Send + 'static) -> Self {
        let (sender, receiver) = sync_channel(QUEUED_BLOCKS);
        let handle = thread::spawn(move || {
            let mut reader = open();
            loop {
                let mut block = Vec::with_capacity(READ_AHEAD_BYTES);
                let read = (&mut reader)
                    .take(READ_AHEAD_BYTES as u64)
                    .read_to_end(&mut block)
                    .unwrap_or_else(|e| panic!("Failed to read input: {e}"));
                // An error means that the reading side was dropped. There is nothing left to do
                // then.
                if read == 0 || sender.send(block).is_err() {
                    return;
                }
            }
        });
        Self {
            receiver,
            block: Vec::new(),
            pos: 0,
            handle: Some(handle),
        }
    }
}

impl Read for ReadAhead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.pos == self.block.len() {
            match self.receiver.recv() {
                Ok(block) => {
                    self.block = block;
                    self.pos = 0;
                }
                Err(_) => {
                    // The thread has finished. If it panicked, the input is incomplete, so the
                    // panic must not be mistaken for the end of the input.
                    if let Some(handle) = self.handle.take()
                        && let Err(panic) = handle.join()
                    {
                        std::panic::resume_unwind(panic);
                    }
                    return Ok(0);
                }
            }
        }
        let n = buf.len().min(self.block.len() - self.pos);
        buf[..n].copy_from_slice(&self.block[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::{BATCH_BYTES, READ_AHEAD_BYTES, ReadAhead, read_in_batches};
    use rayon::prelude::*;
    use std::io::Read;

    #[test]
    fn read_ahead_returns_complete_input() {
        let data: Vec<u8> = (0..3 * READ_AHEAD_BYTES + 17)
            .map(|i| (i % 251) as u8)
            .collect();
        let input = data.clone();
        let mut reader = ReadAhead::spawn(move || std::io::Cursor::new(input));
        let mut result = Vec::new();
        reader.read_to_end(&mut result).unwrap();
        assert_eq!(data, result);
    }

    #[test]
    #[should_panic(expected = "broken input")]
    fn read_ahead_propagates_panics() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                panic!("broken input");
            }
        }
        let mut reader = ReadAhead::spawn(|| Broken);
        reader.read_to_end(&mut Vec::new()).unwrap();
    }

    #[test]
    fn records_keep_input_order_across_batches() {
        // Records of 1 MiB each, so that the input is split into several batches.
        let records: Vec<Vec<u8>> = (0..40u8).map(|i| vec![i; 1024 * 1024]).collect();
        assert!(records.len() * records[0].len() > 2 * BATCH_BYTES);

        let mut result = Vec::new();
        let mut num_batches = 0;
        read_in_batches(
            || records.iter(),
            |source, buffer| match source.next() {
                Some(record) => {
                    buffer.extend_from_slice(record);
                    true
                }
                None => false,
            },
            |batch| {
                num_batches += 1;
                let firsts: Vec<u8> = batch.par_records().map(|record| record[0]).collect();
                result.extend(firsts);
            },
        );

        assert!(num_batches > 1);
        assert_eq!((0..40).collect::<Vec<u8>>(), result);
    }

    #[test]
    fn empty_input_produces_no_batch() {
        let mut num_batches = 0;
        read_in_batches(|| (), |_, _| false, |_| num_batches += 1);
        assert_eq!(0, num_batches);
    }
}
