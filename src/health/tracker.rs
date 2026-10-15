//! Turns readings into incidents: one alert per incident, reminders for
//! critical ones while they last, a "resolved" message when they clear.
//!
//! Hysteresis: an incident opens after `consecutive_samples` bad readings
//! in a row (and after the rule's minimum duration), and resolves after as
//! many good readings in a row. `Unknown` readings count for neither.

use std::collections::BTreeMap;

use super::rules::{Reading, RuleId, Severity, Subject};

/// An open incident, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incident {
    pub subject: Subject,
    pub rule: RuleId,
    pub severity: Severity,
    /// When the condition began (Unix seconds).
    pub started_at: u64,
    /// When the incident was opened.
    pub opened_at: u64,
    /// When the last alert or reminder was delivered; `None` until the
    /// first one is.
    pub notified_at: Option<u64>,
    /// The latest description of the condition.
    pub detail: String,
}

/// Something to tell the admins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// A new incident, or one whose alert was never delivered.
    Opened(Incident),
    /// A critical incident still active after `renotify_interval`.
    Reminder(Incident),
    /// The incident cleared at `at`.
    Resolved {
        incident: Incident,
        at: u64,
        detail: Option<String>,
    },
}

impl Notice {
    pub fn incident(&self) -> &Incident {
        match self {
            Self::Opened(i) | Self::Reminder(i) => i,
            Self::Resolved { incident, .. } => incident,
        }
    }
}

/// A change the store must record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Upsert(Incident),
    Delete(Subject, RuleId),
}

#[derive(Debug, Clone, Default)]
struct Track {
    bad_streak: u32,
    good_streak: u32,
    /// The first bad reading of the current streak of bad conditions.
    first_bad_at: Option<u64>,
    incident: Option<Incident>,
}

/// Tuning, from the config.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub consecutive_samples: u32,
    /// 0: never remind.
    pub renotify_interval: u64,
}

#[derive(Debug, Default)]
pub struct Tracker {
    tracks: BTreeMap<(Subject, RuleId), Track>,
}

impl Tracker {
    /// A tracker that resumes the incidents open before a restart, so they
    /// are neither repeated nor lost.
    pub fn resume(open: impl IntoIterator<Item = Incident>) -> Self {
        let tracks = open
            .into_iter()
            .map(|incident| {
                let key = (incident.subject.clone(), incident.rule);
                let track = Track {
                    first_bad_at: Some(incident.started_at),
                    incident: Some(incident),
                    ..Track::default()
                };
                (key, track)
            })
            .collect();
        Self { tracks }
    }

    #[cfg(test)]
    pub fn open_incidents(&self) -> impl Iterator<Item = &Incident> {
        self.tracks.values().filter_map(|t| t.incident.as_ref())
    }

    /// Feeds one reading. Returns what to tell and what to store.
    pub fn observe(
        &mut self,
        subject: Subject,
        rule: RuleId,
        reading: Reading,
        now: u64,
        policy: Policy,
    ) -> (Option<Notice>, Option<Change>) {
        let track = self.tracks.entry((subject.clone(), rule)).or_default();
        match reading {
            Reading::Unknown => (None, None),
            Reading::Bad {
                detail,
                since,
                min_duration,
            } => {
                track.good_streak = 0;
                track.bad_streak = track.bad_streak.saturating_add(1);
                let first_bad_at = *track.first_bad_at.get_or_insert(now);
                let started_at = since.unwrap_or(first_bad_at).min(now);
                if let Some(incident) = &mut track.incident {
                    incident.detail = detail;
                    let due = match incident.notified_at {
                        // Never delivered: try the alert again.
                        None => Some(Notice::Opened(incident.clone())),
                        Some(at)
                            if incident.severity == Severity::Critical
                                && policy.renotify_interval > 0
                                && now.saturating_sub(at) >= policy.renotify_interval =>
                        {
                            Some(Notice::Reminder(incident.clone()))
                        }
                        Some(_) => None,
                    };
                    return (due, None);
                }
                let lasted = now.saturating_sub(started_at) >= min_duration;
                if track.bad_streak < policy.consecutive_samples || !lasted {
                    return (None, None);
                }
                let incident = Incident {
                    subject,
                    rule,
                    severity: rule.severity(),
                    started_at,
                    opened_at: now,
                    notified_at: None,
                    detail,
                };
                track.incident = Some(incident.clone());
                (
                    Some(Notice::Opened(incident.clone())),
                    Some(Change::Upsert(incident)),
                )
            }
            Reading::Good { detail } => {
                track.bad_streak = 0;
                track.first_bad_at = None;
                if track.incident.is_none() {
                    track.good_streak = 0;
                    return (None, None);
                }
                track.good_streak = track.good_streak.saturating_add(1);
                if track.good_streak < policy.consecutive_samples {
                    return (None, None);
                }
                track.good_streak = 0;
                let incident = track.incident.take().expect("checked above");
                let delete = Change::Delete(incident.subject.clone(), incident.rule);
                // An alert never delivered needs no "resolved" either.
                let notice = incident.notified_at.map(|_| Notice::Resolved {
                    incident,
                    at: now,
                    detail,
                });
                (notice, Some(delete))
            }
        }
    }

