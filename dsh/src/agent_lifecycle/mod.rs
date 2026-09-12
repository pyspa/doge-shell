//! Semantic lifecycle reporting for the shell's built-in AI agent.
//!
//! The agent (the `!` chat loop, the AI pipe, and the persistent `agent`
//! task runner) moves through three states from the outside world's point of
//! view: [`AgentState::Idle`] (no turn running), [`AgentState::Working`] (a
//! turn is running - planning, LLM calls, tool execution, retries, all of
//! it), and [`AgentState::Blocked`] (the turn cannot proceed without a human
//! decision). This module tracks those transitions and forwards them to a
//! pluggable [`LifecycleReporter`] backend, independent of any particular
//! external system.
//!
//! # Module structure
//!
//! - [`herdr`] - the first (and so far only) backend: reports state to
//!   [Herdr](https://herdr.dev), a terminal workspace manager, via its
//!   `pane report-agent`/`release-agent` CLI, when running inside a Herdr
//!   pane. A complete no-op everywhere else.
//!
//! # Design notes
//!
//! - **No `dsh-builtin`/`dsh-types` changes.** Everything here lives in the
//!   `dsh` crate; the three call sites that start a turn
//!   (`shell/eval.rs`'s `!` prefix, `repl/mod.rs`'s AI pipe, `agent.rs`'s
//!   persistent task) and the one that blocks on a human
//!   (`proxy/mod.rs`'s interactive approval) all already live there.
//! - **Sync trait, no `async-trait`.** The only genuinely slow part -
//!   running an external command - happens on a dedicated worker thread
//!   inside [`herdr::HerdrReporter`], not on the caller's task.
//! - **Ordering is guaranteed by a single worker thread, not by `--seq`
//!   alone.** Firing off one `thread::spawn` per report (as
//!   `dsh/src/history/command_history.rs` does for its own fire-and-forget
//!   dual-write) would let two independently-scheduled OS threads race to
//!   deliver, say, `Working` and a later `Blocked` to Herdr out of order;
//!   Herdr's own per-source high-water-mark rule would then silently drop
//!   whichever arrives with the smaller `--seq` - which, for `Blocked`, is
//!   the one event whose entire purpose is to summon a human. `--seq` still
//!   matters (it survives a `dsh` process restart in the same pane, since
//!   Herdr's high-water mark is not documented to reset on
//!   `release-agent`), but delivery order is what the single worker thread
//!   guarantees.

mod agent_command;
mod herdr;

#[cfg(test)]
mod tests;

use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Once, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The stable identity this integration reports under. Fixed, not
/// per-state: Herdr's own guidance is that `--source` must stay constant so
/// a builtin agent and any other agent CLI started from inside the same
/// pane never contend over the same identity.
pub(crate) const SOURCE: &str = "custom:doge-shell";
pub(crate) const AGENT_LABEL: &str = "dsh";

/// A human-readable state message is capped so a very long safety-guard
/// reason (or a pathological tool-call payload) never turns into a
/// multi-kilobyte `argv` entry.
const MAX_MESSAGE_CHARS: usize = 300;

/// Fetches this shell's current lifecycle manager. The one place that knows
/// the path down through `Environment` (`shell/eval.rs`'s `!` prefix,
/// `repl/mod.rs`'s AI pipe, `agent.rs`'s persistent task, `proxy/mod.rs`'s
/// interactive approval, and `agent.rs`'s `cancel`/`delete` all go through
/// this instead of repeating the field chain), so a future call site has one
/// line to copy rather than a chain to get right.
pub fn current(shell: &crate::shell::Shell) -> Arc<AgentLifecycleManager> {
    shell.environment.read().integration_state.lifecycle.clone()
}

/// Semantic lifecycle state of the built-in AI agent, independent of any
/// particular reporting backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentState {
    /// No turn is running; the agent can accept the next request.
    Idle,
    /// A turn is running: planning, LLM inference, tool selection/execution,
    /// retries, or final response generation. Everything between "the user
    /// submitted a request" and "the turn is over" is `Working` - this is
    /// not just "waiting on the LLM API".
    Working,
    /// The agent cannot proceed without a human decision (destructive-command
    /// approval, a permission request, an ambiguous choice). Carries a
    /// human-readable reason where one is available.
    Blocked(String),
}

