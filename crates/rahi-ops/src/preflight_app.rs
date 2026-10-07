//! A cell's own preflight checks (spec 049).
//!
//! `preflight` answers "would `serve` start here" for the chassis; a cell
//! answers it for itself through [`AppCheck`]s it declares. Rahi runs them
//! after its own twelve, against a read-only view of the opened store, the
//! parsed manifest and the verb's environment, one at a time and bounded,
//! and prints them as `app.<name>` lines. Rahi interprets none of the cell's
//! configuration: a check reads its own variables and decides.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rahi_kernel::{CapabilityKind, Manifest};
use rahi_store::{Page, StoreHandle, Value};
use rahi_types::{EnvReader, Error, Result};
use serde::de::DeserializeOwned;

use crate::preflight::Verdict;

/// The most checks one cell may declare (B-2).
pub const MAX_APP_CHECKS: usize = 32;
/// The longest check name (B-2).
pub const MAX_NAME_LEN: usize = 48;
/// The bound on one check (B-6).
pub const CHECK_BOUND: Duration = Duration::from_secs(5);
/// The bound on every app check together (B-6).
pub const PHASE_BOUND: Duration = Duration::from_secs(30);
/// The longest detail or report line printed, in characters (B-7).
pub const MAX_LINE_CHARS: usize = 240;
/// The most report lines printed for one check (B-7).
pub const MAX_REPORT_LINES: usize = 32;

/// The future one check returns.
pub type CheckFuture = Pin<Box<dyn Future<Output = AppVerdict> + Send + 'static>>;

type CheckFn = dyn Fn(PreflightContext) -> CheckFuture + Send + Sync;

/// One named check a cell declares (B-1).
#[derive(Clone)]
pub struct AppCheck {
    name: String,
    run: Arc<CheckFn>,
}

impl AppCheck {
    /// A check named `name` that runs `check` against the context.
    ///
    /// The name is validated when preflight runs, not here, so a cell's
    /// declaration never panics (B-2).
    pub fn new<F, Fut>(name: impl Into<String>, check: F) -> Self
    where
        F: Fn(PreflightContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = AppVerdict> + Send + 'static,
    {
        Self {
            name: name.into(),
            run: Arc::new(move |ctx| Box::pin(check(ctx))),
        }
    }

    /// The name, as declared.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Debug for AppCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppCheck")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// The store, for reading only (B-3).
///
/// It offers spec 011's three query reads and nothing else: no write, no
/// transaction, no migration, no shutdown, and no way back to the handle.
///
/// ```
/// # async fn reads(view: rahi_ops::preflight_app::StoreView) {
/// let _: Vec<(i64,)> = view.query("SELECT 1", vec![]).await.unwrap();
/// # }
/// ```
///
/// ```compile_fail
/// # async fn writes(view: rahi_ops::preflight_app::StoreView) {
/// let _ = view.execute("DELETE FROM t", vec![]).await;
/// # }
/// ```
///
/// ```compile_fail
/// # async fn transacts(view: rahi_ops::preflight_app::StoreView) {
/// let _ = view.txn(vec![]).await;
/// # }
/// ```
///
/// ```compile_fail
/// # fn reaches(view: rahi_ops::preflight_app::StoreView) {
/// let _: rahi_store::StoreHandle = view.handle;
/// # }
/// ```
///
/// ```compile_fail
/// # async fn derefs(view: rahi_ops::preflight_app::StoreView) {
/// let _ = view.shutdown().await;
/// # }
/// ```
#[derive(Clone)]
pub struct StoreView {
    handle: StoreHandle,
}

impl StoreView {
    pub(crate) const fn new(handle: StoreHandle) -> Self {
        Self { handle }
    }

    /// Read the local replica (spec 011's `query`).
    ///
    /// # Errors
    ///
    /// As `StoreHandle::query`.
    pub async fn query<T>(
        &self,
        sql: impl Into<Cow<'static, str>>,
        values: Vec<Value>,
    ) -> Result<Vec<T>>
    where
        T: DeserializeOwned + Send + 'static,
    {
        self.handle.query(sql, values).await
    }

    /// Read through the leader (spec 011's `query_consistent`).
    ///
    /// # Errors
    ///
    /// As `StoreHandle::query_consistent`.
    pub async fn query_consistent<T>(
        &self,
        sql: impl Into<Cow<'static, str>>,
        values: Vec<Value>,
    ) -> Result<Vec<T>>
    where
        T: DeserializeOwned,
    {
        self.handle.query_consistent(sql, values).await
    }

    /// Read one page (spec 011's `query_paged`).
    ///
    /// # Errors
    ///
    /// As `StoreHandle::query_paged`.
    pub async fn query_paged<T>(&self, sql: &str, values: Vec<Value>, page: Page) -> Result<Vec<T>>
    where
        T: DeserializeOwned + Send + 'static,
    {
        self.handle.query_paged(sql, values, page).await
    }
}

impl fmt::Debug for StoreView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoreView")
    }
}

