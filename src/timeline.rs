//! One message per dispute: a header with where the dispute stands and a
//! timeline of everything the watchdog knows about it, in event order.
//!
//! Every source (Mostro's kind-38386 statuses, Serbero's updates) appends a
//! step to the dispute's timeline, then the message is rendered again from
//! the whole timeline and edited in place. Edits do not notify, so a live
//! edit is followed by a [`Nudge`]: a short reply to the message, deleted
//! moments later, whose only job is to make the phones ring.

use std::collections::HashMap;
use std::time::Duration;

use tracing::{info, warn};

use crate::config::AlertsConfig;
use crate::db::{DisputeMessageStore, StoredMessage, TimelineRow};
use crate::serbero::render::humanize;
use crate::serbero::telegram::Messenger;
use crate::{chrono_timestamp, escape_markdown, escape_markdown_code};

/// Steps shown at most; a longer timeline keeps its first step and its
/// last ones, under Telegram's message size limit.
const MAX_SHOWN_STEPS: usize = 12;

/// A step of a dispute's timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// Kind-38386 `initiated`; detail: the initiator.
    Opened,
    /// Kind-38386 `in-progress`; detail: the `solver` tag, when Mostro
    /// sends one. Serbero's when it reports on the dispute; a later one is
    /// a solver taking the dispute over.
    Taken,
    /// Serbero's `mediating`.
    SerberoMediating,
    /// Serbero's `guidance sent`; detail: the path.
    SerberoGuided,
    /// Serbero's `handed off`; detail: the reason.
    SerberoHandedOff,
    /// Serbero's `mediation could not start`.
    SerberoCouldNotStart,
    /// A final kind-38386 status; detail: the status, then `:` and the
    /// `solver` tag when Mostro sends one.
    Resolved,
    /// The cooperative cancel of older nodes (`canceled`).
    Canceled,
    /// A kind-38386 status this version does not know; detail: the status.
    Status,
}

impl EntryKind {
    /// The name stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::Taken => "taken",
            Self::SerberoMediating => "serbero_mediating",
            Self::SerberoGuided => "serbero_guided",
            Self::SerberoHandedOff => "serbero_handed_off",
            Self::SerberoCouldNotStart => "serbero_could_not_start",
            Self::Resolved => "resolved",
            Self::Canceled => "canceled",
            Self::Status => "status",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        [
            Self::Opened,
            Self::Taken,
            Self::SerberoMediating,
            Self::SerberoGuided,
            Self::SerberoHandedOff,
            Self::SerberoCouldNotStart,
            Self::Resolved,
            Self::Canceled,
            Self::Status,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == name)
    }

    fn is_serbero(self) -> bool {
        self.serbero_stage().is_some()
    }

    /// The order of steps that share a second (Nostr times are whole
    /// seconds): a dispute opens, is taken, is mediated, then ends.
    fn rank(self) -> u8 {
        match self {
            Self::Opened => 0,
            Self::Taken => 1,
            Self::SerberoMediating => 2,
            Self::SerberoGuided => 3,
            Self::SerberoHandedOff | Self::SerberoCouldNotStart => 4,
            Self::Status => 5,
            Self::Resolved | Self::Canceled => 6,
        }
    }

    /// Where a Serbero step stands in a mediation: each happens once per
    /// dispute, so a late notice of an earlier stage never moves the
    /// dispute back (the same order as `dm::Update::stage`).
    fn serbero_stage(self) -> Option<u8> {
        match self {
            Self::SerberoMediating => Some(0),
            Self::SerberoGuided => Some(1),
            Self::SerberoHandedOff | Self::SerberoCouldNotStart => Some(2),
            _ => None,
        }
    }
}

/// A step of a dispute's timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    pub detail: Option<String>,
    /// The `created_at` of the source event or DM, Unix seconds.
    pub created_at: i64,
}

impl Entry {
    pub fn new(kind: EntryKind, detail: Option<&str>, created_at: i64) -> Self {
        Self {
            kind,
            detail: detail.map(str::to_owned),
            created_at,
        }
    }
}

/// The steps of stored rows, by event time and then by lifecycle, so a
/// resolution dated the same second as the take still ends the dispute. A
/// row written by a newer version with a kind this one does not know is
/// skipped and logged.
pub fn entries(rows: &[TimelineRow]) -> Vec<Entry> {
    let mut entries: Vec<Entry> = rows
        .iter()
        .filter_map(|row| match EntryKind::parse(&row.kind) {
            Some(kind) => Some(Entry::new(kind, row.detail.as_deref(), row.created_at)),
            None => {
                warn!(kind = %row.kind, "Unknown timeline step kind, not shown");
                None
            }
        })
        .collect();
    entries.sort_by_key(|entry| (entry.created_at, entry.kind.rank()));
    entries
}

