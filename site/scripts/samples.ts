// The sample projects the docs site stages and offers. This is the ONLY
// list — the playground switcher (src/scripts/pg-render.ts), the staged
// public/samples/ copy (build-engine.mjs) and the home-demo graph
// (prebuild.mjs) all read from it, so they cannot drift apart.
//
// `manifest` is the repo-relative path of the project's manifest.json.
// `ymlFrom` is walked for `*.yml|*.yaml` (deterministic order) because lint
// findings cite project-relative paths like `models/marts/schema.yml`; those
// texts land in files.json (.sql excerpts come from the manifest's own
// raw_code — never copied).
export const SAMPLES = [
  {
    name: "jaffle_finance",
    manifest: "tests/fixtures/manifest.json",
    ymlFrom: null,
  },
  {
    name: "sample",
    manifest: "tests/fixtures/sample_manifest.json",
    ymlFrom: "tests/fixtures/sample_project",
  },
] as const;

export type SampleName = (typeof SAMPLES)[number]["name"];

/** The project names the playground switcher offers, in order. */
export const SAMPLE_NAMES: readonly SampleName[] = SAMPLES.map((s) => s.name);
