//! Turning API responses into something a person wants to read.

use serde_json::Value;

use ovn_node::doctor::{Report, Severity};

/// Right-pad to `width` columns, counting characters rather than bytes so a
/// CJK title does not break the layout more than it has to.
fn pad(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len >= width {
        text.chars().take(width).collect()
    } else {
        format!("{text}{}", " ".repeat(width - len))
    }
}

fn short_cid(cid: &str) -> String {
    if cid.chars().count() <= 20 {
        return cid.to_string();
    }
    let chars: Vec<char> = cid.chars().collect();
    format!(
        "{}…{}",
        chars[..10].iter().collect::<String>(),
        chars[chars.len() - 6..].iter().collect::<String>()
    )
}

pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn duration(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn u64_field(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

pub fn status(value: &Value) {
    println!("Node       {}", str_field(value, "nodeName"));
    println!("Peer ID    {}", str_field(value, "peerId"));
    println!("Identity   {}", str_field(value, "publicKey"));
    println!("Uptime     {}", duration(u64_field(value, "uptimeSecs")));
    println!("Data dir   {}", str_field(value, "dataDir"));
    println!();
    println!(
        "Peers      {} connected, {} known, {} in the routing table",
        u64_field(value, "connectedPeers"),
        u64_field(value, "knownPeers"),
        u64_field(value, "routingTablePeers")
    );
    println!(
        "Videos     {} discovered, {} published here, {} being served",
        u64_field(value, "knownVideos"),
        u64_field(value, "localVideos"),
        u64_field(value, "providing")
    );
    let cache = value.get("cache").cloned().unwrap_or(Value::Null);
    println!(
        "Cache      {} in {} blocks (limit {}, {} pinned)",
        bytes(u64_field(&cache, "totalBytes")),
        u64_field(&cache, "blockCount"),
        bytes(u64_field(value, "cacheLimitBytes")),
        bytes(u64_field(&cache, "pinnedBytes"))
    );
    println!();
    let addrs = value
        .get("listenAddrs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if addrs.is_empty() {
        println!("Listening on nothing yet.");
    } else {
        println!("Listening on:");
        for addr in addrs {
            println!("  {}", addr.as_str().unwrap_or_default());
        }
    }
}

pub fn peers(value: &Value) {
    let rows = value.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("No peers yet.");
        println!();
        println!("Add one with a link someone gave you:");
        println!("    ourvideo peer add https://video.example.jp");
        println!();
        println!("Nodes on the same local network are found automatically.");
        return;
    }
    println!(
        "{} {} {}",
        pad("PEER", 24),
        pad("SOURCE", 10),
        pad("NAME", 24)
    );
    for row in &rows {
        println!(
            "{} {} {}",
            pad(&short_cid(str_field(row, "peerId")), 24),
            pad(str_field(row, "source"), 10),
            pad(str_field(row, "nodeName"), 24)
        );
    }
    println!();
    println!("{} peers", rows.len());
}

pub fn videos(value: &Value, empty_hint: &str) {
    let rows = value.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("{empty_hint}");
        return;
    }
    for row in &rows {
        let held = if bool_field(row, "haveContent") {
            "[local] "
        } else {
            ""
        };
        println!("{held}{}", str_field(row, "title"));
        let tags = row
            .get("tags")
            .and_then(Value::as_array)
            .map(|t| {
                t.iter()
                    .filter_map(Value::as_str)
                    .map(|s| format!("#{s}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        println!(
            "  {}  {}  {}",
            str_field(row, "cid"),
            duration(u64_field(row, "durationSecs")),
            tags
        );
    }
    println!();
    println!("{} videos", rows.len());
}

pub fn video_info(value: &Value) {
    println!("Title        {}", str_field(value, "title"));
    println!("CID          {}", str_field(value, "cid"));
    println!("Creator      {}", str_field(value, "creator"));
    println!(
        "Duration     {}",
        duration(u64_field(value, "durationSecs"))
    );
    let tags = value
        .get("tags")
        .and_then(Value::as_array)
        .map(|t| {
            t.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    println!("Tags         {tags}");
    println!(
        "Held here    {}",
        if bool_field(value, "haveContent") {
            "yes, every chunk"
        } else if bool_field(value, "haveManifest") {
            "manifest only"
        } else {
            "no"
        }
    );
    println!(
        "Published    {}",
        if bool_field(value, "isLocal") {
            "by this node"
        } else {
            "elsewhere"
        }
    );
    let description = str_field(value, "description");
    if !description.is_empty() {
        println!();
        println!("{description}");
    }
}

pub fn recommendations(value: &Value) {
    let rows = value.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("Nothing to recommend yet.");
        println!();
        println!("Discover some videos first, then watch a few:");
        println!("    ourvideo video list");
        println!("    ourvideo watch <CID> --seconds 120");
        return;
    }
    for (index, row) in rows.iter().enumerate() {
        println!(
            "{:>2}. {}  ({:+.3})",
            index + 1,
            str_field(row, "title"),
            row.get("score").and_then(Value::as_f64).unwrap_or(0.0)
        );
        println!("    {}", str_field(row, "cid"));
    }
    println!();
    println!("Why any of these? `ourvideo recommendation explain <CID>`");
}

pub fn explanation(value: &Value) {
    println!("{}", str_field(value, "title"));
    println!("{}", str_field(value, "cid"));
    println!();
    println!(
        "Score {:+.4}",
        value.get("score").and_then(Value::as_f64).unwrap_or(0.0)
    );
    println!();
    for reason in value
        .get("reasons")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let label = match reason.get("detail").and_then(Value::as_str) {
            Some(detail) => format!("{} {detail}", str_field(&reason, "factor")),
            None => str_field(&reason, "factor").to_string(),
        };
        println!(
            "  {} {:+.4}",
            pad(&label, 24),
            reason.get("value").and_then(Value::as_f64).unwrap_or(0.0)
        );
    }
    println!();
    println!("Computed on this device from this device's viewing history.");
}

pub fn preferences(value: &Value) {
    let rows = value.as_array().cloned().unwrap_or_default();
    if rows.is_empty() {
        println!("No preferences learned yet. Watch something.");
        return;
    }
    for row in &rows {
        println!(
            "  {} {:+.2}",
            pad(str_field(row, "tag"), 24),
            row.get("weight").and_then(Value::as_f64).unwrap_or(0.0)
        );
    }
    println!();
    println!("This model never leaves your device.");
}

pub fn watch_history(value: &Value) {
    let summary = value.get("summary").cloned().unwrap_or(Value::Null);
    println!(
        "{} viewing events across {} videos, {} watched in total.",
        u64_field(&summary, "eventCount"),
        u64_field(&summary, "distinctVideos"),
        duration(u64_field(&summary, "totalWatchedSecs"))
    );
    println!();
    for row in value
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        println!(
            "  {}  {:>5.0}%  {}",
            pad(&short_cid(str_field(&row, "cid")), 20),
            row.get("ratio").and_then(Value::as_f64).unwrap_or(0.0) * 100.0,
            duration(u64_field(&row, "watchedSecs"))
        );
    }
    println!();
    println!("Stored only on this device. `ourvideo privacy clear` erases it.");
}

/// The diagnosis, as a list a person can read top to bottom.
///
/// Findings come with what to do about them indented underneath, because a
/// problem the reader cannot act on is not worth their attention.
pub fn doctor(report: &Report) -> String {
    let mut out = String::new();
    out.push_str(&format!("Checking {}\n\n", report.data_dir.display()));

    let width = report
        .checks
        .iter()
        .map(|c| c.name.chars().count())
        .max()
        .unwrap_or(0);

    for check in &report.checks {
        let mark = match check.severity {
            Severity::Ok => "ok  ",
            Severity::Warning => "warn",
            Severity::Problem => "FAIL",
        };
        out.push_str(&format!(
            "  {mark}  {}  {}\n",
            pad(&check.name, width),
            check.detail
        ));
        if let Some(remedy) = &check.remedy {
            // Line the remedy up under the finding it belongs to.
            let indent = " ".repeat(width + 10);
            for line in wrap(remedy, 72) {
                out.push_str(&format!("{indent}{line}\n"));
            }
        }
    }

    out.push('\n');
    let (problems, warnings) = (report.problems(), report.warnings());
    match (problems, warnings) {
        (0, 0) => out.push_str("Nothing to fix.\n"),
        (0, w) => out.push_str(&format!(
            "Nothing to fix. {} worth knowing about.\n",
            plural(w, "warning")
        )),
        (p, 0) => out.push_str(&format!("{} to fix.\n", plural(p, "problem"))),
        (p, w) => out.push_str(&format!(
            "{} to fix, and {} worth knowing about.\n",
            plural(p, "problem"),
            plural(w, "warning")
        )),
    }
    out
}

fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Break `text` into lines of at most `width` characters, on word boundaries.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_sizes_read_naturally() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(10 * 1024 * 1024 * 1024), "10.0 GiB");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration(0), "0:00");
        assert_eq!(duration(65), "1:05");
        assert_eq!(duration(3661), "1:01:01");
    }

    #[test]
    fn padding_counts_characters_not_bytes() {
        assert_eq!(pad("ab", 4), "ab  ");
        assert_eq!(pad("ゲーム", 4).chars().count(), 4);
        assert_eq!(pad("abcdef", 3), "abc");
    }

    #[test]
    fn long_ids_are_shortened_but_stay_recognisable() {
        let cid = "bafyreiabcdefghijklmnopqrstuvwxyz234567";
        let short = short_cid(cid);
        assert!(short.starts_with("bafyreiab"));
        assert!(short.ends_with("234567"));
        assert!(short.chars().count() < cid.chars().count());
        assert_eq!(short_cid("short"), "short");
    }
}