/// The verb's environment, owned so a check's future may outlive the
/// verb's borrow of it (B-3, D-9).
#[derive(Clone, Debug, Default)]
pub struct AppEnv(BTreeMap<String, String>);

impl EnvReader for AppEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }

    fn snapshot(&self) -> Option<BTreeMap<String, String>> {
        Some(self.0.clone())
    }
}

/// What one check is given (B-3): the store to read, the manifest, and the
/// verb's environment.
#[derive(Clone, Debug)]
pub struct PreflightContext {
    store: StoreView,
    manifest: Arc<Manifest>,
    env: Arc<AppEnv>,
}

impl PreflightContext {
    /// The opened app store, read-only.
    #[must_use]
    pub const fn store(&self) -> &StoreView {
        &self.store
    }

    /// The cell's parsed manifest; ask it [`Manifest::covers`].
    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The environment the verb was given.
    #[must_use]
    pub fn env(&self) -> &AppEnv {
        &self.env
    }
}

/// How a check ended (B-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppOutcome {
    /// It holds.
    Pass,
    /// It holds, with something the operator should see; does not fail
    /// preflight.
    Warn,
    /// It does not hold; fails preflight.
    Fail,
}

/// One check's verdict: an outcome, a one-line detail, and report lines
/// (B-4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppVerdict {
    /// How it ended.
    pub outcome: AppOutcome,
    /// What was observed.
    pub detail: String,
    /// More to show the operator, one line each.
    pub report: Vec<String>,
}

impl AppVerdict {
    fn of(outcome: AppOutcome, detail: impl Into<String>) -> Self {
        Self {
            outcome,
            detail: detail.into(),
            report: Vec::new(),
        }
    }

    /// The check holds.
    #[must_use]
    pub fn pass(detail: impl Into<String>) -> Self {
        Self::of(AppOutcome::Pass, detail)
    }

    /// The check holds with a warning.
    #[must_use]
    pub fn warn(detail: impl Into<String>) -> Self {
        Self::of(AppOutcome::Warn, detail)
    }

    /// The check fails.
    #[must_use]
    pub fn fail(detail: impl Into<String>) -> Self {
        Self::of(AppOutcome::Fail, detail)
    }

    /// The check fails because the manifest ceiling grants `service` no
    /// `kind` on `resource`, in the one fixed wording (B-4).
    #[must_use]
    pub fn capability_missing(
        service: impl fmt::Display,
        kind: CapabilityKind,
        resource: impl fmt::Display,
    ) -> Self {
        Self::fail(format!(
            "capability {kind} on {resource} for service {service} is not in the manifest ceiling"
        ))
    }

    /// The same verdict with `lines` appended to its report.
    #[must_use]
    pub fn with_report<I, S>(mut self, lines: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.report.extend(lines.into_iter().map(Into::into));
        self
    }
}

/// One printed app line and its report lines (B-7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppLine {
    /// `app.<name>`, or `app` for a refused declaration.
    pub name: String,
    /// What happened.
    pub verdict: Verdict,
    /// The report lines, sanitized and capped.
    pub report: Vec<String>,
}

