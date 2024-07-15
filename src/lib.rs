use std::{
    future::Future,
    pin::Pin,
    task::{ready, Context, Poll},
    time::Duration,
};

use tokio::time::{Instant, Sleep};

/// Timer for a limited set of events that are represented by their ordinals.
/// It multiplexes over a single tokio [Sleep] instance.
/// Deadlines for the same event are coalesced to the sooner one if it has not yet fired.
///
/// Deadlines are stored on a stack-allocated array of size `N`, and the ordinals are used to index into it,
/// so the maximum supported ordinal will be `N - 1`. The implementation is designed for small `N` (think single digits).
///
/// Mapping between ordinals and events is up to the user.
#[derive(Debug)]
pub struct MuxTimer<const N: usize> {
    deadlines: [Option<Instant>; N],
    sleep: Pin<Box<Sleep>>,
    armed_ordinal: usize,
}

impl<const N: usize> Default for MuxTimer<N> {
    fn default() -> Self {
        Self {
            deadlines: [None; N],
            sleep: Box::pin(tokio::time::sleep(Duration::ZERO)),
            armed_ordinal: N,
        }
    }
}

impl<const N: usize> MuxTimer<N> {
    /// Fire timer for event with `ordinal` after `timeout` duration.
    /// Returns `true` if the timer was armed, `false` if it was already armed for the same event with sooner deadline.
    pub fn fire_after(&mut self, ordinal: impl Into<usize>, timeout: Duration) -> bool {
        self.fire_at(ordinal, Instant::now() + timeout)
    }

    /// Fire timer for event with `ordinal` at `deadline`.
    /// Returns `true` if the timer was armed, `false` if it was already armed for the same event with sooner deadline.
    pub fn fire_at(&mut self, ordinal: impl Into<usize>, deadline: Instant) -> bool {
        let ordinal = ordinal.into();
        if let Some(existing_deadline) = &mut self.deadlines[ordinal] {
            if *existing_deadline < deadline {
                return false;
            }
            *existing_deadline = deadline;
        } else {
            self.deadlines[ordinal] = Some(deadline);
        }
        if self.deadline().map_or(true, |d| deadline < d) {
            self.arm(ordinal, deadline);
        }
        true
    }

    fn arm(&mut self, ordinal: usize, deadline: Instant) {
        self.sleep.as_mut().reset(deadline);
        self.armed_ordinal = ordinal;
    }

    /// Returns whether the timer is armed.
    pub fn is_armed(&self) -> bool {
        self.armed_ordinal < N
    }

    /// Returns the next deadline, if armed.
    pub fn deadline(&self) -> Option<Instant> {
        (self.armed_ordinal < N).then(|| self.sleep.deadline())
    }

    /// Returns all current deadlines, which can be indexed by event ordinals.
    pub fn deadlines(&self) -> &[Option<Instant>; N] {
        &self.deadlines
    }

    fn soonest_event(&self) -> Option<(usize, Instant)> {
        self.deadlines
            .iter()
            .enumerate()
            .filter_map(|(ordinal, slot)| slot.map(|deadline| (ordinal, deadline)))
            .min_by(|(_, x), (_, y)| x.cmp(y))
    }
}

/// Wait for the next event and return its ordinal.
/// Panics if the timer is not armed.
impl<const N: usize> Future for MuxTimer<N> {
    type Output = usize;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        assert!(self.armed_ordinal < N);
        ready!(self.sleep.as_mut().poll(cx));
        let fired_ordinal = std::mem::replace(&mut self.armed_ordinal, N);
        let fired_deadline = self.deadlines[fired_ordinal].take().expect("armed");
        assert_eq!(fired_deadline, self.sleep.deadline());
        if let Some((ordinal, deadline)) = self.soonest_event() {
            self.arm(ordinal, deadline);
        }
        Poll::Ready(fired_ordinal)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::pin;

    use super::MuxTimer;

    const EVENT_A: usize = 0;
    const EVENT_B: usize = 1;
    const EVENT_C: usize = 2;

    #[tokio::main(flavor = "current_thread", start_paused = true)]
    #[test]
    async fn firing_order() {
        let mut timer: MuxTimer<3> = MuxTimer::default();
        assert_eq!(timer.deadline(), None);

        assert!(timer.fire_after(EVENT_C, Duration::from_millis(100)));
        assert!(timer.fire_after(EVENT_B, Duration::from_millis(50)));
        assert!(timer.fire_after(EVENT_A, Duration::from_millis(150)));

        pin!(timer);

        let event = timer.as_mut().await;
        assert_eq!(event, EVENT_B);

        let event = timer.as_mut().await;
        assert_eq!(event, EVENT_C);

        let event = timer.as_mut().await;
        assert_eq!(event, EVENT_A);

        assert_eq!(timer.deadline(), None);
    }

    #[tokio::main(flavor = "current_thread", start_paused = true)]
    #[test]
    async fn rearming() {
        let mut timer: MuxTimer<3> = MuxTimer::default();

        assert!(timer.fire_after(EVENT_A, Duration::from_millis(100)));
        assert!(!timer.fire_after(EVENT_A, Duration::from_millis(200)));
        assert!(timer.fire_after(EVENT_A, Duration::from_millis(50)));

        pin!(timer);

        let event = timer.as_mut().await;
        assert_eq!(event, EVENT_A);
        assert_eq!(timer.deadline(), None);
    }
}
