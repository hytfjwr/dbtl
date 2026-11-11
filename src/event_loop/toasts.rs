use super::*;

/// One live toast in the top-right stack: the notice text, its
/// severity (drives the TTL and the border colour), and the arrival stamp the
/// loop adds — the reducer stays clock-free.
pub(super) struct ToastItem {
    pub(super) level: dbtl::NoticeLevel,
    pub(super) text: String,
    pub(super) at: Instant,
}

/// The visible toast stack: newest FIRST (`items[0]` renders on top), capped
/// at [`ToastStack::MAX`]. [`ToastStack::push`] drops the oldest past the cap;
/// [`ToastStack::retain_fresh`] drops anything past its per-level TTL.
#[derive(Default)]
pub(super) struct ToastStack {
    items: Vec<ToastItem>,
}

impl ToastStack {
    /// At most 3 toasts on screen at once.
    const MAX: usize = 3;

    /// Push `item` to the TOP (newest first), evicting the oldest past `MAX`.
    pub(super) fn push(&mut self, item: ToastItem) {
        self.items.insert(0, item);
        self.items.truncate(Self::MAX);
    }

    /// Drop every toast past its per-level TTL at `now`; true when anything
    /// expired (the caller marks the frame dirty). Takes `now` so expiry
    /// compares against a fixed instant — `saturating_duration_since` treats
    /// a future `at` as fresh instead of panicking.
    pub(super) fn retain_fresh(&mut self, now: Instant) -> bool {
        let before = self.items.len();
        self.items
            .retain(|t| now.saturating_duration_since(t.at) < toast_ttl(t.level));
        self.items.len() != before
    }

    /// The draw-seam view: borrows of the live entries, newest first.
    pub(super) fn lines(&self) -> Vec<dbtl::ToastLine<'_>> {
        self.items
            .iter()
            .map(|t| dbtl::ToastLine {
                level: t.level,
                text: t.text.as_str(),
            })
            .collect()
    }
}

/// Per-level on-screen lifetime: info ~2.5s (the legacy toast TTL), warning
/// ~5s so failures stay readable. Owned here, not in `NoticeLevel` — the
/// TTL is a UI policy, the level itself is UI-free.
pub(super) fn toast_ttl(level: dbtl::NoticeLevel) -> Duration {
    match level {
        dbtl::NoticeLevel::Info => Duration::from_millis(2500),
        dbtl::NoticeLevel::Warning => Duration::from_millis(5000),
    }
}

/// Local wall-clock `HH:MM:SS` for notification stamps. `std` has no local
/// time API and adding `time`/`chrono` just for a wall clock isn't worth a
/// new dependency — so the platform `date` answers "what time is
/// it here". The fallback when `date` is absent is UTC, NOT local time — the
/// stamp degrades to a fixed-offset clock rather than guessing the zone.
/// `pub(crate)` because `cli::build_app` stamps startup warnings through the
/// same clock.
pub(crate) fn now_hhmmss() -> String {
    if let Ok(out) = Command::new("date").arg("+%H:%M:%S").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            let s = s.trim();
            if s.len() == 8 {
                return s.to_string();
            }
        }
    }
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600 % 24,
        secs / 60 % 60,
        secs % 60
    )
}

/// The reload/`dbt parse` toast tail for a non-empty load-warnings vec:
/// every warning joined (`"1 warning: <w>"` / `"N warnings: <w1>; <w2>"`) —
/// quoting only the first would hide the rest of what the manifest carries.
pub(super) fn warnings_tail(warnings: &[String]) -> String {
    format!(
        "{} warning{}: {}",
        warnings.len(),
        if warnings.len() == 1 { "" } else { "s" },
        warnings.join("; ")
    )
}
