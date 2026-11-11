//! The pure perf-report logic behind `cargo xtask perf-report`: read one
//! side's Criterion output (`<root>/<group>/<id>/new/estimates.json` →
//! `median.point_estimate`) plus a directory of command timings/binary sizes,
//! and render the Markdown comparison the CI step summary shows. Every input
//! is pre-parsed so [`render_report`] stays a pure function — the unit tests
//! exercise it without touching criterion or the filesystem's real layout.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

/// One side's benchmark ids → median point estimate (ns). `None` means the
/// id ran (a `new/` dir exists) but produced no usable `estimates.json` — a
/// crashed or truncated measurement, rendered `n/a`.
pub type Estimates = BTreeMap<String, Option<f64>>;

/// Wall-clock + max-RSS for one timed command.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommandMetric {
    pub secs: f64,
    pub rss_kib: u64,
}

/// Everything the report renders — all inputs pre-parsed so the renderer is
/// pure and unit-testable.
#[derive(Debug)]
pub struct Report {
    /// e.g. `base (30285a9)` / `head` — the column headers.
    pub base_label: String,
    pub head_label: String,
    /// `None` = the whole side's benchmark run failed (no criterion dir) —
    /// every row shows `n/a` rather than looking "new"/"removed".
    pub base: Option<Estimates>,
    pub head: Option<Estimates>,
    /// Command name → (base, head); `None` = that side's timing is unusable.
    pub commands: CommandMetrics,
    /// (base, head) release-binary byte counts.
    pub binary_size: (Option<u64>, Option<u64>),
    /// True when the change touched the benchmark code itself — deltas then
    /// partly reflect a changed measurement set, not just implementation
    /// drift, so the report says so.
    pub benches_changed: bool,
}

/// A benchmark id's state on ONE side, resolved against the spec's
/// new/removed/n-a distinction.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Side {
    /// The whole side's run failed (no criterion data at all).
    RunFailed,
    /// The side measured fine but has no such id.
    Absent,
    /// The id exists on this side but its estimate is unusable.
    Failed,
    /// Median point estimate in nanoseconds.
    Value(f64),
}

fn side(side: Option<&Estimates>, id: &str) -> Side {
    match side {
        None => Side::RunFailed,
        Some(m) => match m.get(id) {
            None => Side::Absent,
            Some(None) => Side::Failed,
            Some(Some(v)) => Side::Value(*v),
        },
    }
}

/// The regression threshold the workflow's contract fixes: +10% or worse is
/// flagged — informational only, the job never gates on it.
const SLOWER: f64 = 0.10;

fn delta_cell(base: f64, head: f64) -> String {
    if base <= 0.0 {
        return "n/a".to_string();
    }
    let d = head / base - 1.0;
    if d >= SLOWER {
        format!("{:+.1}% **slower**", d * 100.0)
    } else {
        format!("{:+.1}%", d * 100.0)
    }
}

fn fmt_ns(ns: f64) -> String {
    if ns >= 1e9 {
        format!("{:.2} s", ns / 1e9)
    } else if ns >= 1e6 {
        format!("{:.1} ms", ns / 1e6)
    } else if ns >= 1e3 {
        format!("{:.0} µs", ns / 1e3)
    } else {
        format!("{ns:.0} ns")
    }
}

fn fmt_kib(kib: u64) -> String {
    format!("{:.0} MiB", kib as f64 / 1024.0)
}

fn fmt_bytes(bytes: u64) -> String {
    format!("{:.2} MiB", bytes as f64 / (1024.0 * 1024.0))
}

fn fmt_side(s: Side) -> String {
    match s {
        Side::RunFailed | Side::Failed => "n/a".to_string(),
        Side::Absent => "—".to_string(),
        Side::Value(v) => fmt_ns(v),
    }
}

