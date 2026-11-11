use super::*;

// ---- notification log, unread chip, Ctrl-n centre ---------------

#[test]
fn leveled_notice_drains_level_and_keeps_text_compat() {
    let mut a = app();
    a.set_notice_level(NoticeLevel::Warning, "Reload failed");
    let (level, text) = a.take_notice_leveled().expect("leveled notice");
    assert_eq!(level, NoticeLevel::Warning);
    assert_eq!(text, "Reload failed");
    // The text-only drain stays for callers/tests that don't care about level.
    a.set_notice("Copied unique_id");
    let (level, _) = a.take_notice_leveled().expect("info defaults");
    assert_eq!(level, NoticeLevel::Info);
    a.set_notice("Copied unique_id");
    assert_eq!(a.take_notice().as_deref(), Some("Copied unique_id"));
    // append joins text but never downgrades a queued warning's level.
    a.set_notice_level(NoticeLevel::Warning, "bad .dbtl.yml");
    a.append_notice("hint: source mode");
    let (level, text) = a.take_notice_leveled().expect("appended");
    assert_eq!(level, NoticeLevel::Warning);
    assert_eq!(text, "bad .dbtl.yml | hint: source mode");
}

#[test]
fn notification_log_caps_at_100_and_evicts_oldest() {
    let mut a = app();
    for i in 0..App::NOTIFICATION_CAP + 5 {
        a.record_notification(entry(NoticeLevel::Info, &format!("n{i}")));
    }
    let view = a.notifications_snapshot();
    assert_eq!(view.rows.len(), App::NOTIFICATION_CAP, "log capped at 100");
    assert_eq!(view.rows[0].text, "n104", "newest first");
    assert_eq!(
        view.rows.last().unwrap().text,
        "n5",
        "the five oldest evicted"
    );
}

#[test]
fn unread_chip_counts_warnings_only_and_open_clears_it() {
    let mut a = app();
    a.record_notification(entry(NoticeLevel::Warning, "w1"));
    a.record_notification(entry(NoticeLevel::Info, "i1"));
    a.record_notification(entry(NoticeLevel::Warning, "w2"));
    assert_eq!(a.unread_warnings(), 2, "only warnings count");
    apply_action(&mut a, Action::NotificationsOpen);
    assert!(
        matches!(a.mode, Mode::Notifications(_)),
        "Ctrl-n opens the centre"
    );
    assert_eq!(a.unread_warnings(), 0, "opening is the read gesture");
    // Warnings arriving after the open count again.
    a.record_notification(entry(NoticeLevel::Warning, "w3"));
    assert_eq!(a.unread_warnings(), 1);
}

#[test]
fn notification_center_snapshots_newest_first_and_scrolls() {
    let mut a = app();
    a.record_notification(entry(NoticeLevel::Info, "first"));
    a.record_notification(entry(NoticeLevel::Warning, "second"));
    apply_action(&mut a, Action::NotificationsOpen);
    let scroll = |m: &Mode| match m {
        Mode::Notifications(nv) => {
            assert_eq!(nv.rows[0].text, "second", "newest row on top");
            assert_eq!(nv.rows[1].text, "first");
            nv.scroll
        }
        other => panic!("expected Notifications mode, got {other:?}"),
    };
    assert_eq!(scroll(&a.mode), 0);
    // The shared modal-scroll arms hit the centre's slot like any other modal.
    apply_action(&mut a, Action::DetailScrollPage(Direction::Down));
    assert_eq!(scroll(&a.mode), 10, "page = 10 lines");
    apply_action(&mut a, Action::DetailScroll(Direction::Down));
    assert_eq!(scroll(&a.mode), 11);
    apply_action(&mut a, Action::DetailScrollEnd);
    assert_eq!(scroll(&a.mode), usize::MAX, "End records MAX for clamp");
    apply_action(&mut a, Action::DetailScrollHome);
    assert_eq!(scroll(&a.mode), 0);
    apply_action(&mut a, Action::DetailClose);
    assert_eq!(a.mode, Mode::Selection, "close returns to selection");
}

#[test]
fn startup_warnings_join_the_log_as_unread_warnings() {
    let mut a = app();
    a.record_startup_warnings(
        &[
            "bad resource type".to_string(),
            "(scope) ambiguous root".to_string(),
        ],
        "01:02:03",
    );
    assert_eq!(a.unread_warnings(), 2);
    let view = a.notifications_snapshot();
    assert!(
        view.rows
            .iter()
            .all(|r| r.level == NoticeLevel::Warning && r.stamp == "01:02:03"),
        "every startup warning stamped + leveled: {:?}",
        view.rows
    );
    assert_eq!(view.rows[0].text, "(scope) ambiguous root", "newest first");
}

/// Effect-failure overwrite (the acceptance rule): a failed effect must not
/// leave its optimistic success notice behind — the pending channel is
/// last-write-wins, so the drain surfaces ONLY the failure (no success toast,
/// no success log entry).
#[test]
fn failed_effect_notice_overwrites_the_optimistic_success() {
    let mut a = app();
    a.set_notice("Copied unique_id");
    a.set_notice_level(NoticeLevel::Warning, "copy failed: clipboard error");
    let (level, text) = a.take_notice_leveled().expect("one drained notice");
    assert_eq!(level, NoticeLevel::Warning);
    assert_eq!(text, "copy failed: clipboard error");
    assert!(
        a.take_notice_leveled().is_none(),
        "the success notice is gone — nothing else drains"
    );
}

/// The `!N` chip counts warnings in the LOG, so an unread warning evicted by
/// the 100-cap stops counting — the chip can never point at a notification
/// the centre can no longer show.
#[test]
fn unread_chip_drops_warnings_the_cap_evicted() {
    let mut a = app();
    for i in 0..App::NOTIFICATION_CAP + 5 {
        a.record_notification(entry(NoticeLevel::Warning, &format!("w{i}")));
    }
    assert_eq!(
        a.unread_warnings(),
        App::NOTIFICATION_CAP,
        "all 100 logged warnings are unread — the 5 evicted ones are gone"
    );
}

/// Reading then evicting must not underflow the chip: after the centre marks
/// the log read, cap evictions take out READ entries — the unread count stays
/// exactly the warnings pushed since the open.
#[test]
fn unread_chip_survives_eviction_of_read_entries() {
    let mut a = app();
    for i in 0..App::NOTIFICATION_CAP {
        a.record_notification(entry(NoticeLevel::Warning, &format!("old{i}")));
    }
    apply_action(&mut a, Action::NotificationsOpen);
    assert_eq!(a.unread_warnings(), 0, "open marks all 100 read");
    for i in 0..5 {
        a.record_notification(entry(NoticeLevel::Warning, &format!("new{i}")));
    }
    assert_eq!(
        a.unread_warnings(),
        5,
        "only the post-open warnings count; evicted read entries can't underflow"
    );
    apply_action(&mut a, Action::NotificationsOpen);
    assert_eq!(a.unread_warnings(), 0, "re-read keeps the chip at zero");
}
