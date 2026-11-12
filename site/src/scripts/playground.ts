// The playground: type a dbt selector, see which nodes of a sample project it picks, the
// lineage around them, and the lint findings on them. Everything runs in the page.
import { jaffle_finance, sample } from "../data/graph.json";
import lintJaffle from "../data/lint-jaffle_finance.json";
import lintSample from "../data/lint-sample.json";
import { esc } from "./html.ts";
import { mount } from "./lineage-view.ts";
import {
  command,
  drawn,
  EXAMPLES,
  lineageNote,
  lineageStyle,
  lintHtml,
  matchedHtml,
  matchedTitle,
  PROJECTS,
  run,
} from "./playground-view.ts";
import { toProject, type LintData, type Project, type RawSample } from "./samples.ts";

const SAMPLES: Record<string, { project: Project; lint: LintData }> = {
  sample: { project: toProject(sample as unknown as RawSample), lint: lintSample as LintData },
  jaffle_finance: {
    project: toProject(jaffle_finance as unknown as RawSample),
    lint: lintJaffle as LintData,
  },
};

const $ = <T extends HTMLElement>(sel: string) => document.querySelector<T>(sel);

const input = $<HTMLInputElement>("#pg-sel");
const host = $("#pg-lin");
const switcher = $("#pg-proj");
const chips = $("#pg-eg");
const tabs = $(".pg-tabs");

if (input && host && switcher && chips && tabs) {
  let name = PROJECTS[0]!;
  let spec = input.value;
  let previous: Set<string> | null = run(SAMPLES[name]!.project, spec).hit;
  const note =
    host.querySelector<HTMLElement>(".pg-note") ??
    Object.assign(document.createElement("p"), { className: "pg-note" });
  const view = mount(host);

  function render(reveal: { speed?: number } | null = null): void {
    const { project, lint } = SAMPLES[name]!;
    const outcome = run(project, spec);
    const was = previous;
    $("#pg-err")!.textContent = outcome.error;

    const message = lineageNote(project, outcome, spec);
    if (message === null) {
      note.remove();
      view.stage.hidden = false;
      view.render(drawn(project, outcome.hit), lineageStyle(outcome.hit, was), { reveal });
    } else {
      view.stage.hidden = true;
      note.textContent = message;
      host!.appendChild(note);
    }

    $("#pg-match-title")!.innerHTML = matchedTitle(project, outcome);
    $("#pg-lin-title")!.innerHTML = `lineage: <b>${esc(project.name)}</b>`;
    $("#pc-lin")!.textContent = String(outcome.hit.size);
    $("#pg-match")!.innerHTML = matchedHtml(project, outcome, spec, was);
    previous = outcome.error ? was : outcome.hit;

    const linted = lintHtml(lint, outcome.hit, outcome, spec);
    $("#pc-lint")!.textContent = String(linted.count);
    $("#pg-lint")!.innerHTML = linted.html;

    input!.style.width = `${Math.max(1, spec.length)}ch`;
    $("#pg-cmd")!.textContent = command(spec);
    chips!
      .querySelectorAll("button")
      .forEach((b) => b.setAttribute("aria-pressed", String(b.textContent === spec.trim())));
  }

  function load(next: string): void {
    name = next;
    spec = EXAMPLES[name]![0]!;
    previous = null;
    // Keep the project buttons (the pressed one may have focus); only the pressed state moves.
    switcher!
      .querySelectorAll("button")
      .forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.p === name)));
    chips!.innerHTML = EXAMPLES[name]!.map((e) => `<button type="button">${esc(e)}</button>`).join(
      "",
    );
    input!.value = spec;
    render({ speed: 0.35 });
  }

  switcher.addEventListener("click", (ev) => {
    const b = (ev.target as Element).closest("button");
    if (b?.dataset.p && b.dataset.p !== name) load(b.dataset.p);
  });
  chips.addEventListener("click", (ev) => {
    const b = (ev.target as Element).closest("button");
    if (!b) return;
    spec = b.textContent ?? "";
    input.value = spec;
    render();
  });
  input.addEventListener("input", () => {
    spec = input.value;
    render();
  });

  // Output tabs: Lineage and Lint findings.
  const buttons = [...tabs.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
  const pick = (tab: HTMLButtonElement, focus: boolean) => {
    for (const b of buttons) {
      b.setAttribute("aria-selected", String(b === tab));
      b.tabIndex = b === tab ? 0 : -1;
    }
    if (focus) tab.focus();
    $("#pp-lin")!.hidden = tab.id !== "pt-lin";
    $("#pp-lint")!.hidden = tab.id !== "pt-lint";
  };
  buttons.forEach((tab, i) => {
    tab.addEventListener("click", () => pick(tab, false));
    tab.addEventListener("keydown", (ev) => {
      const n = buttons.length;
      const to =
        ev.key === "ArrowRight"
          ? (i + 1) % n
          : ev.key === "ArrowLeft"
            ? (i - 1 + n) % n
            : ev.key === "Home"
              ? 0
              : ev.key === "End"
                ? n - 1
                : -1;
      if (to < 0) return;
      ev.preventDefault();
      pick(buttons[to]!, true);
    });
  });
}