/// The step a kind-38386 `status` adds, with who opened the dispute and
/// the `solver` tag when Mostro sends one.
pub fn status_step(
    status: &str,
    initiator: Option<&str>,
    solver: Option<&str>,
) -> (EntryKind, Option<String>) {
    match status {
        "initiated" => (EntryKind::Opened, initiator.map(str::to_owned)),
        "in-progress" => (EntryKind::Taken, solver.map(str::to_owned)),
        "released" | "cooperatively-canceled" => (EntryKind::Resolved, Some(status.to_owned())),
        "settled" | "seller-refunded" => (
            EntryKind::Resolved,
            Some(match solver {
                Some(solver) => format!("{status}:{solver}"),
                None => status.to_owned(),
            }),
        ),
        "canceled" => (EntryKind::Canceled, None),
        other => (EntryKind::Status, Some(other.to_owned())),
    }
}

/// Opens the timeline of a message sent before the timeline existed: a
/// `dispute_messages` row with no steps gets its stored status, dated when
/// the message was sent, so the next step is not the whole story. Called
/// before any step is added for the dispute.
pub async fn backfill(store: &DisputeMessageStore, dispute_id: &str) -> Result<(), sqlx::Error> {
    let Some(message) = store.get_message(dispute_id).await? else {
        return Ok(());
    };
    if !store.timeline(dispute_id).await?.is_empty() {
        return Ok(());
    }
    let (kind, detail) = status_step(&message.status, None, None);
    store
        .append_timeline(
            dispute_id,
            kind.as_str(),
            detail.as_deref(),
            message.created_at,
        )
        .await?;
    info!(
        dispute_id,
        "Opened the timeline of a message sent before timelines"
    );
    Ok(())
}

/// How solvers are named on the timeline: `[alerts] solver_names`, or a
/// shortened pubkey.
#[derive(Debug, Clone, Default)]
pub struct Names {
    solvers: HashMap<String, String>,
}

impl Names {
    pub fn new(solvers: HashMap<String, String>) -> Self {
        Self { solvers }
    }

    /// Who a `solver` tag names: the configured name, `solver <short
    /// pubkey>`, or `a solver` when the tag is missing.
    fn solver(&self, pubkey: Option<&str>) -> String {
        match pubkey {
            Some(pubkey) => match self.solvers.get(pubkey) {
                Some(name) => name.clone(),
                None => format!("solver {}", shorten(pubkey)),
            },
            None => "a solver".to_owned(),
        }
    }
}

/// `000000e2fdb5…a7f1`: enough of a hex pubkey to tell solvers apart.
fn shorten(pubkey: &str) -> String {
    const HEAD: usize = 12;
    const TAIL: usize = 4;
    let chars: Vec<char> = pubkey.chars().collect();
    if chars.len() <= HEAD + TAIL {
        return pubkey.to_owned();
    }
    let head: String = chars[..HEAD].iter().collect();
    let tail: String = chars[chars.len() - TAIL..].iter().collect();
    format!("{head}…{tail}")
}

/// Who took a dispute, for a `Taken` step.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Holder {
    Serbero,
    /// The solver's label and whether they took the dispute over from an
    /// earlier holder.
    Solver {
        label: String,
        took_over: bool,
    },
}

/// Who each `Taken` step names. Serbero's own kind-38386 event names no
/// solver: the first `Taken` of a dispute Serbero reported on is Serbero's,
/// and every later one is a person taking it over.
fn holders(entries: &[Entry], names: &Names) -> Vec<Option<Holder>> {
    let serbero_involved = entries.iter().any(|e| e.kind.is_serbero());
    let mut taken_before = 0;
    entries
        .iter()
        .map(|entry| {
            if entry.kind != EntryKind::Taken {
                return None;
            }
            taken_before += 1;
            if serbero_involved && taken_before == 1 {
                return Some(Holder::Serbero);
            }
            Some(Holder::Solver {
                label: names.solver(entry.detail.as_deref()),
                took_over: taken_before > 1,
            })
        })
        .collect()
}

/// Where a dispute stands, from its steps in order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Standing {
    Open,
    /// Serbero holds it; what it is doing, when it said.
    WithSerbero(Option<String>),
    /// Serbero gave up; why.
    NeedsSolver(String),
    WithSolver(String),
    Resolved(String),
    Canceled,
    Other(String),
}

fn standing(entries: &[Entry], holders: &[Option<Holder>], names: &Names) -> Standing {
    let mut standing = Standing::Open;
    let mut serbero_stage = 0;
    for (entry, holder) in entries.iter().zip(holders) {
        let serbero_holds = matches!(
            standing,
            Standing::Open | Standing::WithSerbero(_) | Standing::NeedsSolver(_)
        );
        // A late Serbero notice never undoes a takeover, an outcome or a
        // later stage of the mediation.
        let late_serbero = entry.kind.serbero_stage().is_some_and(|stage| {
            let late = !serbero_holds || stage < serbero_stage;
            serbero_stage = serbero_stage.max(stage);
            late
        });
        standing = match (entry.kind, holder) {
            (EntryKind::Opened, _) => Standing::Open,
            (EntryKind::Taken, Some(Holder::Serbero)) => Standing::WithSerbero(None),
            (EntryKind::Taken, Some(Holder::Solver { label, .. })) => {
                Standing::WithSolver(label.clone())
            }
            (EntryKind::Taken, None) => standing,
            (_, _) if late_serbero => standing,
            (EntryKind::SerberoMediating, _) => Standing::WithSerbero(Some("mediating".into())),
            (EntryKind::SerberoGuided, _) => {
                Standing::WithSerbero(Some("guided the parties".into()))
            }
            (EntryKind::SerberoHandedOff, _) => {
                Standing::NeedsSolver(detailed("handed off", entry.detail.as_deref()))
            }
            (EntryKind::SerberoCouldNotStart, _) => {
                Standing::NeedsSolver("mediation could not start".into())
            }
            (EntryKind::Resolved, _) => {
                Standing::Resolved(outcome(entry.detail.as_deref(), names).header)
            }
            (EntryKind::Canceled, _) => Standing::Canceled,
            (EntryKind::Status, _) => {
                Standing::Other(entry.detail.clone().unwrap_or_else(|| "unknown".into()))
            }
        };
    }
    standing
}

