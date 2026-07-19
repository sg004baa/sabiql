/// Streaming record counter for RFC 4180 CSV byte chunks.
///
/// Counts logical CSV records instead of raw newline bytes so that newlines
/// embedded in quoted fields are not miscounted as row boundaries when a
/// CLI client's CSV output is streamed to a file.
pub(crate) struct CsvRecordCounter {
    in_quotes: bool,
    in_record: bool,
    records: usize,
}

impl CsvRecordCounter {
    pub(crate) const fn new() -> Self {
        Self {
            in_quotes: false,
            in_record: false,
            records: 0,
        }
    }

    pub(crate) fn feed(&mut self, chunk: &[u8]) {
        for &byte in chunk {
            match byte {
                b'"' => {
                    self.in_quotes = !self.in_quotes;
                    self.in_record = true;
                }
                b'\n' if !self.in_quotes => {
                    self.records += 1;
                    self.in_record = false;
                }
                // Part of a CRLF terminator; never starts a record by itself.
                b'\r' if !self.in_quotes => {}
                _ => self.in_record = true,
            }
        }
    }

    /// Total records seen, including a trailing record without a final newline.
    pub(crate) const fn finish(&self) -> usize {
        if self.in_record {
            self.records + 1
        } else {
            self.records
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CsvRecordCounter;

    fn count(chunks: &[&[u8]]) -> usize {
        let mut counter = CsvRecordCounter::new();
        for chunk in chunks {
            counter.feed(chunk);
        }
        counter.finish()
    }

    #[test]
    fn simple_rows_counted_per_line() {
        assert_eq!(count(&[b"a,b\n1,2\n3,4\n"]), 3);
    }

    #[test]
    fn quoted_newline_stays_in_one_record() {
        assert_eq!(count(&[b"a,b\n1,\"x\ny\"\n"]), 2);
    }

    #[test]
    fn escaped_quotes_around_newline_stay_in_one_record() {
        assert_eq!(count(&[b"a\n\"he said \"\"hi\n\"\"\"\n"]), 2);
    }

    #[test]
    fn missing_trailing_newline_counts_last_record() {
        assert_eq!(count(&[b"a,b\n1,2"]), 2);
    }

    #[test]
    fn crlf_terminators_counted_once() {
        assert_eq!(count(&[b"a,b\r\n1,2\r\n"]), 2);
    }

    #[test]
    fn empty_input_counts_zero() {
        assert_eq!(count(&[b""]), 0);
    }

    #[test]
    fn quoted_region_split_across_chunks_keeps_state() {
        assert_eq!(count(&[b"a\n\"x\n", b"y\"\n"]), 2);
    }
}
