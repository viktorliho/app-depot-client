//! Session-only transfer observations. SQLite remains the recovery authority.
use app_depot_core::transfer::{TransferObservation, TransferPhase};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct LiveProgress {
    pub observation: TransferObservation,
    pub at: Instant,
    last_bytes: u64,
    last_sample: Instant,
    rate: Option<f64>,
}

impl LiveProgress {
    pub fn new(observation: TransferObservation, at: Instant) -> Self {
        let last_bytes = observation.network_bytes;
        Self {
            observation,
            at,
            last_bytes,
            last_sample: at,
            rate: None,
        }
    }

    pub fn update(&mut self, observation: TransferObservation, at: Instant) {
        if self.observation.phase != TransferPhase::Downloading
            && observation.phase == TransferPhase::Downloading
        {
            self.last_bytes = observation.network_bytes;
            self.last_sample = at;
        } else if observation.network_bytes < self.last_bytes {
            self.rate = None;
        } else if observation.network_bytes > self.last_bytes {
            let seconds = at.saturating_duration_since(self.last_sample).as_secs_f64();
            if seconds >= 0.05 {
                let measured = (observation.network_bytes - self.last_bytes) as f64 / seconds;
                self.rate = Some(
                    self.rate
                        .map_or(measured, |prior| prior * 0.7 + measured * 0.3),
                );
            }
        }
        if observation.network_bytes != self.last_bytes {
            self.last_bytes = observation.network_bytes;
            self.last_sample = at;
        }
        self.observation = observation;
        self.at = at;
    }

    pub fn speed(&self, now: Instant) -> Option<f64> {
        (self.observation.phase == TransferPhase::Downloading
            && now.saturating_duration_since(self.at) < Duration::from_secs(5))
        .then_some(self.rate)
        .flatten()
    }

    pub fn eta(&self, total: u64, now: Instant) -> Option<Duration> {
        let speed = self.speed(now)?;
        if speed < 1024.0
            || self.observation.network_bytes
                < if self.observation.files_total > 1 {
                    4 * 1024 * 1024
                } else {
                    128 * 1024
                }
            || self.observation.received_bytes >= total
            || total == 0
        {
            return None;
        }
        let seconds = (total - self.observation.received_bytes) as f64 / speed;
        (seconds.is_finite() && seconds <= 24.0 * 3600.0).then(|| Duration::from_secs_f64(seconds))
    }

    pub fn stalled(&self, now: Instant) -> bool {
        matches!(
            self.observation.phase,
            TransferPhase::Downloading | TransferPhase::WaitingForServer
        ) && now.saturating_duration_since(self.at) >= Duration::from_secs(5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(bytes: u64, received: u64, phase: TransferPhase) -> TransferObservation {
        TransferObservation {
            phase,
            network_bytes: bytes,
            received_bytes: received,
            current_file: None,
            files_done: 0,
            files_total: 1,
        }
    }
    #[test]
    fn measured_speed_eta_and_idle() {
        let start = Instant::now();
        let mut live = LiveProgress::new(sample(0, 0, TransferPhase::WaitingForServer), start);
        assert!(live.speed(start).is_none());
        live.update(
            sample(1024 * 1024, 1024 * 1024, TransferPhase::Downloading),
            start + Duration::from_secs(1),
        );
        assert!(live.speed(start + Duration::from_secs(1)).is_none());
        live.update(
            sample(2 * 1024 * 1024, 2 * 1024 * 1024, TransferPhase::Downloading),
            start + Duration::from_secs(2),
        );
        assert_eq!(
            live.speed(start + Duration::from_secs(2)),
            Some(1024.0 * 1024.0)
        );
        assert_eq!(
            live.eta(4 * 1024 * 1024, start + Duration::from_secs(2)),
            Some(Duration::from_secs(2))
        );
        assert!(live.stalled(start + Duration::from_secs(8)));
        assert!(live.speed(start + Duration::from_secs(8)).is_none());
        live.update(
            sample(2 * 1024 * 1024, 2 * 1024 * 1024, TransferPhase::Verifying),
            start + Duration::from_secs(9),
        );
        assert!(
            live.eta(4 * 1024 * 1024, start + Duration::from_secs(9))
                .is_none()
        );
    }
    #[test]
    fn reused_bytes_do_not_create_speed_or_eta() {
        let start = Instant::now();
        let mut live = LiveProgress::new(
            sample(0, 4 * 1024 * 1024, TransferPhase::Downloading),
            start,
        );
        live.update(
            sample(0, 8 * 1024 * 1024, TransferPhase::Downloading),
            start + Duration::from_secs(1),
        );
        assert!(live.speed(start + Duration::from_secs(1)).is_none());
        assert!(
            live.eta(16 * 1024 * 1024, start + Duration::from_secs(1))
                .is_none()
        );
    }
    #[test]
    fn tree_eta_waits_for_representative_transfer_bytes() {
        let start = Instant::now();
        let mut first = sample(0, 0, TransferPhase::Downloading);
        first.files_total = 504;
        let mut live = LiveProgress::new(first, start);
        let mut next = sample(512 * 1024, 512 * 1024, TransferPhase::Downloading);
        next.files_total = 504;
        live.update(next, start + Duration::from_secs(1));
        assert!(
            live.eta(17 * 1024 * 1024, start + Duration::from_secs(1))
                .is_none()
        );
    }
}