impl AppLine {
    fn new(name: &str, verdict: Verdict, report: &[String]) -> Self {
        let verdict = match verdict {
            Verdict::Pass(d) => Verdict::Pass(sanitize(&d)),
            Verdict::Warn(d) => Verdict::Warn(sanitize(&d)),
            Verdict::Fail(d) => Verdict::Fail(sanitize(&d)),
            Verdict::Skipped(d) => Verdict::Skipped(sanitize(&d)),
        };
        let mut lines: Vec<String> = report
            .iter()
            .take(MAX_REPORT_LINES)
            .map(|line| sanitize(line))
            .collect();
        if report.len() > MAX_REPORT_LINES {
            lines.push(format!(
                "({} more lines omitted)",
                report.len() - MAX_REPORT_LINES
            ));
        }
        Self {
            name: name.to_owned(),
            verdict,
            report: lines,
        }
    }

    fn check(name: &str, verdict: Verdict) -> Self {
        Self::new(&format!("app.{name}"), verdict, &[])
    }

    /// The line did not fail.
    #[must_use]
    pub const fn ok(&self) -> bool {
        !matches!(self.verdict, Verdict::Fail(_))
    }
}

impl fmt::Display for AppLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.verdict.line(&self.name))?;
        for line in &self.report {
            write!(f, "\n  | {line}")?;
        }
        Ok(())
    }
}

