use super::AiEvent;
use super::background_io::BackgroundIoEvent;
use crate::scheduler::SchedulerEvent;
use crossterm::event::{Event, EventStream};
use futures::StreamExt;
use std::future::pending;
use std::io;
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, Interval, MissedTickBehavior, Sleep, interval_at};

const BACKGROUND_REFRESH_MS: u64 = 1000;
const EXPLANATION_REFRESH_MS: u64 = 200;
const INITIAL_EXPLANATION_IDLE_SECS: u64 = 5;

#[derive(Debug)]
pub(crate) enum LoopEvent {
    BackgroundTick,
    AiRefreshTick,
    ExplanationRefreshTick,
    ExplanationIdle,
    GitRefresh,
    Scheduler(SchedulerEvent),
    CompletionRefresh,
    Ai(AiEvent),
    BackgroundIo(BackgroundIoEvent),
    TerminalInput(Event),
    TerminalError(io::Error),
    TerminalClosed,
}

pub(crate) struct ReplEventLoop {
    reader: Option<EventStream>,
    background: Interval,
    ai_refresh: Interval,
    explanation_refresh: Interval,
    idle_sleep: Pin<Box<Sleep>>,
    git_rx: UnboundedReceiver<()>,
    sched_rx: UnboundedReceiver<SchedulerEvent>,
    completion_rx: UnboundedReceiver<()>,
    ai_rx: UnboundedReceiver<AiEvent>,
    background_io_rx: UnboundedReceiver<BackgroundIoEvent>,
}

impl ReplEventLoop {
    pub fn new(
        ai_refresh_ms: u64,
        git_rx: UnboundedReceiver<()>,
        sched_rx: UnboundedReceiver<SchedulerEvent>,
        completion_rx: UnboundedReceiver<()>,
        ai_rx: UnboundedReceiver<AiEvent>,
        background_io_rx: UnboundedReceiver<BackgroundIoEvent>,
    ) -> Self {
        Self {
            reader: None,
            background: skipping_interval(BACKGROUND_REFRESH_MS),
            ai_refresh: skipping_interval(ai_refresh_ms),
            explanation_refresh: skipping_interval(EXPLANATION_REFRESH_MS),
            idle_sleep: Box::pin(tokio::time::sleep(Duration::from_secs(
                INITIAL_EXPLANATION_IDLE_SECS,
            ))),
            git_rx,
            sched_rx,
            completion_rx,
            ai_rx,
            background_io_rx,
        }
    }

    pub async fn next_event(&mut self) -> LoopEvent {
        tokio::select! {
            _ = self.background.tick() => LoopEvent::BackgroundTick,
            _ = self.ai_refresh.tick() => LoopEvent::AiRefreshTick,
            _ = self.explanation_refresh.tick() => LoopEvent::ExplanationRefreshTick,
            _ = self.idle_sleep.as_mut() => LoopEvent::ExplanationIdle,
            Some(()) = self.git_rx.recv() => LoopEvent::GitRefresh,
            Some(event) = self.sched_rx.recv() => LoopEvent::Scheduler(event),
            Some(()) = self.completion_rx.recv() => LoopEvent::CompletionRefresh,
            Some(event) = self.ai_rx.recv() => LoopEvent::Ai(event),
            Some(event) = self.background_io_rx.recv() => LoopEvent::BackgroundIo(event),
            event = next_terminal_event(&mut self.reader) => match event {
                Some(Ok(event)) => LoopEvent::TerminalInput(event),
                Some(Err(error)) => LoopEvent::TerminalError(error),
                None => LoopEvent::TerminalClosed,
            },
        }
    }

    pub fn reset_idle(&mut self, duration: Duration) {
        self.idle_sleep.as_mut().reset(Instant::now() + duration);
    }

    pub fn pause_input(&mut self) {
        self.reader = None;
    }

    pub fn resume_input(&mut self) {
        self.reader = Some(EventStream::new());
    }

    #[cfg(test)]
    pub async fn recv_ai(&mut self) -> Option<AiEvent> {
        self.ai_rx.recv().await
    }
}

async fn next_terminal_event(reader: &mut Option<EventStream>) -> Option<Result<Event, io::Error>> {
    match reader {
        Some(reader) => reader.next().await,
        None => pending().await,
    }
}

