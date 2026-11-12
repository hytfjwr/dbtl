//! CLI-level coverage for the shared report formats: `check`/`lint`'s seven
//! `--format` renderers, the `--report <FMT>:<PATH>` file destinations,
//! `--step-summary`, `--path-prefix`, `check --docs-dir`, and `impact`'s
//! `--report` — all through the real binary, no TTY involved.

mod common;

use common::{dbtl_cmd, fixture_path, manifest_path as fixture_manifest};
use std::path::{Path, PathBuf};

fn run(args: &[&str]) -> std::process::Output {
    dbtl_cmd()
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("failed to run dbtl")
}

fn out_text(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}
fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Copy the fixture manifest into `dir` — tests that place `.dbtl.yml` or
/// report files beside the data source need an owned directory.
fn manifest_in(dir: &Path) -> PathBuf {
    let dst = dir.join("manifest.json");
    std::fs::copy(fixture_manifest(), &dst).unwrap();
    dst
}

fn check_on(manifest: &Path, extra: &[&str]) -> std::process::Output {
    let mut args: Vec<&str> = vec!["check", "--manifest"];
    args.push(manifest.to_str().unwrap());
    args.extend_from_slice(extra);
    run(&args)
}

// ---- --format: the stdout shapes -------------------------------------------

#[test]
fn check_default_format_is_the_text_line_report() {
    let m = fixture_manifest();
    let out = check_on(&m, &[]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let stdout = out_text(&out);
    assert!(stdout.starts_with("scope: root project only\n"));
    assert!(
        stdout.lines().any(|l| l.starts_with("PASS coverage:")),
        "gate lines: {stdout}"
    );
    // The JSON schema marker and the markdown marker must NOT appear —
    // this is the plain line report.
    assert!(!stdout.contains("dbtl.report") && !stdout.contains("<!-- dbtl:report"));
}

#[test]
fn check_format_markdown_is_real_markdown() {
    let m = fixture_manifest();
    let out = check_on(&m, &["--lint", "--format", "markdown"]);
    assert_eq!(out.status.code(), Some(1), "lint trips the gate");
    let stdout = out_text(&out);
    assert!(
        stdout.starts_with("<!-- dbtl:report -->\n"),
        "the marker leads the document: {stdout}"
    );
    assert!(
        stdout.contains("| Gate | Severity | Status | Message |"),
        "the summary table: {stdout}"
    );
    assert!(
        stdout.contains("## lint direct_join_to_source"),
        "a per-gate section: {stdout}"
    );
    // Findings list under the gate section as file anchors — a `.sql`
    // finding carries its call-site `line:col` (the source() call that
    // made the edge).
    assert!(
        stdout.contains("- models/utilities/pos_prep/pos_cat.sql:4:18 direct_join_to_source"),
        "a finding row: {stdout}"
    );
}

#[test]
fn check_format_json_is_the_report_document() {
    let m = fixture_manifest();
    let out = check_on(&m, &["--lint", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out_text(&out)).expect("stdout is pure JSON");
    assert_eq!(v["schema"], "dbtl.report/v1");
    assert_eq!(v["project"], "jaffle_finance");
    assert_eq!(v["scope"], "root project only");
    assert!(v["gates"].as_array().unwrap().len() > 1);
    for key in ["errors", "warnings", "baselined", "skipped"] {
        assert!(v["summary"].get(key).is_some(), "summary.{key}");
    }
    // The conditional blocks stay absent without --diff / --docs-dir.
    assert!(v.get("impact").is_none(), "impact only under --diff");
    assert!(
        v.get("docs_drift").is_none(),
        "docs_drift only under --docs-dir"
    );
}

#[test]
fn check_json_embeds_the_diff_report_under_diff() {
    let m = fixture_manifest();
    let out = check_on(&m, &["--format", "json", "--diff", m.to_str().unwrap()]);
    assert!(out.status.success(), "self-diff passes: {}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&out_text(&out)).expect("json");
    // `impact` is the same DiffReport `dbtl impact` prints — the baseline
    // label and the pr-impact block are both inside it.
    let impact = &v["impact"];
    assert!(impact.is_object(), "impact embeds under --diff");
    assert_eq!(impact["baseline"], m.to_str().unwrap());
    assert!(impact.get("pr").is_some(), "the PrImpact block rides along");
}

#[test]
fn check_format_github_emits_workflow_commands() {
    let m = fixture_manifest();
    let out = check_on(&m, &["--lint", "--format", "github"]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = out_text(&out);
    // A file-bound annotation carries file/title — `line` rides along only
    // when the finding's position is actually known, never a guessed 1;
    // graph findings pin the call site (line 4, columns 18–58 = the
    // source() call's real span).
    assert!(
        stdout.contains(
            "::warning file=models/utilities/pos_prep/pos_cat.sql,line=4,endLine=4,col=18,endColumn=58,title=direct_join_to_source::"
        ),
        "the annotation shape: {stdout}"
    );
    // The summary lands as a trailing notice.
    let last = stdout.lines().last().unwrap();
    assert!(
        last.starts_with("::notice title=dbtl::"),
        "the counts notice: {last}"
    );
}

#[test]
fn check_format_sarif_is_a_valid_210_document() {
    let m = fixture_manifest();
    let out = check_on(&m, &["--lint", "--format", "sarif"]);
    let v: serde_json::Value = serde_json::from_str(&out_text(&out)).expect("sarif parses");
    assert_eq!(v["version"], "2.1.0");
    assert_eq!(
        v["$schema"], "https://json.schemastore.org/sarif-2.1.0.json",
        "the schema reference"
    );
    let run = &v["runs"][0];
    assert_eq!(run["tool"]["driver"]["name"], "dbtl");
    // The driver declares the whole registry — every shipped rule id.
    let rules = run["tool"]["driver"]["rules"].as_array().unwrap();
    assert_eq!(
        rules.len(),
        dbtl::REGISTRY.len(),
        "one driver rule per registry entry"
    );
    assert!(rules.iter().any(|r| r["id"] == "direct_join_to_source"));
    // The result count is exactly the counted-finding set the json report
    // carries — the formats can't disagree.
    let results = run["results"].as_array().unwrap();
    assert_eq!(results.len(), counted_findings(&m));
    assert!(results.iter().all(|r| r["ruleId"].is_string()
        && r["message"]["text"].is_string()
        && r["partialFingerprints"]["dbtl.finding/v1"].is_string()));
    assert!(results
        .iter()
        .any(|r| r["locations"][0]["physicalLocation"]["region"]["startLine"].is_number()));
}

/// The counted (non-baselined) findings `check --lint --format json`
/// carries — the oracle every finding-listing format must match.
fn counted_findings(m: &Path) -> usize {
    let out = check_on(m, &["--lint", "--format", "json"]);
    let v: serde_json::Value = serde_json::from_str(&out_text(&out)).unwrap();
    v["gates"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|g| {
            g["findings"]
                .as_array()
                .map(|a| a.iter())
                .unwrap_or_default()
        })
        .filter(|f| f["baselined"] == false)
        .count()
}

#[test]
fn check_format_junit_is_wellformed_xml() {
    let m = fixture_manifest();
    let out = check_on(&m, &["--lint", "--format", "junit"]);
    let stdout = out_text(&out);
    assert!(stdout.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
    assert!(stdout.ends_with("</testsuite>\n</testsuites>\n"));
    // One testcase per gate: coverage + every enabled lint rule.
    let cases = stdout.matches("<testcase").count();
    assert_eq!(
        cases,
        1 + dbtl::REGISTRY.len(),
        "coverage + all lint rules: {stdout}"
    );
    // Every FAIL gate is a `<failure>` testcase carrying its findings —
    // only missing_primary_key_tests still gates by default on the fixture.
    let fails = stdout.matches("<failure>").count();
    assert_eq!(fails, 1, "the failing lint gates: {stdout}");
    // The fixture has no catalog sibling — the two catalog-backed rules
    // report `<skipped/>` testcases, informational only.
    assert_eq!(
        stdout.matches("<skipped").count(),
        2,
        "the catalog rules skip without a catalog: {stdout}"
    );
    // The listing inside a failure is the shared `file[:line[:col]] rule
    // message` anchor — a `.sql` finding carries its call-site position.
    assert!(
        stdout.contains("models/utilities/pos_prep/pos_cat.sql:4:18 direct_join_to_source"),
        "finding bodies: {stdout}"
    );
}

#[test]
fn check_format_codeclimate_is_an_issue_array() {
    let m = fixture_manifest();
    let out = check_on(&m, &["--lint", "--format", "codeclimate"]);
    let v: serde_json::Value = serde_json::from_str(&out_text(&out)).expect("codeclimate parses");
    let issues = v.as_array().expect("a top-level array");
    assert_eq!(issues.len(), counted_findings(&m));
    let first = &issues[0];
    assert_eq!(first["type"], "issue");
    assert!(first["check_name"].is_string());
    assert!(first["location"]["path"].is_string());
    assert_eq!(first["severity"], "minor");
    // An error-severity finding maps to "major".
    let gating = issues
        .iter()
        .find(|i| i["check_name"] == "missing_primary_key_tests")
        .expect("the fixture's one error-default rule fires");
    assert_eq!(gating["severity"], "major");
    let fp = first["fingerprint"].as_str().unwrap();
    assert_eq!(fp.len(), 16, "a 16-hex fingerprint: {fp}");
    assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
}

// ---- --report: file destinations -------------------------------------------

#[test]
fn report_writes_multiple_formats_matching_stdout() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    let json_path = tmp.path().join("r.json");
    let md_path = tmp.path().join("nested").join("r.md");
    let out = check_on(
        &m,
        &[
            "--lint",
            "--format",
            "text",
            "--report",
            &format!("json:{}", json_path.display()),
            "--report",
            &format!("markdown:{}", md_path.display()),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "the gates still gate");
    // The stdout format is text; the files carry their own formats — the
    // exact bytes `--format` would print.
    let stdout = out_text(&out);
    assert!(stdout.contains("scope: root project only"));
    let file_json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
    assert_eq!(file_json["schema"], "dbtl.report/v1");
    // Parents are created on demand.
    let md = std::fs::read_to_string(&md_path).unwrap();
    assert!(md.starts_with("<!-- dbtl:report -->"));
    // Byte-equality with the same format on stdout.
    let out2 = check_on(&m, &["--lint", "--format", "json"]);
    assert_eq!(
        out_text(&out2),
        std::fs::read_to_string(&json_path).unwrap(),
        "--report json matches --format json stdout"
    );
}

#[test]
fn report_overwrites_existing_files() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    let path = tmp.path().join("r.json");
    std::fs::write(&path, "stale").unwrap();
    let out = check_on(&m, &["--report", &format!("json:{}", path.display())]);
    assert!(out.status.success());
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["schema"], "dbtl.report/v1", "the stale bytes are gone");
}

#[test]
fn report_rejects_bad_specs_as_usage_errors() {
    let m = fixture_manifest();
    for spec in ["bogus:x", "json", "json:"] {
        let out = check_on(&m, &["--report", spec]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{spec:?} is a usage error, got {}",
            stderr(&out)
        );
    }
}

#[test]
fn quiet_suppresses_stdout_but_still_writes_reports() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    let path = tmp.path().join("r.json");
    let out = check_on(
        &m,
        &[
            "--lint",
            "--quiet",
            "--report",
            &format!("json:{}", path.display()),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "the gate verdict survives");
    assert!(out.stdout.is_empty(), "stdout is silent under --quiet");
    assert!(path.is_file(), "--report still writes under --quiet");
}

// ---- --step-summary / --path-prefix ----------------------------------------

#[test]
fn step_summary_appends_markdown_to_the_env_file() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    let summary = tmp.path().join("summary.md");
    let run_once = || {
        dbtl_cmd()
            .args(["check", "--manifest", m.to_str().unwrap(), "--step-summary"])
            .env("GITHUB_STEP_SUMMARY", &summary)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run")
    };
    assert!(run_once().status.success());
    let once = std::fs::read_to_string(&summary).unwrap();
    assert!(
        once.starts_with("<!-- dbtl:report -->"),
        "markdown appended"
    );
    // Appends, not truncates: a second run doubles the document.
    assert!(run_once().status.success());
    let twice = std::fs::read_to_string(&summary).unwrap();
    assert_eq!(twice.len(), once.len() * 2, "the summary appends");
    // Without the env var nothing is written anywhere (removed explicitly —
    // a CI-inherited variable must not leak into the assertion).
    let out = dbtl_cmd()
        .args(["check", "--manifest", m.to_str().unwrap(), "--step-summary"])
        .env_remove("GITHUB_STEP_SUMMARY")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run");
    assert!(out.status.success());
}

#[test]
fn path_prefix_without_a_path_format_is_a_usage_error() {
    let m = fixture_manifest();
    let out = check_on(&m, &["--path-prefix", "sub"]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    // …and a path-carrying --report format satisfies the requirement even
    // when the stdout format stays text.
    let tmp = tempfile::tempdir().unwrap();
    let m2 = manifest_in(tmp.path());
    let sarif = tmp.path().join("r.sarif");
    let out = check_on(
        &m2,
        &[
            "--lint",
            "--path-prefix",
            "sub/dir",
            "--report",
            &format!("sarif:{}", sarif.display()),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "lint still gates");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sarif).unwrap()).unwrap();
    let uri = doc["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]
        ["uri"]
        .as_str()
        .unwrap();
    assert!(
        uri.starts_with("sub/dir/"),
        "the prefix lands on sarif uris: {uri}"
    );
}

#[test]
fn path_prefix_does_not_reach_concise_lines() {
    // A path-carrying --report makes --path-prefix valid, but the prefix is
    // a github/sarif/codeclimate concern — concise lines (stdout AND the
    // --report concise: file) keep the project-relative path, exactly like
    // text's findings.
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    let concise = tmp.path().join("r.txt");
    let sarif = tmp.path().join("r.sarif");
    let out = check_on(
        &m,
        &[
            "--lint",
            "--format",
            "concise",
            "--path-prefix",
            "sub/dir",
            "--report",
            &format!("concise:{}", concise.display()),
            "--report",
            &format!("sarif:{}", sarif.display()),
        ],
    );
    assert_eq!(out.status.code(), Some(1), "lint still gates");
    let stdout = out_text(&out);
    let file = std::fs::read_to_string(&concise).unwrap();
    for (label, body) in [("stdout", &stdout), ("--report concise:", &file)] {
        let line = body
            .lines()
            .find(|l| l.contains("[select_star]") || l.contains("[root_models]"))
            .unwrap_or_else(|| panic!("{label} carries a finding line: {body}"));
        assert!(
            !line.starts_with("sub/dir"),
            "{label} stays project-relative: {line}"
        );
    }
}

// ---- check --docs-dir -------------------------------------------------------

/// Generate docs into `dir` via the real `docs` subcommand.
fn gen_docs(manifest: &Path, dir: &Path) {
    let out = run(&[
        "docs",
        "--manifest",
        manifest.to_str().unwrap(),
        "--out",
        dir.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "docs generated: {}", stderr(&out));
}

#[test]
fn docs_dir_gate_passes_on_fresh_docs() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    let docs = tmp.path().join("docs");
    gen_docs(&m, &docs);
    let out = check_on(&m, &["--docs-dir", docs.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "up-to-date docs pass: {}",
        stderr(&out)
    );
    assert!(
        out_text(&out).contains("PASS docs drift:"),
        "the gate reports: {}",
        out_text(&out)
    );
}

#[test]
fn docs_dir_gate_reports_the_same_differences_as_docs_check() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    let docs = tmp.path().join("docs");
    gen_docs(&m, &docs);
    // Diverge three ways: corrupt the README, delete one node page, and
    // leave a generated-looking page no current run would emit (stale).
    let page = std::fs::read_dir(docs.join("models"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let page_name = format!(
        "missing: models/{}",
        page.file_name().unwrap().to_string_lossy()
    );
    std::fs::remove_file(&page).unwrap();
    std::fs::write(docs.join("README.md"), "# tampered\n").unwrap();
    std::fs::write(
        docs.join("models").join("zz_removed.md"),
        format!("{}# ghost page\n", dbtl::GENERATED_MARKER),
    )
    .unwrap();

    // `docs --check`'s divergence listing…
    let out = run(&[
        "docs",
        "--manifest",
        m.to_str().unwrap(),
        "--out",
        docs.to_str().unwrap(),
        "--check",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let docs_stdout = out_text(&out);
    assert!(docs_stdout.contains("changed: README.md"));
    assert!(docs_stdout.contains(&page_name));
    assert!(
        docs_stdout.contains("stale: models/zz_removed.md"),
        "the stale page lists, got: {docs_stdout}"
    );

    // …is the same divergence the gate reports — one comparison, two fronts.
    let out = check_on(
        &m,
        &["--docs-dir", docs.to_str().unwrap(), "--format", "json"],
    );
    assert_eq!(out.status.code(), Some(1), "the drift gates");
    let v: serde_json::Value = serde_json::from_str(&out_text(&out)).unwrap();
    let gate = v["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == "docs_drift")
        .expect("the docs_drift gate");
    assert_eq!(gate["status"], "fail");
    let drift = &v["docs_drift"];
    assert_eq!(drift["changed"].as_array().unwrap()[0], "README.md");
    let missing: Vec<String> = drift["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        missing,
        vec![page_name.strip_prefix("missing: ").unwrap()],
        "the same set docs --check listed"
    );
    assert_eq!(
        drift["stale"].as_array().unwrap(),
        &vec![serde_json::json!("models/zz_removed.md")],
        "the stale page fails the gate too"
    );
}

// ---- impact --report ---------------------------------------------------------

#[test]
fn impact_report_writes_the_same_bytes_as_stdout() {
    let m = fixture_manifest();
    let tmp = tempfile::tempdir().unwrap();
    let md_path = tmp.path().join("impact.md");
    let json_path = tmp.path().join("impact.json");
    let out = run(&[
        "impact",
        "--manifest",
        m.to_str().unwrap(),
        "--diff",
        m.to_str().unwrap(),
        "--report",
        &format!("markdown:{}", md_path.display()),
        "--report",
        &format!("json:{}", json_path.display()),
    ]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert_eq!(
        out_text(&out),
        std::fs::read_to_string(&md_path).unwrap(),
        "the markdown file is the stdout document"
    );
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
    assert!(
        v.get("pr").is_some() && v.get("baseline").is_some(),
        "the json file is the DiffReport"
    );
}

#[test]
fn impact_report_rejects_non_impact_formats() {
    let m = fixture_manifest();
    let out = run(&[
        "impact",
        "--manifest",
        m.to_str().unwrap(),
        "--diff",
        m.to_str().unwrap(),
        "--report",
        "sarif:/tmp/x.sarif",
    ]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
}

// ---- lint shares the formats -------------------------------------------------

#[test]
fn lint_accepts_every_format_and_report_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    for fmt in [
        "text",
        "concise",
        "markdown",
        "json",
        "github",
        "sarif",
        "junit",
        "codeclimate",
    ] {
        let out = run(&["lint", "--manifest", m.to_str().unwrap(), "--format", fmt]);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{fmt}: lint gates, stderr: {}",
            stderr(&out)
        );
        assert!(!out.stdout.is_empty(), "{fmt}: a report rendered on stdout");
    }
    // The output flags validate before loading — a bad spec on a missing
    // source is still a usage error, not an input error.
    let out = run(&["lint", "--report", "bogus:x"]);
    assert_eq!(out.status.code(), Some(2));
}

// ---- concise: one greppable line per finding ---------------------------------

/// The committed sample project — source-mode runs carry real yml
/// positions the manifest fixture can't produce.
fn sample_project() -> PathBuf {
    fixture_path("sample_project")
}

#[test]
fn concise_is_one_greppable_line_per_finding() {
    // `file:line:col: severity[rule] message` — the positions come from
    // the finding, so a `.sql` finding with no recorded span anchors at
    // its file and never invents a `:1`.
    let proj = sample_project();
    let out = run(&[
        "lint",
        "--source",
        proj.to_str().unwrap(),
        "--rules",
        "missing_primary_key_tests",
        "--format",
        "concise",
    ]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let stdout = out_text(&out);
    assert!(
        stdout
            .contains("models/marts/schema.yml:4:11: error[missing_primary_key_tests] fct_orders"),
        "a positioned anchor: {stdout}"
    );
    assert!(
        stdout.contains("models/marts/agg_country_orders.sql: error[missing_primary_key_tests]"),
        "a file-only anchor: {stdout}"
    );
    assert!(!stdout.contains(".sql:1"), "no invented :1: {stdout}");
    // …and the same summary the text report ends with.
    assert!(stdout.contains("Found 9 errors and 0 warnings in 5 files"));
    assert!(stdout.contains("1 rule reported findings"));
}

#[test]
fn concise_lines_match_the_json_finding_set() {
    // The line count is the counted-finding count — nothing extra lists,
    // nothing drops (modulo the blank separator + summary tail).
    let m = fixture_manifest();
    let out = run(&[
        "lint",
        "--manifest",
        m.to_str().unwrap(),
        "--format",
        "concise",
    ]);
    let stdout = out_text(&out);
    let concise_count = stdout
        .lines()
        .filter(|l| l.contains("[") && !l.starts_with("Found") && !l.starts_with("No findings"))
        .count();
    assert_eq!(concise_count, counted_findings(&m));
    // The machine reports never adopt the human formats' anchors — the
    // json gate messages keep their `violation(s)` wording.
    let out = lint_on_json(&m);
    assert!(out.contains("violation(s)"), "machine wording: {out}");
}

/// `lint --format json`'s raw stdout — the machine-report oracle.
fn lint_on_json(m: &Path) -> String {
    let out = run(&[
        "lint",
        "--manifest",
        m.to_str().unwrap(),
        "--format",
        "json",
    ]);
    out_text(&out)
}

// ---- --color + ANSI discipline -------------------------------------------------

/// The escape byte ANSI styling starts with — every "no color" assertion
/// searches stdout/the report files for it.
const ESC: char = '\u{1b}';

#[test]
fn color_flag_and_env_control_the_ansi() {
    let m = fixture_manifest();
    // `--color always` styles the human formats — even through a pipe.
    let out = run(&[
        "lint",
        "--manifest",
        m.to_str().unwrap(),
        "--color",
        "always",
    ]);
    assert!(
        out_text(&out).contains(ESC),
        "always forces styling: {}",
        out_text(&out)
    );
    // `--color never` stays plain — and it beats the environment's force.
    let out = dbtl_cmd()
        .args([
            "lint",
            "--manifest",
            m.to_str().unwrap(),
            "--color",
            "never",
        ])
        .env("CLICOLOR_FORCE", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run");
    assert!(
        !out_text(&out).contains(ESC),
        "never wins over CLICOLOR_FORCE"
    );
    // Under `auto` the environment speaks: CLICOLOR_FORCE colors a piped
    // run, NO_COLOR uncolors it.
    let out = dbtl_cmd()
        .args(["lint", "--manifest", m.to_str().unwrap()])
        .env("CLICOLOR_FORCE", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run");
    assert!(
        out_text(&out).contains(ESC),
        "auto honors CLICOLOR_FORCE: {}",
        out_text(&out)
    );
    let out = dbtl_cmd()
        .args(["lint", "--manifest", m.to_str().unwrap()])
        .env("CLICOLOR_FORCE", "1")
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run");
    assert!(
        !out_text(&out).contains(ESC),
        "NO_COLOR beats the force under auto"
    );
    // A bogus value is a usage error — the flag is closed-world.
    let out = run(&[
        "lint",
        "--manifest",
        m.to_str().unwrap(),
        "--color",
        "bogus",
    ]);
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
}

#[test]
fn ansi_never_reaches_machine_formats_or_report_files() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest_in(tmp.path());
    let txt = tmp.path().join("r.txt");
    let json = tmp.path().join("r.json");
    // `--color always` is a stdout knob — the report files stay plain.
    let out = run(&[
        "lint",
        "--manifest",
        m.to_str().unwrap(),
        "--color",
        "always",
        "--report",
        &format!("text:{}", txt.display()),
        "--report",
        &format!("json:{}", json.display()),
    ]);
    assert!(out_text(&out).contains(ESC), "stdout is styled");
    for file in [&txt, &json] {
        let body = std::fs::read_to_string(file).unwrap();
        assert!(!body.contains(ESC), "{} carries no ANSI", file.display());
    }
    // …and so does every machine format on stdout itself.
    for fmt in [
        "json",
        "sarif",
        "junit",
        "codeclimate",
        "markdown",
        "github",
    ] {
        let out = run(&[
            "lint",
            "--manifest",
            m.to_str().unwrap(),
            "--format",
            fmt,
            "--color",
            "always",
        ]);
        assert!(
            !out_text(&out).contains(ESC),
            "{fmt} stdout carries no ANSI"
        );
    }
}

// ---- the annotated excerpt, end to end -----------------------------------------

#[test]
fn text_report_underlines_the_declaring_source_line() {
    // `missing_primary_key_tests` on fct_orders pins the `- name:` entry
    // at schema.yml:4:11 — the excerpt prints that line and underlines
    // exactly its value span.
    let proj = sample_project();
    let out = run(&[
        "lint",
        "--source",
        proj.to_str().unwrap(),
        "--rules",
        "missing_primary_key_tests",
    ]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let stdout = out_text(&out);
    for needle in [
        "error[missing_primary_key_tests]: fct_orders",
        "--> models/marts/schema.yml:4:11",
        "4 |   - name: fct_orders",
        "^^^^^^^^^^ declared here",
        "= help:",
        "= docs: https://hytfjwr.github.io/dbtl/reference/lint-rules/#missing_primary_key_tests",
    ] {
        assert!(stdout.contains(needle), "missing {needle:?} in:\n{stdout}");
    }
}

// ---- broken pipes ---------------------------------------------------------------

#[test]
fn lint_survives_a_broken_stdout_pipe() {
    // `dbtl lint | head` — a reader closing mid-report is a normal end of
    // output, not a panic. (The child may legitimately die by SIGPIPE like
    // any unix filter; it must never crash-report.)
    let m = fixture_manifest();
    let mut child = dbtl_cmd()
        .args([
            "lint",
            "--manifest",
            m.to_str().unwrap(),
            "--color",
            "always",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .spawn()
        .expect("spawn dbtl");
    let mut stdout = child.stdout.take().unwrap();
    use std::io::Read;
    let _ = stdout.read(&mut [0u8; 64]); // consume a bit, then close
    drop(stdout);
    let out = child.wait_with_output().expect("wait");
    assert_ne!(out.status.code(), Some(101), "no panic on a closed pipe");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!err.contains("panic"), "no panic on stderr either: {err}");
}