/// Render the full Markdown report: a benchmark table over the union of
/// both sides' ids, a command table (wall time / max RSS), and the binary
/// size line.
pub fn render_report(r: &Report) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "## Perf report\n\n`{}` → `{}` — Criterion median point estimates, lower is better.\n\n",
        r.base_label, r.head_label
    ));

    let mut ids: BTreeSet<&String> = BTreeSet::new();
    if let Some(m) = &r.base {
        ids.extend(m.keys());
    }
    if let Some(m) = &r.head {
        ids.extend(m.keys());
    }

    out.push_str("| benchmark | base | head | Δ |\n");
    out.push_str("|---|---:|---:|---:|\n");
    for id in ids {
        let b = side(r.base.as_ref(), id);
        let h = side(r.head.as_ref(), id);
        let delta = match (b, h) {
            (Side::Value(bv), Side::Value(hv)) => delta_cell(bv, hv),
            (Side::Absent, Side::Value(_)) => "new".to_string(),
            (Side::Value(_), Side::Absent) => "removed".to_string(),
            _ => "n/a".to_string(),
        };
        out.push_str(&format!(
            "| {id} | {} | {} | {delta} |\n",
            fmt_side(b),
            fmt_side(h)
        ));
    }

    if !r.commands.is_empty() {
        out.push_str("\nCommands on the generated 10k-model project — wall time / max RSS:\n\n");
        out.push_str("| command | base | head | Δ |\n");
        out.push_str("|---|---:|---:|---:|\n");
        for (name, (b, h)) in &r.commands {
            let cell = |m: &Option<CommandMetric>| match m {
                None => "n/a".to_string(),
                Some(m) => format!("{:.2} s / {}", m.secs, fmt_kib(m.rss_kib)),
            };
            let delta = match (b, h) {
                (Some(bv), Some(hv)) => delta_cell(bv.secs, hv.secs),
                _ => "n/a".to_string(),
            };
            out.push_str(&format!(
                "| {name} | {} | {} | {delta} |\n",
                cell(b),
                cell(h)
            ));
        }
    }

    let size = |v: Option<u64>| match v {
        None => "n/a".to_string(),
        Some(b) => fmt_bytes(b),
    };
    let size_delta = match r.binary_size {
        (Some(b), Some(h)) if b > 0 => format!(" ({:+.1}%)", (h as f64 / b as f64 - 1.0) * 100.0),
        _ => String::new(),
    };
    out.push_str(&format!(
        "\nRelease binary size: {} → {}{size_delta}.\n",
        size(r.binary_size.0),
        size(r.binary_size.1)
    ));

    if r.benches_changed {
        out.push_str(
            "\n> Note: this change modifies the benchmark code itself, so part of the delta\n\
             > reflects a changed benchmark set rather than implementation drift.\n",
        );
    }
    out
}

/// Read one side's Criterion output root into an [`Estimates`] map.
/// `Ok(None)` = the dir doesn't exist (that side's run failed wholesale).
///
/// Criterion writes `<root>/<group>/<id>/<param?>/new/estimates.json`; the
/// benchmark id is the `new/` dir's parent relative to `root`
/// (`subgraph_view/rooted/full`). A `new/` dir with no readable
/// `estimates.json` records `None` (`n/a`) — a crash mid-run keeps the row.
pub fn read_estimates(root: &Path) -> io::Result<Option<Estimates>> {
    if !root.is_dir() {
        return Ok(None);
    }
    let mut out = Estimates::new();
    collect(root, root, &mut out)?;
    Ok(Some(out))
}

fn collect(root: &Path, dir: &Path, out: &mut Estimates) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        // A criterion run dir `new/` holds only files; an id directory named
        // `new` would hold subdirs — the distinction keeps `foo/new/new`
        // resolving to id `foo/new` correctly.
        let is_run_dir = path.file_name().is_some_and(|n| n == "new")
            && !fs::read_dir(&path)?.any(|e| e.is_ok_and(|e| e.path().is_dir()));
        if is_run_dir {
            let id = path
                .parent()
                .and_then(|p| p.strip_prefix(root).ok())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            out.insert(id, read_estimate(&path.join("estimates.json")));
        } else {
            collect(root, &path, out)?;
        }
    }
    Ok(())
}

fn read_estimate(path: &Path) -> Option<f64> {
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    v.get("median")?.get("point_estimate")?.as_f64()
}

