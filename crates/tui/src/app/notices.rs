use super::*;

impl App {
    /// Record a transient notice (the toast text) at the default `Info` level.
    /// Last write wins: a later writer in the same loop iteration (e.g.
    /// `run_effect` reporting a failure) deliberately replaces the reducer's
    /// optimistic intent.
    pub fn set_notice(&mut self, text: impl Into<String>) {
        self.set_notice_level(NoticeLevel::Info, text);
    }

    /// `set_notice` with an explicit level: warnings carry a longer on-screen
    /// TTL and bump the `!N` unread count once the loop logs them. Same
    /// last-write-wins rule — an effect failure still replaces the optimistic
    /// intent notice outright.
    pub fn set_notice_level(&mut self, level: NoticeLevel, text: impl Into<String>) {
        self.notice = Some((level, text.into()));
    }

    /// Append to a PENDING notice instead of replacing it (`" | "` joined) —
    /// for startup-time writers that must not clobber a warning already
    /// queued by `with_source` (e.g. a malformed `.dbtl.yml` notice followed
    /// by the source-mode hint: both show in the one toast). The pending
    /// notice's level is preserved (an info append never downgrades a queued
    /// warning).
    pub fn append_notice(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.notice = Some(match self.notice.take() {
            Some((level, prev)) => (level, format!("{prev} | {text}")),
            None => (NoticeLevel::Info, text),
        });
    }

    /// Drain the pending notice's text (one-shot). The event loop calls
    /// [`take_notice_leveled`](App::take_notice_leveled) so the level drives
    /// the toast TTL and the log entry; this text-only view stays for tests
    /// and callers that don't care about severity.
    pub fn take_notice(&mut self) -> Option<String> {
        self.take_notice_leveled().map(|(_, t)| t)
    }

    /// Drain the pending notice WITH its level — the event loop's per-frame
    /// drain (it stamps the wall-clock and pushes the toast + the log entry,
    /// keeping time out of the reducer).
    pub fn take_notice_leveled(&mut self) -> Option<(NoticeLevel, String)> {
        self.notice.take()
    }

    /// Append `entry` to the notification log (newest last; the oldest drops
    /// at [`NOTIFICATION_CAP`](App::NOTIFICATION_CAP)). Warning entries count
    /// toward the unread `!N` chip until the centre is opened. The `stamp` is
    /// the caller's — the event loop / CLI format the wall-clock; `App` keeps
    /// no clock.
    pub fn record_notification(&mut self, entry: NotificationEntry) {
        if self.notifications.len() >= Self::NOTIFICATION_CAP {
            self.notifications.pop_front();
            // The evicted front shifts every index down — the read watermark
            // shifts with it, whether that entry was read or unread.
            self.unread_from = self.unread_from.saturating_sub(1);
        }
        self.notifications.push_back(entry);
    }

    /// Record every startup load warning in the notification log (warning
    /// level, unread) — `build_app` feeds it the `ResolvedSource.warnings`
    /// vec after printing them to stderr as before. `stamp` is the caller's
    /// wall-clock reading, taken ONCE per batch: stamping per line would
    /// spawn one `date` per warning (and could split a batch across a
    /// second boundary anyway).
    pub fn record_startup_warnings(&mut self, warnings: &[String], stamp: &str) {
        for w in warnings {
            self.record_notification(NotificationEntry {
                stamp: stamp.to_string(),
                level: NoticeLevel::Warning,
                text: w.clone(),
            });
        }
    }

    /// The number of unread WARNING notifications (drives the `!N` chip) —
    /// counted from the log itself each call, so the chip can never count a
    /// warning the cap has already evicted.
    pub fn unread_warnings(&self) -> usize {
        self.notifications
            .iter()
            .skip(self.unread_from)
            .filter(|n| n.level == NoticeLevel::Warning)
            .count()
    }

    /// The notification log's rows newest-first, snapshotted for the
    /// `Ctrl-n` centre (the render layer needs no `Dag`, like every modal).
    pub fn notifications_snapshot(&self) -> NotificationsView {
        NotificationsView {
            rows: self.notifications.iter().rev().cloned().collect(),
            scroll: 0,
        }
    }

    /// Mark every logged entry read — opening the notification centre is the
    /// read gesture that zeroes the `!N` chip. Later entries stay unread.
    pub fn mark_notifications_read(&mut self) {
        self.unread_from = self.notifications.len();
    }
}
