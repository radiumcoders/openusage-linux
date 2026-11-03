//! Claude Code's local session logs priced into daily spend: a port of 0.7.x's
//! `ClaudeLogUsageScanner` (ccusage's Claude adapter).
//!
//! - Roots are each `CLAUDE_CONFIG_DIR` entry (a config dir, or its `projects/`), else
//!   `$XDG_CONFIG_HOME/claude` and `~/.claude`; every `projects/**/*.jsonl` under them is read.
//! - A usage line needs `"usage":{`, a valid timestamp, no `null` in fields Claude never writes as
//!   null, a semver-ish `version`, and non-empty ids and model.
//! - Lines are deduplicated by `(message.id, requestId)`, with a second pass for sidechain logs that
//!   replay a parent message under a new request id. The parent wins, then the larger token total,
//!   then the line that carries `speed`.
//! - Advisor iterations count as their own entries under their own model.
//! - A line's `costUSD` is used when present; otherwise the tokens are priced. Lines nothing can price
//!   are left out of every figure.
//!
//! Files are cached by length and modification time for the process; a file that grew is read only
//! from where the last read stopped.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};

use crate::metric::{BreakdownModel, MetricKind, MetricLine, MetricValue, UsageBreakdown};
use crate::pricing::{Pricing, Tokens};

#[path = "local_parse.rs"]
mod parse;

use parse::parse_entries;
#[cfg(test)]
use parse::is_semver_prefix;

const MARKER: &str = "\"usage\":{";
pub(crate) const WINDOW_DAYS: i64 = 30;

/// One priced day: dollars, tokens, and each model's share.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Day {
    pub dollars: f64,
    pub tokens: f64,
    pub models: BTreeMap<String, (f64, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    timestamp: DateTime<Utc>,
    tokens: Tokens,
    message_id: Option<String>,
    request_id: Option<String>,
    sidechain: bool,
    has_speed: bool,
    cost_usd: Option<f64>,
    model: Option<String>,
}

struct CachedFile {
    len: u64,
    modified: SystemTime,
    /// Where the next read starts: just past the last complete line.
    offset: u64,
    entries: Vec<Entry>,
}

static CACHE: Mutex<Option<HashMap<PathBuf, CachedFile>>> = Mutex::new(None);

/// The roots Claude Code writes to, as on the Mac.
pub fn roots() -> Vec<PathBuf> {
    let home = crate::paths::home();
    let configured = std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .filter(|raw| !raw.trim().is_empty());
    let candidates: Vec<PathBuf> = match configured {
        Some(raw) => raw
            .split(',')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| {
                let path = expand_home(part, &home);
                if path.file_name().is_some_and(|name| name == "projects") {
                    path.parent().map(Path::to_path_buf).unwrap_or(path)
                } else {
                    path
                }
            })
            .collect(),
        None => {
            let xdg = std::env::var_os("XDG_CONFIG_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            vec![xdg.join("claude"), home.join(".claude")]
        }
    };
    let mut roots: Vec<PathBuf> = Vec::new();
    for root in candidates {
        if root.join("projects").is_dir() && !roots.contains(&root) {
            roots.push(root);
        }
    }
    roots
}

fn expand_home(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if path == "~" => home.to_path_buf(),
        None => PathBuf::from(path),
    }
}

/// The account UUID Claude Code is signed in to, from its own `.claude.json`.
pub fn signed_in_account() -> Option<String> {
    let home = crate::paths::home();
    let mut files: Vec<PathBuf> = roots().iter().map(|root| root.join(".claude.json")).collect();
    files.push(home.join(".claude.json"));
    files.into_iter().find_map(|file| {
        let text = std::fs::read_to_string(file).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        value
            .get("oauthAccount")?
            .get("accountUuid")?
            .as_str()
            .filter(|uuid| !uuid.is_empty())
            .map(str::to_lowercase)
    })
}

