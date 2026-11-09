# dbtl 改善プロジェクト — 進捗

Stacked PRs (base → head). すべて `cargo test --workspace` + `clippy -D warnings` + `fmt` 済み。

## Shipped

| PR | Branch | 内容 | Status |
|----|--------|------|--------|
| #73 | refactor/split-main | main.rs 分割 (cli/terminal/event_loop/watch/themes) | open |
| #74 | feat/docs-clean-check | `dbtl docs --clean/--check` + generated marker | open |
| #75 | feat/macro-diff | macro 変更検知 (`macro:<name>`) | open |
| #76 | feat/impact-check-cli | `pr_impact`→core + `dbtl impact`/`dbtl check` | open |
| #77 | feat/run-results | run_results.json + RunStatus/RunTime lenses + weighted critical path | open |
| #78 | feat/selectors | dbt selector 構文 (`!` prefix, `--select`) | open |
| #79 | feat/package-filter | `package_name` + `--project-only` + `Dag::filtered` + catalog.json | open |
| #80 | feat/semantic-governance | semantic models/metrics/saved queries + unit_tests + access/group/contract/version | open |
| #81 | feat/dirty-redraw | dirty-flag idle redraw | open |

## In progress

| PR | Branch | 内容 | Status |
|----|--------|------|--------|
| PR10 | feat/path-and-fulltext | B.7: 2点間経路表示 (`p`) + 全文検索 (sql/desc/columns) | 🚧 |
| PR11 | feat/state-persist | B.5: `$XDG_STATE_HOME/dbtl/` 状態保存 + `~/.config/dbtl/config.yml` | ⬜ |
| PR12 | feat/y-prefix | B.6: `y`-prefix 2打鍵 yank + which-key ヒント | ⬜ |
| PR13 | feat/benches | C.10 残: criterion benches + synthetic manifest gen | ⬜ |

## Done items (from proposal)

- [x] A.1 `dbtl impact`/`dbtl check` (PR #76)
- [x] A.2 run_results.json + Run lenses (PR #77)
- [x] A.3 selector 構文 (PR #78)
- [x] A.4 macro diff (PR #75)
- [ ] B.5 状態保存 + config
- [ ] B.6 キー空間整理 (y-prefix)
- [ ] B.7 2点間経路 + 全文検索
- [x] B.8 新リソース型 + governance (PR #80)
- [x] B.9 package 区別 + catalog.json (PR #79)
- [ ] C.10 計測 (dirty-flag は #81 済み, benches 未)
- [x] D.11 main.rs 分割 (PR #73)
- [x] D.12 docs --clean/--check (PR #74)
