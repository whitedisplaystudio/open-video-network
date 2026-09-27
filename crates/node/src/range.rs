//! HTTP byte ranges.
//!
//! A browser will not scrub through a video unless the server answers
//! `Range` requests, so this is what makes streaming playback work rather
//! than download-then-play.

/// A resolved byte range: both ends inclusive, both within the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    /// Bytes covered. Never zero: both ends are inclusive and `start <= end`
    /// by construction, so there is no empty range to ask about.
    pub fn byte_count(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn whole(total: u64) -> Option<Self> {
        (total > 0).then(|| Self {
            start: 0,
            end: total - 1,
        })
    }

    /// Chunk indices this range touches, given a chunk size.
    pub fn chunk_indices(&self, chunk_size: u64) -> std::ops::RangeInclusive<usize> {
        let first = (self.start / chunk_size) as usize;
        let last = (self.end / chunk_size) as usize;
        first..=last
    }
}

/// Why a `Range` header could not be honoured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeError {
    /// Syntactically wrong. Per RFC 9110 an unsatisfiable-looking but
    /// malformed header is ignored, and the whole resource is sent.
    Malformed,
    /// Syntactically fine but outside the file: answer 416.
    Unsatisfiable,
}

/// Parse a `Range` header against a known total size.
///
/// Only single ranges are supported. A multi-range request is treated as
/// malformed, which means the whole file is sent — correct behaviour, and
/// what every player falls back to anyway.
pub fn parse_range(header: &str, total: u64) -> Result<ByteRange, RangeError> {
    if total == 0 {
        return Err(RangeError::Unsatisfiable);
    }
    let spec = header.trim();
    let spec = spec.strip_prefix("bytes=").ok_or(RangeError::Malformed)?;
    if spec.contains(',') {
        return Err(RangeError::Malformed);
    }
    let (first, last) = spec.split_once('-').ok_or(RangeError::Malformed)?;
    let (first, last) = (first.trim(), last.trim());

    match (first.is_empty(), last.is_empty()) {
        // "bytes=-500": the final 500 bytes.
        (true, false) => {
            let suffix: u64 = last.parse().map_err(|_| RangeError::Malformed)?;
            if suffix == 0 {
                return Err(RangeError::Unsatisfiable);
            }
            let start = total.saturating_sub(suffix);
            Ok(ByteRange {
                start,
                end: total - 1,
            })
        }
        // "bytes=500-": from 500 to the end.
        (false, true) => {
            let start: u64 = first.parse().map_err(|_| RangeError::Malformed)?;
            if start >= total {
                return Err(RangeError::Unsatisfiable);
            }
            Ok(ByteRange {
                start,
                end: total - 1,
            })
        }
        // "bytes=0-1023"
        (false, false) => {
            let start: u64 = first.parse().map_err(|_| RangeError::Malformed)?;
            let end: u64 = last.parse().map_err(|_| RangeError::Malformed)?;
            if start > end || start >= total {
                return Err(RangeError::Unsatisfiable);
            }
            Ok(ByteRange {
                start,
                // A player may ask for more than there is; clamp rather than
                // refuse.
                end: end.min(total - 1),
            })
        }
        (true, true) => Err(RangeError::Malformed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_range_is_parsed() {
        assert_eq!(
            parse_range("bytes=0-1023", 4096),
            Ok(ByteRange {
                start: 0,
                end: 1023
            })
        );
        assert_eq!(
            parse_range("bytes=100-200", 4096),
            Ok(ByteRange {
                start: 100,
                end: 200
            })
        );
    }

    #[test]
    fn an_open_ended_range_runs_to_the_end() {
        // What a browser sends first for a video element.
        assert_eq!(
            parse_range("bytes=0-", 4096),
            Ok(ByteRange {
                start: 0,
                end: 4095
            })
        );
        assert_eq!(
            parse_range("bytes=4000-", 4096),
            Ok(ByteRange {
                start: 4000,
                end: 4095
            })
        );
    }

    #[test]
    fn a_suffix_range_takes_the_tail() {
        // What a player sends to find the moov atom at the end of an mp4.
        assert_eq!(
            parse_range("bytes=-500", 4096),
            Ok(ByteRange {
                start: 3596,
                end: 4095
            })
        );
        // A suffix longer than the file is the whole file, not an error.
        assert_eq!(
            parse_range("bytes=-99999", 4096),
            Ok(ByteRange {
                start: 0,
                end: 4095
            })
        );
    }

    #[test]
    fn an_end_past_the_file_is_clamped_not_refused() {
        assert_eq!(
            parse_range("bytes=4000-99999", 4096),
            Ok(ByteRange {
                start: 4000,
                end: 4095
            })
        );
    }

    #[test]
    fn a_start_past_the_file_is_unsatisfiable() {
        assert_eq!(
            parse_range("bytes=4096-", 4096),
            Err(RangeError::Unsatisfiable)
        );
        assert_eq!(
            parse_range("bytes=5000-6000", 4096),
            Err(RangeError::Unsatisfiable)
        );
        assert_eq!(
            parse_range("bytes=-0", 4096),
            Err(RangeError::Unsatisfiable)
        );
    }

    #[test]
    fn an_inverted_range_is_unsatisfiable() {
        assert_eq!(
            parse_range("bytes=200-100", 4096),
            Err(RangeError::Unsatisfiable)
        );
    }

    #[test]
    fn nonsense_is_malformed_so_the_whole_file_is_sent() {
        for header in [
            "",
            "bytes",
            "bytes=",
            "bytes=-",
            "items=0-10",
            "bytes=abc-def",
            "0-10",
            "bytes=0-10, 20-30",
        ] {
            assert_eq!(
                parse_range(header, 4096),
                Err(RangeError::Malformed),
                "{header:?}"
            );
        }
    }

    #[test]
    fn an_empty_file_can_satisfy_nothing() {
        assert_eq!(parse_range("bytes=0-", 0), Err(RangeError::Unsatisfiable));
        assert_eq!(ByteRange::whole(0), None);
    }

    #[test]
    fn ranges_map_onto_the_chunks_they_touch() {
        let chunk = 1000;
        let indices = |start, end| {
            let r = ByteRange { start, end };
            let i = r.chunk_indices(chunk);
            (*i.start(), *i.end())
        };
        assert_eq!(indices(0, 999), (0, 0));
        assert_eq!(indices(0, 1000), (0, 1));
        assert_eq!(indices(1500, 2500), (1, 2));
        assert_eq!(indices(2999, 2999), (2, 2));
    }

    #[test]
    fn length_counts_both_ends() {
        assert_eq!(ByteRange { start: 0, end: 0 }.byte_count(), 1);
        assert_eq!(ByteRange { start: 10, end: 19 }.byte_count(), 10);
        assert_eq!(ByteRange::whole(4096).unwrap().byte_count(), 4096);
    }
}