/// Each local day of the window, oldest first, or none when there are no Claude logs at all.
pub fn scan(now: DateTime<Utc>) -> Option<BTreeMap<NaiveDate, Day>> {
    let roots = roots();
    if roots.is_empty() {
        return None;
    }
    let since = window_start(now);
    let mut files: Vec<(PathBuf, std::fs::Metadata)> = Vec::new();
    for root in &roots {
        collect_jsonl(&root.join("projects"), &mut files);
    }
    if files.is_empty() {
        return None;
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let since_system: SystemTime = since.into();
    let mut guard = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    let mut seen = Vec::new();
    let mut entries: Vec<Entry> = Vec::new();
    for (path, metadata) in files {
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        // A file untouched since before the window can't hold a line inside it.
        if modified < since_system {
            continue;
        }
        let cached = cache.remove(&path);
        let file = refresh_file(&path, metadata.len(), modified, cached, since);
        entries.extend(file.entries.iter().cloned());
        seen.push(path.clone());
        cache.insert(path, file);
    }
    cache.retain(|path, _| seen.contains(path));
    drop(guard);
    Some(aggregate(dedup(entries), since, Pricing::bundled()))
}

/// The start of the local day `WINDOW_DAYS - 1` days ago, so the window is thirty days with today.
pub fn window_start(now: DateTime<Utc>) -> DateTime<Utc> {
    let day = now.with_timezone(&Local).date_naive() - Duration::days(WINDOW_DAYS - 1);
    local_midnight(day)
}

fn local_midnight(day: NaiveDate) -> DateTime<Utc> {
    let midnight = day.and_hms_opt(0, 0, 0).expect("midnight exists");
    Local
        .from_local_datetime(&midnight)
        .earliest()
        .map(|local| local.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&midnight))
}

fn collect_jsonl(directory: &Path, out: &mut Vec<(PathBuf, std::fs::Metadata)>) {
    let Ok(read) = std::fs::read_dir(directory) else {
        return;
    };
    for item in read.flatten() {
        let path = item.path();
        let Ok(kind) = item.file_type() else { continue };
        if kind.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().is_some_and(|extension| extension == "jsonl") {
            if let Ok(metadata) = item.metadata() {
                out.push((path, metadata));
            }
        }
    }
}

fn refresh_file(
    path: &Path,
    len: u64,
    modified: SystemTime,
    cached: Option<CachedFile>,
    since: DateTime<Utc>,
) -> CachedFile {
    let (mut offset, mut entries) = match cached {
        Some(file) if file.len == len && file.modified == modified => {
            return CachedFile { entries: pruned(file.entries, since), ..file };
        }
        // Grown: read only what was appended.
        Some(file) if len >= file.offset => (file.offset, pruned(file.entries, since)),
        _ => (0, Vec::new()),
    };
    match read_from(path, offset) {
        Ok((read, parsed)) => {
            offset += read;
            entries.extend(parsed.into_iter().filter(|entry| entry.timestamp >= since));
        }
        Err(error) => {
            crate::log::warn(
                "plugin:claude",
                &format!("couldn't read a Claude Code log: {error}"),
            );
        }
    }
    CachedFile {
        len,
        modified,
        offset,
        entries,
    }
}

fn pruned(mut entries: Vec<Entry>, since: DateTime<Utc>) -> Vec<Entry> {
    entries.retain(|entry| entry.timestamp >= since);
    entries
}

/// Parses the complete lines from `offset` on; returns the bytes consumed and the entries.
fn read_from(path: &Path, offset: u64) -> std::io::Result<(u64, Vec<Entry>)> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut consumed = 0u64;
    let mut entries = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 || line.last() != Some(&b'\n') {
            // End of file, or a line still being written: read it next time.
            break;
        }
        consumed += read as u64;
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        if text.contains(MARKER) {
            entries.extend(parse_entries(text));
        }
    }
    Ok((consumed, entries))
}

// MARK: - Dedup and aggregation

