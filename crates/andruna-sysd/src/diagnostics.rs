//! Missing-function counters per package
//! ([03-components.md](../../docs/spec/03-components.md#cross-cutting-missing-function-counters)).
//!
//! A host process reports the stubbed or unimplemented entry points its app
//! called (`ReportMissingFunctions`); sysd adds them to the package's
//! counters in `diagnostics\<package>.missing` and the manager's
//! Diagnostics page reads them (`GetMissingFunctions`). The file is UTF-8,
//! one `<calls>\t<name>` line per entry, most-called first; it survives
//! restarts and is removed with the package's data.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use andruna_ipc::{MissingFunction, is_valid_package_name};

/// Entries kept per package; the least-called are dropped beyond this.
pub const MAX_ENTRIES: usize = 500;

/// Longest accepted entry name, in bytes.
pub const MAX_NAME: usize = 200;

/// Serializes read-modify-write of the counter files.
static LOCK: Mutex<()> = Mutex::new(());

fn file(dir: &Path, package: &str) -> PathBuf {
    dir.join(format!("{package}.missing"))
}

fn parse(text: &str) -> Vec<MissingFunction> {
    text.lines()
        .filter_map(|l| {
            let (calls, name) = l.split_once('\t')?;
            Some(MissingFunction {
                name: name.to_string(),
                calls: calls.parse().ok()?,
            })
        })
        .collect()
}

/// The counters of `package`, most-called first, and when they were last
/// written (milliseconds since the Unix epoch, 0 when there are none).
pub fn read(dir: &Path, package: &str) -> io::Result<(u64, Vec<MissingFunction>)> {
    if !is_valid_package_name(package) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid package name",
        ));
    }
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = file(dir, package);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok((0, Vec::new())),
        Err(e) => return Err(e),
    };
    let updated = fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_millis() as u64);
    Ok((updated, parse(&text)))
}

/// Adds a report to the counters of `package`. Entries with an empty,
/// over-long or multi-line name are ignored.
pub fn add(dir: &Path, package: &str, report: &[MissingFunction]) -> io::Result<()> {
    if !is_valid_package_name(package) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid package name",
        ));
    }
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = file(dir, package);
    let mut counts: HashMap<String, u64> = match fs::read_to_string(&path) {
        Ok(t) => parse(&t).into_iter().map(|e| (e.name, e.calls)).collect(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => return Err(e),
    };
    for e in report {
        if e.name.is_empty() || e.name.len() > MAX_NAME || e.name.contains(['\t', '\n', '\r']) {
            continue;
        }
        let c = counts.entry(e.name.clone()).or_default();
        *c = c.saturating_add(e.calls);
    }
    let mut entries: Vec<(String, u64)> = counts.into_iter().collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    entries.truncate(MAX_ENTRIES);
    let text: String = entries.iter().map(|(n, c)| format!("{c}\t{n}\n")).collect();
    fs::create_dir_all(dir)?;
    let tmp = path.with_extension("missing.tmp");
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mf(name: &str, calls: u64) -> MissingFunction {
        MissingFunction {
            name: name.into(),
            calls,
        }
    }

    #[test]
    fn reports_add_up_most_called_first() {
        let dir = std::env::temp_dir().join(format!("andruna-diag-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(read(&dir, "a.b").unwrap(), (0, vec![]));
        add(
            &dir,
            "a.b",
            &[mf("lx:x", 2), mf("service:y", 1), mf("bad\tname", 9)],
        )
        .unwrap();
        add(&dir, "a.b", &[mf("service:y", 5), mf("", 3)]).unwrap();
        let (updated, entries) = read(&dir, "a.b").unwrap();
        assert!(updated > 0);
        assert_eq!(entries, vec![mf("service:y", 6), mf("lx:x", 2)]);
        assert!(read(&dir, "..").is_err());
        assert!(add(&dir, "a/b", &[]).is_err());

        let many: Vec<_> = (0..MAX_ENTRIES as u64 + 20)
            .map(|i| mf(&format!("lx:f{i}"), i + 10))
            .collect();
        add(&dir, "c.d", &many).unwrap();
        let (_, entries) = read(&dir, "c.d").unwrap();
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(entries[0].calls, MAX_ENTRIES as u64 + 29);
        fs::remove_dir_all(&dir).unwrap();
    }
}
