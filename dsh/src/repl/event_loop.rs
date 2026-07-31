use std::pin::Pin;
use std::time::Duration;
use tokio::time::{Instant, Interval, MissedTickBehavior, Sleep, interval_at};

const BACKGROUND_REFRESH_MS: u64 = 1000;
const EXPLANATION_REFRESH_MS: u64 = 200;
const INITIAL_EXPLANATION_IDLE_SECS: u64 = 5;

pub(crate) struct ReplEventLoop {
    pub background: Interval,
    pub ai_refresh: Interval,
    pub explanation_refresh: Interval,
    pub idle_sleep: Pin<Box<Sleep>>,
}

impl ReplEventLoop {
    pub fn new(ai_refresh_ms: u64) -> Self {
        Self {
            background: skipping_interval(BACKGROUND_REFRESH_MS),
            ai_refresh: skipping_interval(ai_refresh_ms),
            explanation_refresh: skipping_interval(EXPLANATION_REFRESH_MS),
            idle_sleep: Box::pin(tokio::time::sleep(Duration::from_secs(
                INITIAL_EXPLANATION_IDLE_SECS,
            ))),
        }
    }

    pub fn reset_idle(&mut self, duration: Duration) {
        self.idle_sleep.as_mut().reset(Instant::now() + duration);
    }
}

fn skipping_interval(period_ms: u64) -> Interval {
    let period = Duration::from_millis(period_ms);
    let mut interval = interval_at(Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    interval
}