fn dedup(entries: Vec<Entry>) -> Vec<Entry> {
    let mut kept: Vec<Entry> = Vec::new();
    let mut exact: HashMap<(String, Option<String>), usize> = HashMap::new();
    let mut by_message: HashMap<String, Vec<usize>> = HashMap::new();
    for entry in entries {
        let Some(message_id) = entry.message_id.clone() else {
            kept.push(entry);
            continue;
        };
        let key = (message_id.clone(), entry.request_id.clone());
        let collision = exact.get(&key).copied().or_else(|| {
            by_message.get(&message_id).and_then(|indexes| {
                indexes
                    .iter()
                    .copied()
                    .find(|index| entry.sidechain || kept[*index].sidechain)
            })
        });
        if let Some(index) = collision {
            if replaces(&entry, &kept[index]) {
                let old = &kept[index];
                if let Some(old_id) = &old.message_id {
                    exact.remove(&(old_id.clone(), old.request_id.clone()));
                }
                kept[index] = entry;
                exact.insert(key, index);
            }
            continue;
        }
        let index = kept.len();
        kept.push(entry);
        exact.insert(key, index);
        by_message.entry(message_id).or_default().push(index);
    }
    kept
}

fn replaces(candidate: &Entry, existing: &Entry) -> bool {
    if candidate.sidechain != existing.sidechain {
        return existing.sidechain;
    }
    let (new, old) = (candidate.tokens.total(), existing.tokens.total());
    if new != old {
        return new > old;
    }
    candidate.has_speed && !existing.has_speed
}

fn aggregate(entries: Vec<Entry>, since: DateTime<Utc>, pricing: &Pricing) -> BTreeMap<NaiveDate, Day> {
    let mut days: BTreeMap<NaiveDate, Day> = BTreeMap::new();
    for entry in entries.into_iter().filter(|entry| entry.timestamp >= since) {
        let model = entry
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty());
        let cost = match (entry.cost_usd, model) {
            (Some(carried), _) => carried,
            (None, Some(model)) => match pricing.cost(model, &entry.tokens) {
                Some(cost) => cost,
                None => continue,
            },
            (None, None) => continue,
        };
        let day = entry.timestamp.with_timezone(&Local).date_naive();
        let slot = days.entry(day).or_default();
        let tokens = entry.tokens.total() as f64;
        slot.dollars += cost;
        slot.tokens += tokens;
        let name = model.unwrap_or("Unattributed").to_string();
        let share = slot.models.entry(name).or_insert((0.0, 0.0));
        share.0 += cost;
        share.1 += tokens;
    }
    days
}

/// Today, Yesterday, and Last 30 Days, with each row's models, from scanned days.
pub fn history_lines(
    days: &BTreeMap<NaiveDate, Day>,
    now: DateTime<Utc>,
) -> (Vec<MetricLine>, BTreeMap<String, UsageBreakdown>) {
    let today = now.with_timezone(&Local).date_naive();
    let mut lines = Vec::new();
    let mut breakdowns = BTreeMap::new();
    for (label, key, from, to) in [
        ("Today", "today", 0, 1),
        ("Yesterday", "yesterday", 1, 2),
        ("Last 30 Days", "last30", 0, WINDOW_DAYS),
    ] {
        let mut dollars = 0.0;
        let mut tokens = 0.0;
        let mut models: BTreeMap<String, (f64, f64)> = BTreeMap::new();
        for back in from..to {
            let Some(day) = days.get(&(today - Duration::days(back))) else {
                continue;
            };
            dollars += day.dollars;
            tokens += day.tokens;
            for (name, (cost, count)) in &day.models {
                let share = models.entry(name.clone()).or_insert((0.0, 0.0));
                share.0 += cost;
                share.1 += count;
            }
        }
        lines.push(MetricLine::values(
            label,
            vec![
                MetricValue::new(dollars, MetricKind::Dollars),
                MetricValue::labeled(tokens, MetricKind::Count, "tokens"),
            ],
        ));
        let ranked = UsageBreakdown::ranked(
            models
                .into_iter()
                .map(|(name, (cost, count))| BreakdownModel {
                    name,
                    dollars: Some(cost),
                    tokens: Some(count),
                    share: None,
                })
                .collect(),
        );
        if let Some(breakdown) = ranked {
            breakdowns.insert(key.to_string(), breakdown);
        }
    }
    (lines, breakdowns)
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
