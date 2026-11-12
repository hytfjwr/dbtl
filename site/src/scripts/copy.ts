// Copy buttons on code blocks: <figure class="code"><button class="copy">…</button><pre>…</pre></figure>

function fallbackCopy(text: string): void {
  const area = document.createElement("textarea");
  area.value = text;
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.select();
  try {
    document.execCommand("copy");
  } finally {
    area.remove();
  }
}

async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    fallbackCopy(text);
  }
}

document.addEventListener("click", (event) => {
  const target = event.target;
  if (!(target instanceof Element)) return;
  const button = target.closest<HTMLButtonElement>("button.copy");
  const pre = button?.parentElement?.querySelector("pre");
  if (!button || !pre) return;
  void copyText(pre.textContent ?? "").then(() => {
    button.dataset.state = "done";
    button.textContent = "copied";
    setTimeout(() => {
      delete button.dataset.state;
      button.textContent = "copy";
    }, 1400);
  });
});