/// Control characters become `?`, and the line is cut at
/// [`MAX_LINE_CHARS`] characters (B-7).
#[must_use]
pub fn sanitize(line: &str) -> String {
    line.chars()
        .take(MAX_LINE_CHARS)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Why a declaration is refused, if it is (B-2).
///
/// # Errors
///
/// [`Error::Validation`] naming the count, or the first invalid or
/// duplicate name.
pub fn validate(checks: &[AppCheck]) -> Result<()> {
    if checks.len() > MAX_APP_CHECKS {
        return Err(Error::Validation(format!(
            "{} checks are declared; at most {MAX_APP_CHECKS} are allowed",
            checks.len()
        )));
    }
    let mut seen = BTreeSet::new();
    for check in checks {
        let name = check.name();
        if !valid_name(name) {
            return Err(Error::Validation(format!(
                "check name {:?} is not [a-z][a-z0-9_]{{0,47}}",
                sanitize(name)
            )));
        }
        if !seen.insert(name) {
            return Err(Error::Validation(format!(
                "check name {name:?} is declared twice"
            )));
        }
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= MAX_NAME_LEN
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// What a ready app phase reads.
pub(crate) struct Ready {
    pub(crate) store: StoreHandle,
    pub(crate) manifest: Manifest,
    pub(crate) env: BTreeMap<String, String>,
}

/// What the app phase is given by the chassis checks before it (B-5).
pub(crate) enum Ground {
    /// Every app check may run.
    Ready(Box<Ready>),
    /// No app check runs; each is reported skipped for this reason.
    Skip(String),
    /// No app check runs, and the phase fails with this reason.
    Refuse(String),
}

/// The app phase: validate, then run each check in order under its bounds
/// (B-2, B-5, B-6). Every spawned task is finished or aborted, and joined,
/// before this returns, so the caller may shut the store down after it.
pub(crate) async fn run(checks: Vec<AppCheck>, ground: Ground) -> Vec<AppLine> {
    if checks.is_empty() {
        return Vec::new();
    }
    if let Err(err) = validate(&checks) {
        return vec![AppLine::new(
            "app",
            Verdict::Fail(err.message().to_owned()),
            &[],
        )];
    }
    let ctx = match ground {
        Ground::Ready(ready) => PreflightContext {
            store: StoreView::new(ready.store),
            manifest: Arc::new(ready.manifest),
            env: Arc::new(AppEnv(ready.env)),
        },
        Ground::Skip(because) => {
            return checks
                .iter()
                .map(|c| AppLine::check(c.name(), Verdict::Skipped(format!("skipped: {because}"))))
                .collect();
        }
        Ground::Refuse(because) => {
            return vec![AppLine::new("app", Verdict::Fail(because), &[])];
        }
    };

    let started = Instant::now();
    let mut lines = Vec::with_capacity(checks.len());
    for check in checks {
        let left = PHASE_BOUND.saturating_sub(started.elapsed());
        if left.is_zero() {
            lines.push(AppLine::check(
                check.name(),
                Verdict::Skipped("skipped: the app check budget is spent".to_owned()),
            ));
            continue;
        }
        let bound = left.min(CHECK_BOUND);
        let mut task = tokio::spawn((check.run)(ctx.clone()));
        let line = match tokio::time::timeout(bound, &mut task).await {
            Ok(Ok(verdict)) => {
                let detail = verdict.detail;
                let v = match verdict.outcome {
                    AppOutcome::Pass => Verdict::Pass(detail),
                    AppOutcome::Warn => Verdict::Warn(detail),
                    AppOutcome::Fail => Verdict::Fail(detail),
                };
                AppLine::new(&format!("app.{}", check.name()), v, &verdict.report)
            }
            Ok(Err(joined)) if joined.is_panic() => {
                AppLine::check(check.name(), Verdict::Fail("panicked".to_owned()))
            }
            Ok(Err(_)) => AppLine::check(check.name(), Verdict::Fail("cancelled".to_owned())),
            Err(_) => {
                task.abort();
                let _ = task.await;
                let detail = if bound == CHECK_BOUND {
                    format!("timed out after {}s", CHECK_BOUND.as_secs())
                } else {
                    "timed out: the app check budget is spent".to_owned()
                };
                AppLine::check(check.name(), Verdict::Fail(detail))
            }
        };
        lines.push(line);
    }
    lines
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn named(name: &str) -> AppCheck {
        AppCheck::new(name, |_| async { AppVerdict::pass("ok") })
    }

    #[test]
    fn names_are_lowercase_identifiers_up_to_48() {
        for good in ["a", "embedding_model", "x1", &"a".repeat(48)] {
            assert!(validate(&[named(good)]).is_ok(), "{good}");
        }
        for bad in ["", "A", "1a", "_a", "a-b", "a.b", "é", &"a".repeat(49)] {
            assert!(validate(&[named(bad)]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_duplicate_and_a_33rd_check_are_refused() {
        let err = validate(&[named("a"), named("b"), named("a")]).unwrap_err();
        assert!(err.message().contains("\"a\" is declared twice"), "{err}");
        let many: Vec<_> = (0..33).map(|i| named(&format!("c{i}"))).collect();
        let err = validate(&many).unwrap_err();
        assert!(err.message().contains("33 checks"), "{err}");
        assert!(validate(&many[..32]).is_ok());
    }

    #[test]
    fn sanitize_replaces_controls_and_cuts_at_240() {
        assert_eq!(sanitize("a\nb\tc\u{7}"), "a?b?c?");
        assert_eq!(sanitize(&"é".repeat(300)).chars().count(), 240);
    }

    #[test]
    fn the_capability_wording_is_fixed() {
        let v = AppVerdict::capability_missing("embed", CapabilityKind::HttpEgress, "api.example");
        assert_eq!(v.outcome, AppOutcome::Fail);
        assert_eq!(
            v.detail,
            "capability http.egress on api.example for service embed is not in the manifest ceiling"
        );
    }

    #[test]
    fn report_lines_are_capped_with_a_count() {
        let report: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let line = AppLine::new("app.x", Verdict::Pass("ok".to_owned()), &report);
        assert_eq!(line.report.len(), 33);
        assert_eq!(line.report[32], "(8 more lines omitted)");
        let printed = line.to_string();
        assert!(
            printed.starts_with("PASS app.x: ok\n  | line 0\n"),
            "{printed}"
        );
        assert!(
            printed.ends_with("\n  | (8 more lines omitted)"),
            "{printed}"
        );
    }
}