fn skipping_interval(period_ms: u64) -> Interval {
    let period = Duration::from_millis(period_ms);
    let mut interval = interval_at(Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::timeout;

    struct EventLoopFixture {
        event_loop: ReplEventLoop,
        git_tx: tokio::sync::mpsc::UnboundedSender<()>,
        sched_tx: tokio::sync::mpsc::UnboundedSender<SchedulerEvent>,
        completion_tx: tokio::sync::mpsc::UnboundedSender<()>,
        ai_tx: tokio::sync::mpsc::UnboundedSender<AiEvent>,
        background_io_tx: tokio::sync::mpsc::UnboundedSender<BackgroundIoEvent>,
    }

    fn event_loop() -> EventLoopFixture {
        let (git_tx, git_rx) = tokio::sync::mpsc::unbounded_channel();
        let (sched_tx, sched_rx) = tokio::sync::mpsc::unbounded_channel();
        let (completion_tx, completion_rx) = tokio::sync::mpsc::unbounded_channel();
        let (ai_tx, ai_rx) = tokio::sync::mpsc::unbounded_channel();
        let (background_io_tx, background_io_rx) = tokio::sync::mpsc::unbounded_channel();
        EventLoopFixture {
            event_loop: ReplEventLoop::new(
                60_000,
                git_rx,
                sched_rx,
                completion_rx,
                ai_rx,
                background_io_rx,
            ),
            git_tx,
            sched_tx,
            completion_tx,
            ai_tx,
            background_io_tx,
        }
    }

    #[tokio::test]
    async fn channels_are_mapped_to_typed_loop_events() {
        let EventLoopFixture {
            mut event_loop,
            git_tx,
            sched_tx,
            completion_tx,
            ai_tx,
            background_io_tx,
        } = event_loop();

        git_tx.send(()).unwrap();
        assert!(matches!(
            timeout(Duration::from_millis(50), event_loop.next_event())
                .await
                .unwrap(),
            LoopEvent::GitRefresh
        ));

        completion_tx.send(()).unwrap();
        assert!(matches!(
            timeout(Duration::from_millis(50), event_loop.next_event())
                .await
                .unwrap(),
            LoopEvent::CompletionRefresh
        ));

        sched_tx
            .send(SchedulerEvent {
                id: 1,
                name: "build".to_string(),
                command: "cargo build".to_string(),
                cwd: "/work".to_string(),
                stdout: String::new(),
                stderr: String::new(),
                exit_code: 0,
                duration: Duration::from_millis(10),
                timed_out: false,
                changed: false,
                notify: true,
            })
            .unwrap();
        assert!(matches!(
            timeout(Duration::from_millis(50), event_loop.next_event())
                .await
                .unwrap(),
            LoopEvent::Scheduler(SchedulerEvent { id: 1, .. })
        ));

        ai_tx
            .send(AiEvent::AutoFix(crate::repl::AutoFixSuggestion {
                replacement: "fix".to_string(),
                title: None,
                kind: crate::repl::AutoFixKind::QuickFix,
                command_time: None,
            }))
            .unwrap();
        assert!(matches!(
            timeout(Duration::from_millis(50), event_loop.next_event())
                .await
                .unwrap(),
            LoopEvent::Ai(AiEvent::AutoFix(fix)) if fix.replacement == "fix"
        ));

        background_io_tx
            .send(BackgroundIoEvent::TimingSaved {
                generation: 0,
                dirty_records: 0,
                result: Ok(crate::command_timing::TimingWriteOutcome::Written),
            })
            .unwrap();
        assert!(matches!(
            timeout(Duration::from_millis(50), event_loop.next_event())
                .await
                .unwrap(),
            LoopEvent::BackgroundIo(BackgroundIoEvent::TimingSaved { .. })
        ));
    }

    #[tokio::test]
    async fn timer_is_mapped_to_background_tick() {
        let EventLoopFixture { mut event_loop, .. } = event_loop();
        event_loop.background = skipping_interval(1);

        assert!(matches!(
            timeout(Duration::from_millis(50), event_loop.next_event())
                .await
                .unwrap(),
            LoopEvent::BackgroundTick
        ));
    }

    #[test]
    fn terminal_input_is_representable_without_touching_the_terminal() {
        let event = LoopEvent::TerminalInput(Event::Resize(120, 40));
        assert!(matches!(
            event,
            LoopEvent::TerminalInput(Event::Resize(120, 40))
        ));
    }
}