fn header(standing: &Standing) -> String {
    match standing {
        Standing::Open => "🚨 OPEN · needs a solver".to_owned(),
        Standing::WithSerbero(None) => "🤖 WITH SERBERO".to_owned(),
        Standing::WithSerbero(Some(doing)) => format!("🤖 WITH SERBERO · {doing}"),
        Standing::NeedsSolver(why) => format!("🙋 NEEDS A SOLVER · {why}"),
        Standing::WithSolver(label) => format!("👨‍⚖️ WITH A SOLVER · {label}"),
        Standing::Resolved(how) => format!("✅ RESOLVED · {how}"),
        Standing::Canceled => "🗑 CANCELED · cooperatively".to_owned(),
        Standing::Other(status) => format!("📡 {status}"),
    }
}

/// How a dispute ended, from a `Resolved` step's detail.
struct Outcome {
    icon: &'static str,
    /// For the header: `released by seller`.
    header: String,
    /// For the step: `Released by seller · resolved by the parties`.
    line: String,
}

fn outcome(detail: Option<&str>, names: &Names) -> Outcome {
    let (status, solver) = match detail.and_then(|d| d.split_once(':')) {
        Some((status, solver)) => (status, Some(solver)),
        None => (detail.unwrap_or("resolved"), None),
    };
    match status {
        "released" => Outcome {
            icon: "🔓",
            header: "released by seller".into(),
            line: "Released by seller · resolved by the parties".into(),
        },
        "cooperatively-canceled" => Outcome {
            icon: "🤝",
            header: "canceled cooperatively".into(),
            line: "Canceled cooperatively · resolved by the parties".into(),
        },
        "settled" => Outcome {
            icon: "✅",
            header: format!("settled, buyer paid, by {}", names.solver(solver)),
            line: format!("Settled, buyer paid · resolved by {}", names.solver(solver)),
        },
        "seller-refunded" => Outcome {
            icon: "💰",
            header: format!("seller refunded by {}", names.solver(solver)),
            line: format!("Seller refunded · resolved by {}", names.solver(solver)),
        },
        other => Outcome {
            icon: "✔️",
            header: other.to_owned(),
            line: format!("Resolved · {other}"),
        },
    }
}

fn detailed(text: &str, detail: Option<&str>) -> String {
    match detail {
        Some(detail) => format!("{text} · {}", humanize(detail)),
        None => text.to_owned(),
    }
}

/// A step's icon and text, before escaping.
fn step(entry: &Entry, holder: Option<&Holder>, names: &Names) -> (&'static str, String) {
    match (entry.kind, holder) {
        (EntryKind::Opened, _) => (
            "🚨",
            match entry.detail.as_deref() {
                Some(initiator) if initiator != "unknown" => format!("Opened by {initiator}"),
                _ => "Opened".to_owned(),
            },
        ),
        (EntryKind::Taken, Some(Holder::Serbero)) => ("🤖", "Taken by Serbero".to_owned()),
        (
            EntryKind::Taken,
            Some(Holder::Solver {
                label,
                took_over: true,
            }),
        ) => ("👨‍⚖️", format!("Taken over by {label}")),
        (EntryKind::Taken, _) => (
            "👨‍⚖️",
            format!("Taken by {}", names.solver(entry.detail.as_deref())),
        ),
        (EntryKind::SerberoMediating, _) => ("🤖", "Serbero mediating".to_owned()),
        (EntryKind::SerberoGuided, _) => (
            "🤖",
            detailed("Serbero guided the parties", entry.detail.as_deref()),
        ),
        (EntryKind::SerberoHandedOff, _) => (
            "🙋",
            detailed("Serbero handed off", entry.detail.as_deref()),
        ),
        (EntryKind::SerberoCouldNotStart, _) => {
            ("🙋", "Serbero could not start mediation".to_owned())
        }
        (EntryKind::Resolved, _) => {
            let outcome = outcome(entry.detail.as_deref(), names);
            (outcome.icon, outcome.line)
        }
        (EntryKind::Canceled, _) => ("🗑", "Canceled cooperatively".to_owned()),
        (EntryKind::Status, _) => (
            "📡",
            format!("Status: {}", entry.detail.as_deref().unwrap_or("unknown")),
        ),
    }
}