fn sanitize_message(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > MAX_MESSAGE_CHARS {
        let mut truncated: String = collapsed.chars().take(MAX_MESSAGE_CHARS).collect();
        truncated.push('…');
        truncated
    } else {
        collapsed
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Turns [`AgentState`] transitions into calls to an external system.
///
/// Implementations must never block the caller for long (the intended shape
/// is "enqueue and return"; see [`herdr::HerdrReporter`]) and must never let
/// a failure escape as an `Err` - there is nowhere for one to go that
/// wouldn't fail an otherwise-healthy agent turn.
pub trait LifecycleReporter: Send + Sync {
    /// Report a state transition. `seq` is already strictly increasing
    /// relative to every previous call (including calls made by an earlier
    /// process instance reporting under the same source/pane).
    fn report(&self, state: &AgentState, seq: u64);
    /// Release lifecycle authority - the agent/session is exiting.
    fn release(&self, seq: u64);
    /// Best-effort drain of any queued work, bounded by `timeout`. Called at
    /// most once, from [`AgentLifecycleManager::shutdown`].
    fn shutdown(&self, timeout: Duration);
    /// Best-effort question used only for periodic reconciliation: does the
    /// backend believe it has actually delivered `desired`? The default (and
    /// [`NullReporter`]'s answer) is "yes, always" - i.e. no reconciliation.
    fn is_stale(&self, desired: &AgentState) -> bool {
        let _ = desired;
        false
    }
    /// Whether this backend is genuinely doing something, as opposed to
    /// [`NullReporter`] (which overrides this to `false`). Used only by
    /// [`reactivate_after_fork`] to decide whether a forked child needs to
    /// re-arm reporting for itself - fork() does not duplicate threads, so a
    /// backend that depends on one (like [`herdr::HerdrReporter`]'s worker)
    /// silently stops doing anything in the child even though its `Arc`
    /// survives.
    fn is_active(&self) -> bool {
        true
    }
}

/// No-op backend used whenever no external lifecycle integration is active.
/// Every method is a no-op, so callers never need an `if herdr_enabled`
/// branch of their own.
pub struct NullReporter;

impl LifecycleReporter for NullReporter {
    fn report(&self, _state: &AgentState, _seq: u64) {}
    fn release(&self, _seq: u64) {}
    fn shutdown(&self, _timeout: Duration) {}
    fn is_active(&self) -> bool {
        false
    }
}

struct ManagerState {
    /// `None` until the first report has actually gone out. Distinct from
    /// `Some(AgentState::Idle)`: seeding this with `Idle` directly made the
    /// very first `report_idle()` call at activation indistinguishable from
    /// a repeat of that same state, so `emit`'s dedup check silently
    /// swallowed it - Herdr was never told the source existed until some
    /// later, unrelated state change happened to occur first.
    last_reported: Option<AgentState>,
    last_seq: u64,
    /// Nesting depth of [`AgentLifecycleManager::begin_yield`]. While
    /// non-zero, this pane's authority has been handed to a foreground agent
    /// CLI: `emit` keeps tracking state locally (so the reclaim on the
    /// outermost guard's drop can resend it) but never calls into
    /// `reporter.report`, and `reconcile_if_stale` is a no-op - see
    /// [`AgentLifecycleManager::begin_yield`].
    yield_depth: usize,
}

/// Allocate the next `--seq` value for this manager, keeping it strictly
/// increasing across both wall-clock time and every previous call - the same
/// rule `emit` and `shutdown` already relied on before this was extracted so
/// `begin_yield`/`end_yield` could share it without duplicating the
/// `max(now_millis(), ..)` idiom a third time.
fn next_seq(state: &mut ManagerState) -> u64 {
    let seq = std::cmp::max(now_millis(), state.last_seq + 1);
    state.last_seq = seq;
    seq
}

/// Tracks the shell's current agent lifecycle state, deduplicates repeated
/// reports, keeps `--seq` strictly increasing, and forwards changes to a
/// [`LifecycleReporter`] backend.
pub struct AgentLifecycleManager {
    owner_pid: u32,
    state: Mutex<ManagerState>,
    /// Only the outermost `begin_turn()`/`TurnGuard` pair matters for
    /// deciding when a turn is truly over. The three production call sites
    /// never nest today, but this makes that an invariant rather than an
    /// assumption - see `dsh/src/agent.rs`'s `execute_chat_message` call,
    /// which could in principle be reached again through a tool-executed
    /// `!`/`agent resume` command.
    depth: AtomicUsize,
    reporter: Arc<dyn LifecycleReporter>,
    shutdown_once: Once,
}

impl AgentLifecycleManager {
    /// Shared by every constructor below so a future field only needs
    /// updating in one place instead of in every struct literal.
    fn with_seq(reporter: Arc<dyn LifecycleReporter>, last_seq: u64) -> Arc<Self> {
        Arc::new(Self {
            owner_pid: std::process::id(),
            state: Mutex::new(ManagerState {
                last_reported: None,
                last_seq,
                yield_depth: 0,
            }),
            depth: AtomicUsize::new(0),
            reporter,
            shutdown_once: Once::new(),
        })
    }

    pub fn new(reporter: Arc<dyn LifecycleReporter>) -> Arc<Self> {
        Self::with_seq(reporter, now_millis())
    }

    /// A manager backed by [`NullReporter`] - the default until (and unless)
    /// a real backend is activated.
    pub fn null() -> Arc<Self> {
        Self::new(Arc::new(NullReporter))
    }

    /// Test-only: construct a manager as if a prior instance's last used
    /// `seq` were already known, without depending on real wall-clock gaps
    /// between two managers built microseconds apart in the same test
    /// process (`now_millis()`-seeded construction can otherwise collide
    /// within the same millisecond).
    #[cfg(test)]
    fn new_with_seed_seq(reporter: Arc<dyn LifecycleReporter>, seed_seq: u64) -> Arc<Self> {
        Self::with_seq(reporter, seed_seq)
    }

    /// A manager that starts already yielded (`yield_depth == 1`, no initial
    /// report sent) - for a forked child continuing its parent's
    /// already-yielded pane authority rather than reclaiming authority the
    /// parent doesn't currently hold. See [`reactivate_after_fork`].
    fn new_yielded(reporter: Arc<dyn LifecycleReporter>) -> Arc<Self> {
        let manager = Self::new(reporter);
        manager.state.lock().yield_depth = 1;
        manager
    }

    /// Fork safety: a forked child that still holds a clone of this `Arc`
    /// from before the fork must never report or release on the parent
    /// shell's behalf using a backend the fork did not bring with it (see
    /// [`reactivate_after_fork`], which gives a forked child that keeps
    /// running Rust code - unlike `spawn_subshell`/`fork_process`'s
    /// external-command children, which call `std::process::exit` right
    /// after forking and so never reach a `TurnGuard`/`ShutdownGuard` drop at
    /// all - a manager of its own instead of relying on this guard to make
    /// the stale one usable).
    fn owned_by_this_process(&self) -> bool {
        std::process::id() == self.owner_pid
    }

    fn emit(&self, state: AgentState, force: bool) {
        if !self.owned_by_this_process() {
            return;
        }
        // Once `shutdown()` has run, this manager is done for good (it's
        // idempotent via `Once`, so a late `TurnGuard`/`YieldGuard` drop
        // after the interactive session has already torn down must not
        // re-claim authority this process just released).
        if self.shutdown_once.is_completed() {
            return;
        }
        let mut guard = self.state.lock();
        // `last_reported` starts `None`, not `Some(Idle)`: the very first
        // call for any state must always go through, or Herdr is never told
        // the source exists until some later, unrelated state change
        // happens to occur first.
        if !force && guard.last_reported.as_ref() == Some(&state) {
            return;
        }
        let seq = next_seq(&mut guard);
        guard.last_reported = Some(state.clone());
        // While this pane's authority is on loan to a foreground agent CLI
        // (`begin_yield`), keep tracking what we *would* report - so the
        // reclaim on `end_yield` can resend the current state - but never
        // actually call into the reporter. Reporting here would fight the
        // agent CLI (or Herdr's own detection of it) for the same pane.
        let yielded = guard.yield_depth > 0;
        drop(guard);
        if yielded {
            return;
        }
        self.reporter.report(&state, seq);
    }

    pub fn report_idle(&self) {
        self.emit(AgentState::Idle, false);
    }

    pub fn report_working(&self) {
        self.emit(AgentState::Working, false);
    }

    pub fn report_blocked(&self, reason: impl AsRef<str>) {
        self.emit(
            AgentState::Blocked(sanitize_message(reason.as_ref())),
            false,
        );
    }

    /// Begin a turn: reports `Working`, returns a guard that reports back to
    /// `Idle` when dropped - unless the turn ended `Blocked` (see
    /// [`TurnGuard`]'s doc comment).
    pub fn begin_turn(self: &Arc<Self>) -> TurnGuard {
        self.depth.fetch_add(1, Ordering::SeqCst);
        self.report_working();
        TurnGuard {
            manager: self.clone(),
        }
    }

    fn end_turn(&self) {
        // Only the outermost guard's drop ends the turn.
        if self.depth.fetch_sub(1, Ordering::SeqCst) != 1 {
            return;
        }
        // A turn that already ended in `Blocked` (the persistent task's
        // unattended `TaskStatus::InputRequired` case, reported explicitly
        // by `dsh/src/agent.rs` before this guard drops) must not be
        // silently overwritten with `Idle` - the whole point of reporting
        // it was to keep it visible after this turn (and often this
        // process) ends.
        let is_blocked = matches!(
            self.state.lock().last_reported,
            Some(AgentState::Blocked(_))
        );
        if !is_blocked {
            self.report_idle();
        }
    }

    /// Begin an interactive approval wait: reports `Blocked(reason)`,
    /// returns a guard that reports back to `Working` when dropped. Only
    /// meaningful while a turn is already active, which is always the case
    /// on the one call site this is used from (`dsh/src/proxy/mod.rs`'s
    /// synchronous `confirm_action` wait).
    pub fn begin_blocked(self: &Arc<Self>, reason: impl AsRef<str>) -> BlockedGuard {
        self.report_blocked(reason);
        BlockedGuard {
            manager: self.clone(),
        }
    }

    fn end_blocked(&self) {
        self.report_working();
    }

    /// Whether the backend is genuinely doing something (as opposed to
    /// [`NullReporter`]). [`yield_to_foreground_agent`] gates on this right
    /// after its one `environment.read()`, so starting an ordinary
    /// foreground command costs one lock acquisition plus this call when
    /// Herdr isn't active - no `state` lock, and no work past this point.
    pub fn is_active(&self) -> bool {
        self.reporter.is_active()
    }

    /// Whether this pane's authority is currently on loan to a foreground
    /// agent CLI. See [`Self::begin_yield`].
    pub fn is_yielded(&self) -> bool {
        self.state.lock().yield_depth > 0
    }

    /// Hand this pane's Herdr lifecycle authority to a foreground agent CLI
    /// this shell is about to run: Herdr's own screen detection is disabled
    /// for a pane for as long as an external integration (this one) holds
    /// its authority, so as long as dsh keeps it, starting `codex` or
    /// `claude` from inside dsh is never recognized by Herdr. Releasing it
    /// for the duration lets Herdr's built-in detection (or the agent CLI's
    /// own integration) classify the pane instead.
    ///
    /// Only the outermost call actually releases, and only the outermost
    /// guard's drop reclaims - see [`Self::end_yield`]. A complete no-op
    /// (`is_yielded()` stays `false`) for a [`NullReporter`] manager, one
    /// not owned by the current process (a stale post-fork clone; see
    /// [`Self::owned_by_this_process`]), or one that has already
    /// `shutdown()` (mirrors the same guard in [`Self::emit`], for the same
    /// reason: a release racing an emergency signal-triggered shutdown must
    /// not run after this process already considers itself torn down) -
    /// [`yield_to_foreground_agent`] also gates on [`Self::is_active`]
    /// before calling this, so the check here is a backstop, not the only
    /// thing standing between a `NullReporter` manager and a real `herdr`
    /// invocation.
    pub fn begin_yield(self: &Arc<Self>) -> YieldGuard {
        if self.owned_by_this_process()
            && self.reporter.is_active()
            && !self.shutdown_once.is_completed()
        {
            let mut guard = self.state.lock();
            guard.yield_depth += 1;
            let is_outermost = guard.yield_depth == 1;
            let seq = if is_outermost {
                Some(next_seq(&mut guard))
            } else {
                None
            };
            drop(guard);
            if let Some(seq) = seq {
                self.reporter.release(seq);
            }
        }
        YieldGuard {
            manager: self.clone(),
        }
    }

    fn end_yield(&self) {
        if !self.owned_by_this_process() {
            return;
        }
        let resend = {
            let mut guard = self.state.lock();
            if guard.yield_depth == 0 {
                // The common case, not a rare one: `begin_yield` only ever
                // incremented `yield_depth` when Herdr was active, owned by
                // this process, and not yet shut down (see its own guard) -
                // so for most sessions (Herdr inactive) every `YieldGuard`
                // reaches this branch. Also guards against underflow if
                // `end_yield` is ever somehow called more times than
                // `begin_yield` incremented.
                return;
            }
            guard.yield_depth -= 1;
            if guard.yield_depth == 0 {
                guard.last_reported.clone()
            } else {
                None
            }
        };
        // Reclaiming with whatever we most recently *would* have reported
        // (tracked by `emit` even while yielded) rather than unconditionally
        // `Idle`: if the agent CLI's own turn ran a nested `!`/`agent`
        // command that moved this shell to `Blocked`, that's what a human
        // still needs to see once authority comes back.
        if let Some(state) = resend {
            self.emit(state, true);
        }
    }

    /// Release lifecycle authority and give the backend a bounded window to
    /// flush any queued work. Idempotent - safe to call from both the normal
    /// end-of-session guard and an emergency signal-triggered shutdown.
    pub fn shutdown(&self) {
        if !self.owned_by_this_process() {
            return;
        }
        self.shutdown_once.call_once(|| {
            let seq = next_seq(&mut self.state.lock());
            self.reporter.release(seq);
            self.reporter.shutdown(Duration::from_millis(750));
        });
    }

    /// Best-effort periodic reconciliation: if the backend's last confirmed
    /// delivery doesn't match what we currently believe the state to be
    /// (e.g. a transient `herdr` failure dropped exactly the report that
    /// would have moved it off `Working`), resend the current state. Cheap
    /// to call often - `NullReporter::is_stale` always answers `false`, so
    /// this is a single method call and nothing else when Herdr isn't
    /// active. Intended to be called from the REPL's existing background
    /// tick, not a new timer.
    pub fn reconcile_if_stale(&self) {
        if !self.owned_by_this_process() {
            return;
        }
        // Authority is on loan to a foreground agent CLI: reconciling here
        // would resend our own last state and fight that CLI (or Herdr's
        // detection of it) for the pane - exactly what `begin_yield` exists
        // to prevent.
        if self.is_yielded() {
            return;
        }
        // Nothing has been reported yet (activation's own report is still
        // in flight, or Herdr just became active) - nothing to reconcile.
        let Some(desired) = self.state.lock().last_reported.clone() else {
            return;
        };
        if self.reporter.is_stale(&desired) {
            self.emit(desired, true);
        }
    }
}

/// RAII guard for an active turn. See [`AgentLifecycleManager::begin_turn`].
#[must_use]
pub struct TurnGuard {
    manager: Arc<AgentLifecycleManager>,
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.manager.end_turn();
    }
}

/// RAII guard for an active interactive approval wait. See
/// [`AgentLifecycleManager::begin_blocked`].
#[must_use]
pub struct BlockedGuard {
    manager: Arc<AgentLifecycleManager>,
}

impl Drop for BlockedGuard {
    fn drop(&mut self) {
        self.manager.end_blocked();
    }
}

/// RAII guard for a foreground agent CLI holding this pane's Herdr
/// authority. See [`AgentLifecycleManager::begin_yield`] and
/// [`yield_to_foreground_agent`].
#[must_use]
pub struct YieldGuard {
    manager: Arc<AgentLifecycleManager>,
}

impl Drop for YieldGuard {
    fn drop(&mut self) {
        self.manager.end_yield();
    }
}

/// Guarantees [`AgentLifecycleManager::shutdown`] runs when the interactive
/// session's function scope ends - normal return, `?`, or unwinding from a
/// panic anywhere in its call tree. This shell doesn't set `panic =
/// "abort"`, so `Drop` impls on the unwinding stack still run for the
/// double-Ctrl-C exit idiom (`panic!("Shell terminated by double Ctrl+C")`)
/// and for a genuine bug alike.
///
/// Deliberately a plain local guard, not tied to `Repl`'s own `Drop` impl:
/// that one fires dozens of times across unrelated unit tests
/// (`dsh/src/repl/mod.rs` says so explicitly), which would either spawn
/// spurious `herdr` invocations during `cargo test` or need a test-mode gate
/// bolted on for no reason. This guard is only ever constructed once, from
/// `run_interactive`.
#[must_use]
pub struct ShutdownGuard(Arc<AgentLifecycleManager>);

impl ShutdownGuard {
    pub fn new(manager: Arc<AgentLifecycleManager>) -> Self {
        Self(manager)
    }
}

impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

/// Process-wide marker so a `dsh` launched from inside another `dsh` (in the
/// same Herdr pane, inheriting `HERDR_PANE_ID`) never contends with its
/// parent for lifecycle authority. Set once, read at most once per process;
/// a `OnceLock` rather than re-reading the environment everywhere this
/// matters keeps the "have we already decided" check itself allocation-free.
static ACTIVATION: OnceLock<()> = OnceLock::new();

/// Activate Herdr reporting for this shell session, if this process is
/// running inside a Herdr pane and no ancestor `dsh` in the same pane has
/// already claimed it.
///
/// On success, publishes the nested-dsh owner marker two ways: as a real
/// process environment variable (`unsafe { std::env::set_var }` - so any
/// child spawned via a plain `Command::new`, such as the AI `execute` tool's
/// `sh -c` or `ShellProxy::capture_command`, inherits it through ordinary OS
/// env inheritance) *and* as the returned `(key, value)` pair, which the
/// caller must additionally push through `Environment::set_system_env_var`
/// (so a child spawned through this shell's own external-command path,
/// which builds each child's `envp` explicitly from its own snapshot in
/// `dsh/src/process/process.rs` rather than the live process environment,
/// sees it too). Neither alone reaches every spawning path this shell has.
pub fn activate() -> (Arc<AgentLifecycleManager>, Option<(&'static str, String)>) {
    // Idempotency guard: `run_interactive` is the only call site, and it
    // only runs once per process, but this keeps that an invariant rather
    // than an assumption if a future caller is added.
    if ACTIVATION.set(()).is_err() {
        return (AgentLifecycleManager::null(), None);
    }
    match herdr::HerdrEnv::detect() {
        Some(env) => {
            let reporter = herdr::HerdrReporter::spawn(env, SOURCE, AGENT_LABEL);
            let manager = AgentLifecycleManager::new(reporter);
            manager.report_idle();
            let pid = std::process::id().to_string();
            // SAFETY: called once, early in `run_interactive`, before any
            // additional thread that itself reads/writes the process
            // environment has been spawned.
            unsafe { std::env::set_var(herdr::OWNER_ENV, &pid) };
            (manager, Some((herdr::OWNER_ENV, pid)))
        }
        None => (AgentLifecycleManager::null(), None),
    }
}

/// Give a forked child that will keep running Rust code (not one that
/// immediately `exec`s or calls `std::process::exit` right after forking) a
/// working reporter of its own.
///
/// `fork()` only duplicates the calling thread: a backend that depends on a
/// background worker (`herdr::HerdrReporter`) has no worker at all in the
/// child, even though its `Arc` and `mpsc::Sender` survive - every report
/// would just queue forever into a channel nothing drains. This matters
/// because `dsh/src/process/fork.rs::fork_builtin_process` forks a
/// backgrounded builtin (`agent run ... -- goal &`) and then runs the
/// *entire* builtin, including any AI turns, to completion in the child -
/// exactly the flagship scenario this feature exists for.
///
/// A no-op (one boolean check) unless Herdr was active in the parent before
/// the fork. Re-detects directly, bypassing both the `ACTIVATION` guard
/// (specific to `run_interactive`'s once-per-process semantics) and the
/// nested-dsh owner-marker exclusion: this child is the same session's own
/// lifecycle authority continuing, not a competing shell trying to claim it.
///
/// The caller must still explicitly call `shutdown()` on
/// `Environment.integration_state.lifecycle` before the child actually
/// exits - `std::process::exit` skips destructors entirely, so
/// `ShutdownGuard`'s own `Drop` never runs in a forked child.
///
/// If the parent had yielded pane authority to a foreground agent CLI at
/// the moment of the fork (`yield_to_foreground_agent`), the child
/// continues that same yield rather than reclaiming authority the parent
/// itself doesn't currently hold.
pub fn reactivate_after_fork(shell: &mut crate::shell::Shell) {
    let (was_active, was_yielded) = {
        let env = shell.environment.read();
        let lifecycle = &env.integration_state.lifecycle;
        (lifecycle.is_active(), lifecycle.is_yielded())
    };
    if !was_active {
        return;
    }
    let Some(env) = herdr::HerdrEnv::detect_for_forked_child() else {
        return;
    };
    let reporter = herdr::HerdrReporter::spawn(env, SOURCE, AGENT_LABEL);
    let manager = if was_yielded {
        AgentLifecycleManager::new_yielded(reporter)
    } else {
        let manager = AgentLifecycleManager::new(reporter);
        manager.report_idle();
        manager
    };
    shell.environment.write().integration_state.lifecycle = manager;
}

/// If the foreground job about to run is a Herdr-recognized agent CLI, hand
/// this pane's Herdr lifecycle authority to it for the duration - see
/// [`AgentLifecycleManager::begin_yield`] for why that's needed at all
/// (Herdr's own screen detection is disabled for a pane for as long as an
/// external integration holds its authority, so as long as dsh keeps it,
/// starting `codex` or `claude` from inside dsh is never recognized by
/// Herdr).
///
/// `interactive`/`foreground` are explicit arguments rather than read off
/// `job` because `fg` resumes a job whose own `job.foreground` stays
/// `false` (it was backgrounded) - see
/// `dsh/src/proxy/builtin/jobs.rs::execute_fg`, the other call site, which
/// passes `true` directly for exactly this reason. Only the pipeline's last
/// external command is inspected, matching
/// `terminal::title::last_external_command_basename`'s rule: a command that
/// only feeds a pipe never draws to the terminal for Herdr's screen
/// detection to see either, so yielding for it would only cost this shell
/// its own authority for nothing in return.
///
/// A complete no-op (`None`, no lock taken beyond
/// [`AgentLifecycleManager::is_active`]) whenever Herdr isn't active, the
/// job isn't a foreground external command, the handoff feature is disabled
/// (`DSH_HERDR_AGENT_HANDOFF`), or the job's last pipeline stage isn't a
/// recognized agent CLI (`DSH_HERDR_AGENT_COMMANDS`).
pub fn yield_to_foreground_agent(
    shell: &crate::shell::Shell,
    job: &crate::process::Job,
    interactive: bool,
    foreground: bool,
) -> Option<YieldGuard> {
    if !interactive || !foreground || job.capture_output || !job.struct_pipe_exprs.is_empty() {
        return None;
    }
    // One `environment.read()` for everything this needs (the manager
    // clone and both settings) rather than `current(shell)`'s own lock plus
    // a second one here - every foreground job pays this, so it should
    // cost at most one lock acquisition even when it turns out not to be a
    // recognized agent CLI.
    let env = shell.environment.read();
    let manager = env.integration_state.lifecycle.clone();
    if !manager.is_active() {
        return None;
    }
    if !agent_command::handoff_enabled(env.get_var(agent_command::HANDOFF_KEY).as_deref()) {
        return None;
    }
    let name = crate::terminal::title::last_external_command_basename(job)?;
    let configured = env.get_var(agent_command::AGENT_COMMANDS_KEY);
    if !agent_command::is_agent_command(&name, configured.as_deref()) {
        return None;
    }
    drop(env);
    Some(manager.begin_yield())
}
