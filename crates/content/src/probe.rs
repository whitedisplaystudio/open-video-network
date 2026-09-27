//! Optional inspection of a source file.
//!
//! V1 does not transcode (section 24) and must not require FFmpeg to be
//! installed. `ffprobe` is used only to fill in a video's duration when it
//! happens to be available; when it is not, the duration is simply unknown.

use std::path::Path;
use std::process::Command;

/// Guess a media type from the file extension. Deliberately small: this is a
/// display hint, not a security boundary.
pub fn guess_media_type(path: impl AsRef<Path>) -> String {
    let ext = path
        .as_ref()
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "ogv" => "video/ogg",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "opus" => "audio/opus",
        "flac" => "audio/flac",
        "wav" => "audio/wav",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Duration in whole seconds, if `ffprobe` is installed and understands the
/// file. Returns `None` rather than failing: a missing duration is a cosmetic
/// problem, not a reason to refuse a publish.
pub fn probe_duration_secs(path: impl AsRef<Path>) -> Option<u64> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path.as_ref())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let seconds: f64 = text.trim().parse().ok()?;
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    Some(seconds.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types_come_from_the_extension() {
        assert_eq!(guess_media_type("a.mp4"), "video/mp4");
        assert_eq!(guess_media_type("a.MP4"), "video/mp4");
        assert_eq!(guess_media_type("a.webm"), "video/webm");
        assert_eq!(guess_media_type("a.unknown"), "application/octet-stream");
        assert_eq!(guess_media_type("noext"), "application/octet-stream");
    }

    #[test]
    fn probing_a_non_media_file_is_none_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notavideo.mp4");
        std::fs::write(&path, b"definitely not mp4").unwrap();
        assert_eq!(probe_duration_secs(&path), None);
    }
}