/// `2026-10-10` and `09:41:02` of a Unix time.
fn date_and_time(created_at: i64) -> (String, String) {
    let stamp = chrono_timestamp(u64::try_from(created_at).unwrap_or_default());
    let date = stamp.get(..10).unwrap_or_default().to_owned();
    let time = stamp.get(11..19).unwrap_or_default().to_owned();
    (date, time)
}

/// The dispute's message (MarkdownV2): where it stands and how it got
/// there. Times are UTC; the date shows once, and again on a step whose
/// day differs from the step before it.
pub fn render(dispute_id: &str, entries: &[Entry], names: &Names) -> String {
    let holders = holders(entries, names);
    let standing = standing(entries, &holders, names);
    let mut text = format!(
        "⚖️ *DISPUTE* `{}`\n*Status:* {}\n",
        escape_markdown_code(dispute_id),
        escape_markdown(&header(&standing)),
    );
    if entries.is_empty() {
        return text;
    }

    let omitted = entries.len().saturating_sub(MAX_SHOWN_STEPS);
    let shown: Vec<(usize, &Entry)> = entries
        .iter()
        .enumerate()
        .filter(|(i, _)| *i == 0 || *i > omitted)
        .collect();
    let (first_date, _) = date_and_time(shown[0].1.created_at);
    let mut previous_date = first_date.clone();
    text.push('\n');
    for (position, (index, entry)) in shown.iter().enumerate() {
        if position == 1 && omitted > 0 {
            text.push_str(&escape_markdown(&format!(
                "… {omitted} earlier steps omitted\n"
            )));
        }
        let (date, time) = date_and_time(entry.created_at);
        let stamp = if date == previous_date {
            time
        } else {
            format!("{date} {time}")
        };
        previous_date = date;
        let (icon, line) = step(entry, holders[*index].as_ref(), names);
        text.push_str(&format!(
            "{icon} `{}` {}\n",
            escape_markdown_code(&stamp),
            escape_markdown(&line)
        ));
    }
    text.push_str(&format!(
        "\n_All times UTC · {}_",
        escape_markdown(&first_date)
    ));
    text
}

/// The notification of an edit: a short reply to the dispute's message,
/// deleted once the phones have rung. `[alerts] edit_notifications`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Nudge {
    /// How long the reply stays before it is deleted.
    pub lifetime: Duration,
}

impl Nudge {
    /// The nudge `[alerts]` asks for; `None` with `edit_notifications` off.
    pub fn from_config(alerts: &AlertsConfig) -> Option<Self> {
        alerts.edit_notifications.then_some(Self {
            lifetime: Duration::from_secs(alerts.edit_notification_lifetime),
        })
    }
}

/// The nudge's text (MarkdownV2): the dispute and `added`, the step the
/// edit showed, so the push notification's preview says what happened.
/// The step is read in its place on the timeline, which decides who a
/// `Taken` names; a step not on the timeline reads as its latest. `None`
/// for an empty timeline.
pub fn nudge_text(
    dispute_id: &str,
    entries: &[Entry],
    added: &Entry,
    names: &Names,
) -> Option<String> {
    let holders = holders(entries, names);
    let index = entries
        .iter()
        .rposition(|entry| entry == added)
        .or_else(|| entries.len().checked_sub(1))?;
    let (icon, line) = step(&entries[index], holders[index].as_ref(), names);
    Some(format!(
        "🔔 *Dispute* `{}`\n{icon} {}",
        escape_markdown_code(dispute_id),
        escape_markdown(&line)
    ))
}

/// Notifies of an edit of `message`, the dispute's message, with a reply
/// naming `added`, the step the edit showed. Nothing is sent for an empty
/// timeline.
pub async fn nudge<M: Messenger>(
    store: &DisputeMessageStore,
    telegram: &M,
    names: &Names,
    dispute_id: &str,
    message: &StoredMessage,
    added: &Entry,
    nudge: Nudge,
) -> Result<(), sqlx::Error> {
    let rows = store.timeline(dispute_id).await?;
    let Some(text) = nudge_text(dispute_id, &entries(&rows), added, names) else {
        return Ok(());
    };
    telegram
        .nudge(message.chat_id, message.message_id, &text, nudge.lifetime)
        .await;
    info!(dispute_id, "🔔 Edit notification sent");
    Ok(())
}

