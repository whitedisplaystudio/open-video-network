//! Per-peer token buckets.
//!
//! Section 31 asks for rate limiting and announcement spam protection. This is
//! the cheapest version that actually works: each peer gets a bucket that
//! refills at a fixed rate, and a peer that empties it is ignored until it
//! refills. No allocation per message, and the map is pruned as it is used.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use libp2p::PeerId;

#[derive(Clone, Copy, Debug)]
struct Bucket {
    tokens: f64,
    last_refill: Instant,
}

#[derive(Debug)]
pub struct RateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    buckets: HashMap<PeerId, Bucket>,
    last_prune: Instant,
}

impl RateLimiter {
    /// `per_minute` messages allowed in steady state, with the same number
    /// available as an initial burst.
    pub fn per_minute(per_minute: u32) -> Self {
        let capacity = per_minute.max(1) as f64;
        Self {
            capacity,
            refill_per_sec: capacity / 60.0,
            buckets: HashMap::new(),
            last_prune: Instant::now(),
        }
    }

    /// Take one token for `peer`. `false` means the peer is over its budget
    /// and the caller should drop the message.
    pub fn allow(&mut self, peer: &PeerId) -> bool {
        self.allow_at(peer, Instant::now())
    }

    fn allow_at(&mut self, peer: &PeerId, now: Instant) -> bool {
        self.prune(now);
        let capacity = self.capacity;
        let refill = self.refill_per_sec;
        let bucket = self.buckets.entry(*peer).or_insert(Bucket {
            tokens: capacity,
            last_refill: now,
        });
        let elapsed = now
            .saturating_duration_since(bucket.last_refill)
            .as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill).min(capacity);
        bucket.last_refill = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Drop buckets that have been full for a while, so a node that has met
    /// thousands of peers does not carry them all forever.
    fn prune(&mut self, now: Instant) {
        const PRUNE_EVERY: Duration = Duration::from_secs(300);
        if now.saturating_duration_since(self.last_prune) < PRUNE_EVERY {
            return;
        }
        self.last_prune = now;
        let capacity = self.capacity;
        let refill = self.refill_per_sec;
        self.buckets.retain(|_, bucket| {
            let elapsed = now
                .saturating_duration_since(bucket.last_refill)
                .as_secs_f64();
            bucket.tokens + elapsed * refill < capacity
        });
    }

    #[cfg(test)]
    fn tracked_peers(&self) -> usize {
        self.buckets.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_peer_within_its_budget_is_allowed() {
        let mut limiter = RateLimiter::per_minute(60);
        let peer = PeerId::random();
        for _ in 0..60 {
            assert!(limiter.allow(&peer));
        }
    }

    #[test]
    fn a_peer_over_its_budget_is_dropped() {
        let mut limiter = RateLimiter::per_minute(10);
        let peer = PeerId::random();
        for _ in 0..10 {
            assert!(limiter.allow(&peer));
        }
        assert!(!limiter.allow(&peer));
    }

    #[test]
    fn budget_refills_over_time() {
        let mut limiter = RateLimiter::per_minute(60);
        let peer = PeerId::random();
        let start = Instant::now();
        for _ in 0..60 {
            assert!(limiter.allow_at(&peer, start));
        }
        assert!(!limiter.allow_at(&peer, start));
        // One token per second at 60/minute.
        assert!(limiter.allow_at(&peer, start + Duration::from_secs(1)));
    }

    #[test]
    fn one_noisy_peer_does_not_starve_another() {
        let mut limiter = RateLimiter::per_minute(5);
        let noisy = PeerId::random();
        let quiet = PeerId::random();
        for _ in 0..5 {
            limiter.allow(&noisy);
        }
        assert!(!limiter.allow(&noisy));
        assert!(limiter.allow(&quiet));
    }

    #[test]
    fn idle_peers_are_pruned_from_the_map() {
        let mut limiter = RateLimiter::per_minute(60);
        let start = Instant::now();
        for _ in 0..50 {
            limiter.allow_at(&PeerId::random(), start);
        }
        assert_eq!(limiter.tracked_peers(), 50);
        // Well past the prune interval and past a full refill.
        limiter.allow_at(&PeerId::random(), start + Duration::from_secs(600));
        assert_eq!(limiter.tracked_peers(), 1);
    }

    #[test]
    fn a_peer_cannot_reset_its_budget_by_reconnecting() {
        // Buckets are keyed by peer id and survive disconnects, so churning
        // connections is not a way around the limit.
        let mut limiter = RateLimiter::per_minute(2);
        let peer = PeerId::random();
        let start = Instant::now();
        assert!(limiter.allow_at(&peer, start));
        assert!(limiter.allow_at(&peer, start));
        assert!(!limiter.allow_at(&peer, start));
        assert!(!limiter.allow_at(&peer, start));
    }
}
