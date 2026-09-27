//! Thumbnail extraction.
//!
//! Optional, like [`crate::probe`]: FFmpeg is not a requirement of V1
//! (section 24), so a missing or unhappy `ffmpeg` means a video without a
//! thumbnail, never a failed publish.

use std::path::Path;
use std::process::Command;

use crate::probe::probe_duration_secs;

/// Widest edge of a generated thumbnail. Small enough that it travels as a
/// single block and cheap enough to fetch before deciding to watch anything.
pub const THUMBNAIL_WIDTH: u32 = 640;

/// Largest thumbnail we will produce or accept, comfortably inside one chunk.
pub const MAX_THUMBNAIL_BYTES: usize = 512 * 1024;

/// Extract a JPEG thumbnail, or `None` if that is not possible.
///
/// The frame is taken at 10% of the duration rather than at the start, where
/// many videos are black or a title card.
pub fn extract_thumbnail(path: impl AsRef<Path>) -> Option<Vec<u8>> {
    let path = path.as_ref();
    let seek = probe_duration_secs(path)
        .map(|duration| (duration as f64 * 0.1).clamp(0.0, 600.0))
        .unwrap_or(0.0);

    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-ss"])
        .arg(format!("{seek:.2}"))
        .arg("-i")
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={THUMBNAIL_WIDTH}:-2"),
            "-f",
            "image2pipe",
            "-vcodec",
            "mjpeg",
            "-q:v",
            "4",
            "-",
        ])
        .output()
        .ok()?;

    if !output.status.success() || output.stdout.is_empty() {
        tracing::debug!(
            path = %path.display(),
            "no thumbnail: ffmpeg is unavailable or could not decode the file"
        );
        return None;
    }
    if output.stdout.len() > MAX_THUMBNAIL_BYTES {
        tracing::debug!(
            bytes = output.stdout.len(),
            "no thumbnail: the generated image is larger than the limit"
        );
        return None;
    }
    if !looks_like_jpeg(&output.stdout) {
        tracing::debug!("no thumbnail: ffmpeg produced something that is not a JPEG");
        return None;
    }
    Some(output.stdout)
}

/// JPEG SOI and EOI markers. A cheap sanity check on a subprocess's output,
/// and the same check applied to a thumbnail block received from a peer.
pub fn looks_like_jpeg(data: &[u8]) -> bool {
    data.len() > 4 && data.starts_with(&[0xFF, 0xD8]) && data.ends_with(&[0xFF, 0xD9])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_video_yields_no_thumbnail_rather_than_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notavideo.mp4");
        std::fs::write(&path, b"not a video at all").unwrap();
        assert!(extract_thumbnail(&path).is_none());
    }

    #[test]
    fn a_missing_file_yields_no_thumbnail() {
        assert!(extract_thumbnail("/nonexistent/path/video.mp4").is_none());
    }

    #[test]
    fn jpeg_detection_rejects_anything_else() {
        assert!(looks_like_jpeg(&[0xFF, 0xD8, 0x00, 0x11, 0xFF, 0xD9]));
        assert!(!looks_like_jpeg(&[0xFF, 0xD8, 0x00]));
        assert!(!looks_like_jpeg(b"\x89PNG\r\n\x1a\n"));
        assert!(!looks_like_jpeg(&[]));
    }
}