/// Command name → (base, head) measurements — `None` per side = unusable.
pub type CommandMetrics = BTreeMap<String, (Option<CommandMetric>, Option<CommandMetric>)>;

/// What the metrics dir held: per-command timings plus the two sides'
/// release-binary sizes.
#[derive(Debug)]
pub struct MetricsInput {
    pub commands: CommandMetrics,
    /// (base, head) byte counts.
    pub binary_size: (Option<u64>, Option<u64>),
}

/// Read the command/binary-size metrics dir the workflow writes:
/// `<name>.{base,head}` files holding `secs rss_kib exit` (an exit ≥ 2 —
/// usage or input error — voids the measurement → `n/a`), plus `size.{base,
/// head}` byte counts.
pub fn read_metrics(dir: &Path) -> io::Result<MetricsInput> {
    let mut commands: CommandMetrics = BTreeMap::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            // Only `<cmd>.base` / `<cmd>.head` — the `.time` sidecars GNU
            // time writes must not become phantom `<cmd>.base` commands.
            let ext = path.extension().and_then(|e| e.to_str());
            if ext != Some("base") && ext != Some("head") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem != "size" {
                names.insert(stem.to_string());
            }
        }
    }
    for name in names {
        commands.insert(
            name.clone(),
            (
                parse_metric(&dir.join(format!("{name}.base"))),
                parse_metric(&dir.join(format!("{name}.head"))),
            ),
        );
    }
    Ok(MetricsInput {
        commands,
        binary_size: (
            parse_size(&dir.join("size.base")),
            parse_size(&dir.join("size.head")),
        ),
    })
}

fn parse_metric(path: &Path) -> Option<CommandMetric> {
    let text = fs::read_to_string(path).ok()?;
    let mut it = text.split_whitespace();
    let secs: f64 = it.next()?.parse().ok()?;
    let rss_kib: u64 = it.next()?.parse().ok()?;
    let exit: i32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    // Exit 1 is a gate finding (check --lint reports violations) — still a
    // valid measurement. Exit ≥2 is a usage/input error: unusable → n/a.
    (exit < 2).then_some(CommandMetric { secs, rss_kib })
}