/// Why a redraw failed. The timeline is kept either way; the next step
/// redraws the message again.
#[derive(Debug, thiserror::Error)]
pub enum RedrawError {
    #[error("dispute store: {0}")]
    Store(#[from] sqlx::Error),
    #[error("Telegram: {0}")]
    Telegram(#[from] teloxide::RequestError),
}

/// The dispute's message rendered from its whole timeline.
pub async fn rendered(
    store: &DisputeMessageStore,
    names: &Names,
    dispute_id: &str,
) -> Result<String, sqlx::Error> {
    let rows = store.timeline(dispute_id).await?;
    Ok(render(dispute_id, &entries(&rows), names))
}

/// Renders the dispute's message again and edits it. `Ok(None)` when the
/// dispute has no message to edit; otherwise the message, now up to date.
pub async fn redraw<M: Messenger>(
    store: &DisputeMessageStore,
    telegram: &M,
    names: &Names,
    dispute_id: &str,
) -> Result<Option<StoredMessage>, RedrawError> {
    let Some(message) = store.get_message(dispute_id).await? else {
        return Ok(None);
    };
    let text = rendered(store, names, dispute_id).await?;
    telegram
        .edit(message.chat_id, message.message_id, &text)
        .await?;
    store
        .update_status(dispute_id, &message.status, &text)
        .await?;
    info!(dispute_id, "✏️ Dispute message redrawn from its timeline");
    Ok(Some(message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serbero::testing::{Call, FakeTelegram};

    const DISPUTE: &str = "96629381-bcb8-4d4f-8c66-e8f86f3e86ea";
    const SOLVER: &str = "000000e2fdb5000000000000000000000000000000000000000000000000a7f1";
    /// 2026-10-10 09:41:02 UTC.
    const OPENED: i64 = 1_791_625_262;

    fn entry(kind: EntryKind, detail: Option<&str>, offset: i64) -> Entry {
        Entry::new(kind, detail, OPENED + offset)
    }

    fn named() -> Names {
        Names::new(HashMap::from([(SOLVER.to_owned(), "grunch".to_owned())]))
    }

    fn header_of(entries: &[Entry], names: &Names) -> String {
        let holders = holders(entries, names);
        header(&standing(entries, &holders, names))
    }

    #[test]
    fn the_nudge_names_the_dispute_and_the_step_added_escaped() {
        let handoff = entry(EntryKind::SerberoHandedOff, Some("conflicting_claims"), 471);
        let entries = vec![
            entry(EntryKind::Opened, Some("buyer"), 0),
            entry(EntryKind::Taken, None, 1),
            handoff.clone(),
        ];

        let text = nudge_text(DISPUTE, &entries, &handoff, &Names::default()).unwrap();

        assert_eq!(
            text,
            "🔔 *Dispute* `96629381-bcb8-4d4f-8c66-e8f86f3e86ea`\n\
             🙋 Serbero handed off · conflicting claims"
        );
    }

    #[test]
    fn the_nudge_knows_who_took_the_dispute_over() {
        let takeover = entry(EntryKind::Taken, Some(SOLVER), 600);
        let entries = vec![
            entry(EntryKind::Opened, Some("seller"), 0),
            entry(EntryKind::Taken, None, 1),
            entry(EntryKind::SerberoMediating, None, 3),
            takeover.clone(),
        ];

        let text = nudge_text(DISPUTE, &entries, &takeover, &named()).unwrap();

        assert!(text.ends_with("👨‍⚖️ Taken over by grunch"), "{text}");
    }

    /// A Serbero notice that arrives after the takeover is the news, not
    /// the takeover.
    #[test]
    fn the_nudge_names_a_late_step_not_the_latest() {
        let late = entry(EntryKind::SerberoMediating, None, 3);
        let entries = vec![
            entry(EntryKind::Opened, Some("seller"), 0),
            entry(EntryKind::Taken, None, 1),
            late.clone(),
            entry(EntryKind::Taken, Some(SOLVER), 600),
        ];

        let text = nudge_text(DISPUTE, &entries, &late, &named()).unwrap();

        assert!(text.ends_with("🤖 Serbero mediating"), "{text}");
    }

    #[test]
    fn a_step_missing_from_the_timeline_reads_as_its_latest() {
        let entries = vec![entry(EntryKind::Opened, Some("buyer"), 0)];
        let unstored = entry(EntryKind::Taken, None, 1);

        let text = nudge_text(DISPUTE, &entries, &unstored, &Names::default()).unwrap();

        assert!(text.ends_with("🚨 Opened by buyer"), "{text}");
        assert_eq!(nudge_text(DISPUTE, &[], &unstored, &Names::default()), None);
    }

    #[test]
    fn the_nudge_is_off_when_edit_notifications_are() {
        let on = AlertsConfig::default();
        let off = AlertsConfig {
            edit_notifications: false,
            ..AlertsConfig::default()
        };

        assert_eq!(
            Nudge::from_config(&on),
            Some(Nudge {
                lifetime: Duration::from_secs(60)
            })
        );
        assert_eq!(Nudge::from_config(&off), None);
    }

    #[test]
    fn renders_the_incidents_lifecycle_in_event_order() {
        // The steps as they reached the watchdog: the release first, from a
        // catch-up, and the handoff DM later. The store sorts them; here
        // the test does.
        let mut entries = vec![
            entry(EntryKind::Resolved, Some("released"), 1_579),
            entry(EntryKind::Opened, Some("buyer"), 0),
            entry(EntryKind::SerberoHandedOff, Some("facts_gathered"), 471),
            entry(EntryKind::Taken, None, 1),
            entry(EntryKind::SerberoMediating, None, 3),
        ];
        entries.sort_by_key(|e| e.created_at);

        let text = render(DISPUTE, &entries, &Names::default());

        assert_eq!(
            text,
            "⚖️ *DISPUTE* `96629381-bcb8-4d4f-8c66-e8f86f3e86ea`\n\
             *Status:* ✅ RESOLVED · released by seller\n\
             \n\
             🚨 `09:41:02` Opened by buyer\n\
             🤖 `09:41:03` Taken by Serbero\n\
             🤖 `09:41:05` Serbero mediating\n\
             🙋 `09:48:53` Serbero handed off · facts gathered\n\
             🔓 `10:07:21` Released by seller · resolved by the parties\n\
             \n\
             _All times UTC · 2026\\-10\\-10_"
        );
    }

    #[test]
    fn renders_a_takeover_and_a_solver_decision_with_the_configured_name() {
        let entries = vec![
            entry(EntryKind::Opened, Some("seller"), 0),
            entry(EntryKind::Taken, None, 1),
            entry(EntryKind::SerberoMediating, None, 2),
            entry(EntryKind::SerberoHandedOff, Some("facts_gathered"), 355),
            entry(EntryKind::Taken, Some(SOLVER), 2_288),
            entry(
                EntryKind::Resolved,
                Some(&format!("seller-refunded:{SOLVER}")),
                3_603,
            ),
        ];

        let text = render(DISPUTE, &entries, &named());

        assert!(text.contains("*Status:* ✅ RESOLVED · seller refunded by grunch\n"));
        assert!(text.contains("👨‍⚖️ `10:19:10` Taken over by grunch\n"));
        assert!(text.contains("💰 `10:41:05` Seller refunded · resolved by grunch\n"));
    }

    #[test]
    fn an_unknown_solver_shows_a_shortened_pubkey_and_no_tag_shows_a_solver() {
        let entries = vec![
            entry(EntryKind::Opened, Some("buyer"), 0),
            entry(EntryKind::Taken, Some(SOLVER), 10),
            entry(EntryKind::Resolved, Some("settled"), 20),
        ];

        let text = render(DISPUTE, &entries, &Names::default());

        assert!(text.contains("👨‍⚖️ `09:41:12` Taken by solver 000000e2fdb5…a7f1\n"));
        assert!(text.contains("✅ `09:41:22` Settled, buyer paid · resolved by a solver\n"));
        assert!(text.contains("*Status:* ✅ RESOLVED · settled, buyer paid, by a solver\n"));
    }

    #[test]
    fn the_header_follows_the_dispute() {
        let names = named();
        let opened = entry(EntryKind::Opened, Some("buyer"), 0);
        let taken = entry(EntryKind::Taken, None, 1);
        let mediating = entry(EntryKind::SerberoMediating, None, 2);
        let guided = entry(EntryKind::SerberoGuided, Some("refund"), 3);
        let handed_off = entry(EntryKind::SerberoHandedOff, Some("round_limit"), 4);
        let could_not_start = entry(EntryKind::SerberoCouldNotStart, None, 4);
        let taken_over = entry(EntryKind::Taken, Some(SOLVER), 5);
        let canceled = entry(EntryKind::Canceled, None, 6);
        let other = entry(EntryKind::Status, Some("frozen"), 6);

        assert_eq!(header_of(&[], &names), "🚨 OPEN · needs a solver");
        assert_eq!(
            header_of(std::slice::from_ref(&opened), &names),
            "🚨 OPEN · needs a solver"
        );
        // Without Serbero, the first taker is a solver.
        assert_eq!(
            header_of(&[opened.clone(), taken.clone()], &names),
            "👨‍⚖️ WITH A SOLVER · a solver"
        );
        assert_eq!(
            header_of(&[opened.clone(), taken.clone(), mediating.clone()], &names),
            "🤖 WITH SERBERO · mediating"
        );
        assert_eq!(
            header_of(&[opened.clone(), taken.clone(), guided.clone()], &names),
            "🤖 WITH SERBERO · guided the parties"
        );
        assert_eq!(
            header_of(
                &[
                    opened.clone(),
                    taken.clone(),
                    mediating.clone(),
                    handed_off.clone()
                ],
                &names
            ),
            "🙋 NEEDS A SOLVER · handed off · round limit"
        );
        assert_eq!(
            header_of(
                &[opened.clone(), taken.clone(), could_not_start.clone()],
                &names
            ),
            "🙋 NEEDS A SOLVER · mediation could not start"
        );
        assert_eq!(
            header_of(
                &[
                    opened.clone(),
                    taken.clone(),
                    handed_off.clone(),
                    taken_over.clone()
                ],
                &names
            ),
            "👨‍⚖️ WITH A SOLVER · grunch"
        );
        assert_eq!(
            header_of(&[opened.clone(), canceled.clone()], &names),
            "🗑 CANCELED · cooperatively"
        );
        assert_eq!(header_of(&[opened, other], &names), "📡 frozen");
    }

    #[test]
    fn a_late_serbero_update_never_undoes_a_takeover_or_an_outcome() {
        let names = named();
        let base = [
            entry(EntryKind::Opened, Some("buyer"), 0),
            entry(EntryKind::Taken, None, 1),
            entry(EntryKind::SerberoMediating, None, 2),
        ];
        let late_handoff = entry(EntryKind::SerberoHandedOff, Some("flood"), 100);

        let mut taken_over = base.to_vec();
        taken_over.push(entry(EntryKind::Taken, Some(SOLVER), 50));
        taken_over.push(late_handoff.clone());
        assert_eq!(header_of(&taken_over, &names), "👨‍⚖️ WITH A SOLVER · grunch");

        let mut resolved = base.to_vec();
        resolved.push(entry(EntryKind::Resolved, Some("released"), 50));
        resolved.push(late_handoff);
        assert_eq!(
            header_of(&resolved, &names),
            "✅ RESOLVED · released by seller"
        );
    }

    #[test]
    fn a_late_notice_of_an_earlier_mediation_stage_keeps_the_handoff() {
        let names = named();
        let steps = [
            entry(EntryKind::Opened, Some("buyer"), 0),
            entry(EntryKind::Taken, None, 1),
            entry(EntryKind::SerberoHandedOff, Some("flood"), 10),
            entry(EntryKind::SerberoMediating, None, 20),
            entry(EntryKind::SerberoGuided, Some("refund"), 30),
        ];

        assert_eq!(
            header_of(&steps, &names),
            "🙋 NEEDS A SOLVER · handed off · flood"
        );
    }

    #[test]
    fn a_step_on_another_day_shows_its_date() {
        let entries = vec![
            entry(EntryKind::Opened, Some("buyer"), 0),
            entry(EntryKind::Taken, Some(SOLVER), 86_400),
            entry(EntryKind::Resolved, Some("released"), 86_460),
        ];

        let text = render(DISPUTE, &entries, &Names::default());

        assert!(text.contains("🚨 `09:41:02` Opened by buyer\n"));
        assert!(text.contains("👨‍⚖️ `2026-10-11 09:41:02` Taken by solver"));
        assert!(text.contains("🔓 `09:42:02` Released by seller"));
        assert!(text.ends_with("_All times UTC · 2026\\-10\\-10_"));
    }

    #[test]
    fn a_long_timeline_keeps_its_first_and_last_steps() {
        let mut entries = vec![entry(EntryKind::Opened, Some("buyer"), 0)];
        for i in 1..=20 {
            entries.push(entry(EntryKind::Status, Some(&format!("step{i}")), i));
        }

        let text = render(DISPUTE, &entries, &Names::default());

        let lines: Vec<&str> = text.lines().collect();
        assert!(text.contains(
            "Opened by buyer\n… 9 earlier steps omitted\n📡 `09:41:12` Status: step10\n"
        ));
        assert!(text.contains("Status: step20\n"));
        assert!(!text.contains("step9\n"));
        // Header (2), blank, 12 steps and the omission line, blank, footer.
        assert_eq!(lines.len(), 2 + 1 + 13 + 1 + 1);
    }

    #[test]
    fn reserved_characters_in_details_are_escaped() {
        let entries = vec![
            entry(EntryKind::Opened, Some("buyer_1"), 0),
            entry(EntryKind::Status, Some("on-hold (v2.1)"), 1),
        ];

        let text = render("id.with-dots_and_more", &entries, &Names::default());

        assert!(text.contains("`id.with-dots_and_more`"));
        assert!(text.contains("Opened by buyer\\_1\n"));
        assert!(text.contains("Status: on\\-hold \\(v2\\.1\\)\n"));
    }

    #[test]
    fn steps_in_the_same_second_keep_the_lifecycle_order() {
        let rows: Vec<TimelineRow> = [
            ("resolved", Some("released"), 10),
            ("taken", None, 10),
            ("serbero_mediating", None, 10),
            ("opened", Some("buyer"), 10),
        ]
        .into_iter()
        .map(|(kind, detail, created_at)| TimelineRow {
            kind: kind.into(),
            detail: detail.map(Into::into),
            created_at,
        })
        .collect();

        let parsed = entries(&rows);

        let kinds: Vec<EntryKind> = parsed.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                EntryKind::Opened,
                EntryKind::Taken,
                EntryKind::SerberoMediating,
                EntryKind::Resolved
            ]
        );
        assert_eq!(
            header_of(&parsed, &Names::default()),
            "✅ RESOLVED · released by seller"
        );
    }

    #[test]
    fn each_status_maps_to_its_step() {
        let step = |status: &str| status_step(status, Some("buyer"), Some(SOLVER));

        assert_eq!(step("initiated"), (EntryKind::Opened, Some("buyer".into())));
        assert_eq!(step("in-progress"), (EntryKind::Taken, Some(SOLVER.into())));
        assert_eq!(
            step("released"),
            (EntryKind::Resolved, Some("released".into()))
        );
        assert_eq!(
            step("cooperatively-canceled"),
            (EntryKind::Resolved, Some("cooperatively-canceled".into()))
        );
        assert_eq!(
            step("settled"),
            (EntryKind::Resolved, Some(format!("settled:{SOLVER}")))
        );
        assert_eq!(
            step("seller-refunded"),
            (
                EntryKind::Resolved,
                Some(format!("seller-refunded:{SOLVER}"))
            )
        );
        assert_eq!(step("canceled"), (EntryKind::Canceled, None));
        assert_eq!(step("frozen"), (EntryKind::Status, Some("frozen".into())));
        assert_eq!(
            status_step("settled", None, None),
            (EntryKind::Resolved, Some("settled".into()))
        );
        assert_eq!(
            status_step("in-progress", None, None),
            (EntryKind::Taken, None)
        );
    }

    #[tokio::test]
    async fn backfill_opens_the_timeline_of_a_message_sent_before_timelines() {
        let dir = tempfile::tempdir().unwrap();
        let store = DisputeMessageStore::new(&dir.path().join("t.db"))
            .await
            .unwrap();
        store
            .insert(DISPUTE, 7, -100, "in-progress", "old")
            .await
            .unwrap();
        let sent_at = store
            .get_message(DISPUTE)
            .await
            .unwrap()
            .unwrap()
            .created_at;

        backfill(&store, DISPUTE).await.unwrap();
        // Again, and for a dispute without a message: nothing more.
        backfill(&store, DISPUTE).await.unwrap();
        backfill(&store, "none").await.unwrap();

        assert_eq!(
            store.timeline(DISPUTE).await.unwrap(),
            vec![TimelineRow {
                kind: "taken".into(),
                detail: None,
                created_at: sent_at,
            }]
        );
        assert_eq!(store.timeline("none").await.unwrap(), vec![]);
    }

    #[test]
    fn every_kind_stores_and_parses_back() {
        for kind in [
            EntryKind::Opened,
            EntryKind::Taken,
            EntryKind::SerberoMediating,
            EntryKind::SerberoGuided,
            EntryKind::SerberoHandedOff,
            EntryKind::SerberoCouldNotStart,
            EntryKind::Resolved,
            EntryKind::Canceled,
            EntryKind::Status,
        ] {
            assert_eq!(EntryKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(EntryKind::parse("teleported"), None);
    }

    #[test]
    fn rows_of_an_unknown_kind_are_skipped() {
        let rows = vec![
            TimelineRow {
                kind: "opened".into(),
                detail: Some("buyer".into()),
                created_at: 1,
            },
            TimelineRow {
                kind: "teleported".into(),
                detail: None,
                created_at: 2,
            },
        ];

        let parsed = entries(&rows);

        assert_eq!(
            parsed,
            vec![Entry::new(EntryKind::Opened, Some("buyer"), 1)]
        );
    }

    #[tokio::test]
    async fn redraw_edits_the_stored_message_from_the_timeline() {
        let dir = tempfile::tempdir().unwrap();
        let store = DisputeMessageStore::new(&dir.path().join("t.db"))
            .await
            .unwrap();
        let telegram = FakeTelegram::default();
        store
            .insert(DISPUTE, 7, -100, "in-progress", "old")
            .await
            .unwrap();
        store
            .append_timeline(DISPUTE, "opened", Some("buyer"), OPENED)
            .await
            .unwrap();
        store
            .append_timeline(DISPUTE, "serbero_mediating", None, OPENED + 3)
            .await
            .unwrap();

        let redrawn = redraw(&store, &telegram, &Names::default(), DISPUTE)
            .await
            .unwrap();

        assert_eq!(redrawn.map(|m| m.message_id), Some(7));
        assert!(matches!(
            telegram.calls().as_slice(),
            [Call::Edit { chat_id: -100, message_id: 7, text }]
                if text.contains("Serbero mediating")
        ));
        let stored = store.get_message(DISPUTE).await.unwrap().unwrap();
        assert!(stored.text.unwrap().contains("Serbero mediating"));
        assert_eq!(stored.status, "in-progress");
        assert_eq!(
            redraw(&store, &telegram, &Names::default(), "none")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn the_nudge_replies_to_the_dispute_message_with_its_latest_step() {
        let dir = tempfile::tempdir().unwrap();
        let store = DisputeMessageStore::new(&dir.path().join("t.db"))
            .await
            .unwrap();
        let telegram = FakeTelegram::default();
        store
            .insert(DISPUTE, 7, -100, "in-progress", "old")
            .await
            .unwrap();
        store
            .append_timeline(DISPUTE, "opened", Some("buyer"), OPENED)
            .await
            .unwrap();
        store
            .append_timeline(DISPUTE, "serbero_mediating", None, OPENED + 3)
            .await
            .unwrap();
        let message = store.get_message(DISPUTE).await.unwrap().unwrap();
        let lifetime = Duration::from_secs(3);

        nudge(
            &store,
            &telegram,
            &Names::default(),
            DISPUTE,
            &message,
            &Entry::new(EntryKind::SerberoMediating, None, OPENED + 3),
            Nudge { lifetime },
        )
        .await
        .unwrap();

        assert_eq!(
            telegram.calls(),
            vec![Call::Nudge {
                chat_id: -100,
                reply_to: 7,
                text: "🔔 *Dispute* `96629381-bcb8-4d4f-8c66-e8f86f3e86ea`\n🤖 Serbero mediating"
                    .to_owned(),
                lifetime,
            }]
        );
    }
}