    /// Records that the alert or reminder of an incident was delivered.
    pub fn mark_notified(&mut self, subject: &Subject, rule: RuleId, now: u64) -> Option<Change> {
        let incident = self
            .tracks
            .get_mut(&(subject.clone(), rule))?
            .incident
            .as_mut()?;
        incident.notified_at = Some(now);
        Some(Change::Upsert(incident.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::Keys;

    const T0: u64 = 1_792_000_000;
    const POLICY: Policy = Policy {
        consecutive_samples: 3,
        renotify_interval: 3600,
    };

    fn mostro() -> Subject {
        Subject::Mostro(Keys::generate().public_key())
    }

    fn bad(min_duration: u64) -> Reading {
        Reading::Bad {
            detail: "bad".into(),
            since: None,
            min_duration,
        }
    }

    fn good() -> Reading {
        Reading::Good {
            detail: Some("fine".into()),
        }
    }

    /// Feeds `reading` once a minute from `start`, delivering every notice
    /// at once. Returns the notices.
    fn run(
        tracker: &mut Tracker,
        subject: &Subject,
        rule: RuleId,
        readings: &[Reading],
        start: u64,
    ) -> Vec<Notice> {
        let mut notices = Vec::new();
        for (i, reading) in readings.iter().enumerate() {
            let now = start + 60 * i as u64;
            let (notice, _) = tracker.observe(subject.clone(), rule, reading.clone(), now, POLICY);
            if let Some(notice) = notice {
                if !matches!(notice, Notice::Resolved { .. }) {
                    tracker.mark_notified(subject, rule, now);
                }
                notices.push(notice);
            }
        }
        notices
    }

    #[test]
    fn an_alert_fires_after_consecutive_bad_samples_and_only_once() {
        let mut tracker = Tracker::default();
        let s = mostro();

        let notices = run(&mut tracker, &s, RuleId::BlockLag, &vec![bad(0); 10], T0);

        assert_eq!(notices.len(), 1);
        let Notice::Opened(incident) = &notices[0] else {
            panic!("expected an alert");
        };
        assert_eq!(incident.opened_at, T0 + 120, "third sample");
        assert_eq!(incident.started_at, T0);
    }

    #[test]
    fn a_single_bad_sample_between_good_ones_never_alerts() {
        let mut tracker = Tracker::default();
        let s = mostro();
        let flapping = [
            bad(0),
            bad(0),
            good(),
            bad(0),
            good(),
            bad(0),
            bad(0),
            good(),
        ];

        assert!(run(&mut tracker, &s, RuleId::BlockLag, &flapping, T0).is_empty());
    }

    #[test]
    fn unknown_readings_neither_count_nor_break_a_streak() {
        let mut tracker = Tracker::default();
        let s = mostro();
        let readings = [bad(0), Reading::Unknown, bad(0), Reading::Unknown, bad(0)];

        let notices = run(&mut tracker, &s, RuleId::LndUnsynced, &readings, T0);

        assert_eq!(notices.len(), 1);
    }

    #[test]
    fn a_condition_must_also_last_its_minimum_duration() {
        let mut tracker = Tracker::default();
        let s = mostro();

        // 15 minutes of lag, sampled every minute: fires at minute 15.
        let notices = run(&mut tracker, &s, RuleId::BlockLag, &vec![bad(900); 20], T0);

        let Notice::Opened(incident) = &notices[0] else {
            panic!("expected an alert");
        };
        assert_eq!(incident.opened_at, T0 + 900);
    }

    #[test]
    fn a_condition_older_than_the_watchdog_alerts_at_once_after_the_samples() {
        let mut tracker = Tracker::default();
        let s = mostro();
        let since_hours_ago = Reading::Bad {
            detail: "unsynced".into(),
            since: Some(T0 - 6 * 3600),
            min_duration: 600,
        };

        let notices = run(
            &mut tracker,
            &s,
            RuleId::LndUnsynced,
            &vec![since_hours_ago; 3],
            T0,
        );

        let Notice::Opened(incident) = &notices[0] else {
            panic!("expected an alert");
        };
        assert_eq!(incident.started_at, T0 - 6 * 3600);
    }

    #[test]
    fn critical_incidents_are_repeated_each_renotify_interval_warnings_never() {
        let s = mostro();
        let two_hours = vec![bad(0); 125];

        let mut tracker = Tracker::default();
        let critical = run(&mut tracker, &s, RuleId::BlockLag, &two_hours, T0);
        let mut tracker = Tracker::default();
        let warning = run(&mut tracker, &s, RuleId::SettledUnpaid, &two_hours, T0);

        let reminders = critical
            .iter()
            .filter(|n| matches!(n, Notice::Reminder(_)))
            .count();
        assert_eq!(reminders, 2);
        assert_eq!(warning.len(), 1);
    }

    #[test]
    fn recovery_needs_consecutive_good_samples_and_reports_once() {
        let mut tracker = Tracker::default();
        let s = mostro();
        let mut readings = vec![bad(0); 5];
        readings.extend([good(), bad(0), good(), good(), good(), good(), good()]);

        let notices = run(&mut tracker, &s, RuleId::StatusDown, &readings, T0);

        assert_eq!(notices.len(), 2);
        let Notice::Resolved { at, .. } = &notices[1] else {
            panic!("expected a recovery");
        };
        assert_eq!(*at, T0 + 60 * 9, "third good sample in a row");
    }

    #[test]
    fn an_undelivered_alert_is_retried_and_needs_no_recovery() {
        let mut tracker = Tracker::default();
        let s = mostro();
        let rule = RuleId::HealthSilent;

        for i in 0..3 {
            tracker.observe(s.clone(), rule, bad(0), T0 + i, POLICY);
        }
        // Telegram was down: not marked delivered. The next bad sample
        // tries again.
        let (retry, _) = tracker.observe(s.clone(), rule, bad(0), T0 + 3, POLICY);
        assert!(matches!(retry, Some(Notice::Opened(_))));

        for i in 4..7 {
            let (notice, _) = tracker.observe(s.clone(), rule, good(), T0 + i, POLICY);
            assert!(notice.is_none());
        }
        assert_eq!(tracker.open_incidents().count(), 0);
    }

    #[test]
    fn a_resumed_incident_is_not_alerted_again() {
        let s = mostro();
        let incident = Incident {
            subject: s.clone(),
            rule: RuleId::BlockLag,
            severity: Severity::Critical,
            started_at: T0 - 600,
            opened_at: T0 - 300,
            notified_at: Some(T0 - 300),
            detail: "lag".into(),
        };
        let mut tracker = Tracker::resume([incident]);

        let notices = run(&mut tracker, &s, RuleId::BlockLag, &vec![bad(0); 5], T0);
        assert!(notices.is_empty());

        let notices = run(
            &mut tracker,
            &s,
            RuleId::BlockLag,
            &vec![good(); 3],
            T0 + 600,
        );
        assert!(matches!(notices.as_slice(), [Notice::Resolved { .. }]));
    }

    #[test]
    fn opening_and_resolving_produce_store_changes() {
        let mut tracker = Tracker::default();
        let s = mostro();
        let rule = RuleId::InfoSilent;
        let mut changes = Vec::new();

        for (i, reading) in [bad(0), bad(0), bad(0), good(), good(), good()]
            .into_iter()
            .enumerate()
        {
            let now = T0 + i as u64;
            let (notice, change) = tracker.observe(s.clone(), rule, reading, now, POLICY);
            changes.extend(change);
            if matches!(notice, Some(Notice::Opened(_))) {
                changes.extend(tracker.mark_notified(&s, rule, now));
            }
        }

        assert!(matches!(changes[0], Change::Upsert(ref i) if i.notified_at.is_none()));
        assert!(matches!(changes[1], Change::Upsert(ref i) if i.notified_at == Some(T0 + 2)));
        assert_eq!(changes[2], Change::Delete(s, rule));
    }
}