fn parse_size(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(base: Estimates, head: Estimates) -> Report {
        Report {
            base_label: "base".to_string(),
            head_label: "head".to_string(),
            base: Some(base),
            head: Some(head),
            commands: BTreeMap::new(),
            binary_size: (None, None),
            benches_changed: false,
        }
    }

    #[test]
    fn regression_over_ten_percent_is_marked() {
        let mut base = Estimates::new();
        let mut head = Estimates::new();
        base.insert("dag_build/500".to_string(), Some(100_000_000.0));
        head.insert("dag_build/500".to_string(), Some(112_000_000.0));
        base.insert("dag_build/8000".to_string(), Some(2_000_000_000.0));
        head.insert("dag_build/8000".to_string(), Some(2_100_000_000.0));
        let md = render_report(&report(base, head));
        assert!(md.contains("| dag_build/500 | 100.0 ms | 112.0 ms | +12.0% **slower** |"));
        // Exactly +10% marks too; +5% stays unmarked.
        assert!(md.contains("| dag_build/8000 | 2.00 s | 2.10 s | +5.0% |"));
    }

    #[test]
    fn new_and_removed_rows() {
        let mut base = Estimates::new();
        let mut head = Estimates::new();
        base.insert("old_group/a".to_string(), Some(50_000_000.0));
        head.insert("new_group/b".to_string(), Some(60_000_000.0));
        let md = render_report(&report(base, head));
        assert!(md.contains("| old_group/a | 50.0 ms | — | removed |"));
        assert!(md.contains("| new_group/b | — | 60.0 ms | new |"));
    }

    #[test]
    fn failed_measurements_render_na() {
        let mut base = Estimates::new();
        let mut head = Estimates::new();
        // Ran but produced no estimate.
        base.insert("broken/1".to_string(), None);
        head.insert("broken/1".to_string(), Some(1_000_000.0));
        let mut r = report(base, head);
        // And a wholesale side failure: no estimates at all.
        r.base = None;
        r.head
            .as_mut()
            .unwrap()
            .insert("x/1".to_string(), Some(2_000_000.0));
        let md = render_report(&r);
        assert!(md.contains("| broken/1 | n/a | 1.0 ms | n/a |"));
        assert!(md.contains("| x/1 | n/a | 2.0 ms | n/a |"));
    }

    #[test]
    fn number_formatting() {
        assert_eq!(fmt_ns(500.0), "500 ns");
        assert_eq!(fmt_ns(12_300.0), "12 µs");
        assert_eq!(fmt_ns(407_320_000.0), "407.3 ms");
        assert_eq!(fmt_ns(2_050_000_000.0), "2.05 s");
        assert_eq!(fmt_kib(120_832), "118 MiB");
        assert_eq!(fmt_bytes(5_695_001), "5.43 MiB");
    }

    #[test]
    fn commands_and_size() {
        let mut r = report(Estimates::new(), Estimates::new());
        r.commands.insert(
            "check".to_string(),
            (
                Some(CommandMetric {
                    secs: 1.20,
                    rss_kib: 120_832,
                }),
                Some(CommandMetric {
                    secs: 1.44,
                    rss_kib: 121_000,
                }),
            ),
        );
        r.commands.insert(
            "docs".to_string(),
            (
                Some(CommandMetric {
                    secs: 8.0,
                    rss_kib: 5,
                }),
                None,
            ),
        );
        r.binary_size = (Some(5_695_001), Some(5_800_000));
        let md = render_report(&r);
        assert!(md.contains("| check | 1.20 s / 118 MiB | 1.44 s / 118 MiB | +20.0% **slower** |"));
        assert!(md.contains("| docs | 8.00 s / 0 MiB | n/a | n/a |"));
        assert!(md.contains("Release binary size: 5.43 MiB → 5.53 MiB (+1.8%)."));
    }

    #[test]
    fn benches_changed_note() {
        let mut r = report(Estimates::new(), Estimates::new());
        r.benches_changed = true;
        assert!(render_report(&r).contains("changed benchmark set"));
    }

    #[test]
    fn read_estimates_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let run = dir.path().join("dag_build/500/new");
        fs::create_dir_all(&run).unwrap();
        fs::write(
            run.join("estimates.json"),
            r#"{"median": {"point_estimate": 407451.0, "standard_error": 1.0}}"#,
        )
        .unwrap();
        // A multi-segment id, like subgraph_view/rooted/full.
        let run2 = dir.path().join("subgraph_view/rooted/full/new");
        fs::create_dir_all(&run2).unwrap();
        fs::write(
            run2.join("estimates.json"),
            r#"{"median":{"point_estimate":1.5e9}}"#,
        )
        .unwrap();
        // A crashed run: dir exists, no estimates.
        fs::create_dir_all(dir.path().join("dead/1/new")).unwrap();
        let m = read_estimates(dir.path()).unwrap().unwrap();
        assert_eq!(m["dag_build/500"], Some(407451.0));
        assert_eq!(m["subgraph_view/rooted/full"], Some(1.5e9));
        assert_eq!(m["dead/1"], None);
        assert!(read_estimates(&dir.path().join("missing"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn read_metrics_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("check.base"), "1.20 120832 1\n").unwrap();
        fs::write(dir.path().join("check.head"), "2.40 121000 0\n").unwrap();
        fs::write(dir.path().join("size.base"), "5695001\n").unwrap();
        let metrics = read_metrics(dir.path()).unwrap();
        let (commands, size) = (metrics.commands, metrics.binary_size);
        // Exit 1 (gate findings) still counts as measured.
        assert_eq!(
            commands["check"].0,
            Some(CommandMetric {
                secs: 1.20,
                rss_kib: 120_832
            })
        );
        assert_eq!(commands["check"].1.unwrap().secs, 2.40);
        assert_eq!(size.0, Some(5_695_001));
        assert_eq!(size.1, None);
    }

    #[test]
    fn exit_code_two_voids_a_measurement() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("docs.base"), "0.01 100 2\n").unwrap();
        let commands = read_metrics(dir.path()).unwrap().commands;
        assert_eq!(commands["docs"].0, None);
    }
}
