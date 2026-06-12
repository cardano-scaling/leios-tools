//! Network-layer partition runtime (T27 §S2).
//!
//! A [`PartitionRuntime`] holds a compiled, time-ordered partition
//! schedule and the set of currently-cut directed links.  Both engines
//! embed one per shard: the actor engine inside `NetworkCoordinator`, the
//! sequential engine inside `SequentialSimulation`.  The runtime is pure
//! state + a virtual-clock cursor — it performs no I/O and is advanced by
//! the engine at deterministic points in its event loop.
//!
//! Determinism: the schedule is a `Vec` sorted by timestamp with a stable
//! sort (ties keep scenario order), advanced by a monotonic cursor; the
//! cut state is a `BTreeMap`.  Two runs with the same compiled schedule
//! and the same virtual-clock trajectory flip identical links at identical
//! instants without any cross-shard coordination.

use std::collections::{BTreeMap, btree_map::Entry};

use crate::{
    clock::Timestamp,
    config::{Link, NodeId, PartitionOp, PartitionScheduleEntry},
    events::EventTracker,
};

pub(crate) struct PartitionRuntime {
    /// Compiled schedule, sorted ascending by timestamp (stable).
    schedule: Vec<PartitionScheduleEntry>,
    /// Index of the next not-yet-applied entry.  Advances monotonically
    /// as virtual time moves forward.
    cursor: usize,
    /// Currently-cut directed links, refcounted: the value is the number
    /// of active scenarios cutting the link, so overlapping windows
    /// compose — a heal only reopens a link once no scenario still cuts
    /// it.  A link absent here is active.
    cut: BTreeMap<Link, usize>,
    /// Whether this shard emits partition telemetry.  Exactly one shard
    /// (shard 0) is the emitter, so each window edge fires one event.
    is_emitter: bool,
}

impl PartitionRuntime {
    /// Build a runtime from a (clone of the) compiled schedule.  Returns
    /// `None` when the schedule is empty, so callers can skip all gating
    /// overhead and stay bit-identical to a no-partition run.
    pub(crate) fn new(mut schedule: Vec<PartitionScheduleEntry>, is_emitter: bool) -> Option<Self> {
        if schedule.is_empty() {
            return None;
        }
        // Stable sort: entries sharing a timestamp keep their build order
        // (scenario order; an Activate can never collide with its own Heal
        // since build_schedule rejects stop <= start).
        schedule.sort_by_key(|e| e.timestamp);
        Some(Self {
            schedule,
            cursor: 0,
            cut: BTreeMap::new(),
            is_emitter,
        })
    }

    /// Is the directed edge `from → to` currently allowed to carry a send?
    /// Hot path: the empty-cut fast path keeps this near-free outside an
    /// active partition window.
    pub(crate) fn is_link_active(&self, from: NodeId, to: NodeId) -> bool {
        self.cut.is_empty() || !self.cut.contains_key(&Link { from, to })
    }

    /// The next scheduled partition instant, if any — used by the actor
    /// engine to wake its coordinator even when no message traffic is due.
    pub(crate) fn next_event_time(&self) -> Option<Timestamp> {
        self.schedule.get(self.cursor).map(|e| e.timestamp)
    }

    /// Apply every schedule entry with `timestamp <= now`, mutating the
    /// cut set and (on the emitter shard) emitting telemetry.  Idempotent
    /// across repeated calls at the same `now`: the cursor only ever moves
    /// forward, so each entry fires exactly once.
    pub(crate) fn advance_to(&mut self, now: Timestamp, tracker: &EventTracker) {
        while let Some(entry) = self.schedule.get(self.cursor) {
            if entry.timestamp > now {
                break;
            }
            match entry.op {
                PartitionOp::Activate => {
                    for link in &entry.links {
                        *self.cut.entry(*link).or_insert(0) += 1;
                    }
                    if self.is_emitter {
                        tracker.track_partition_started(&entry.scenario_name, &entry.links);
                    }
                }
                PartitionOp::Heal => {
                    for link in &entry.links {
                        // Decrement the refcount; the link only reopens
                        // once no overlapping scenario still cuts it.
                        match self.cut.entry(*link) {
                            Entry::Occupied(cut_count) if *cut_count.get() == 1 => {
                                cut_count.remove();
                            }
                            Entry::Occupied(mut cut_count) => *cut_count.get_mut() -= 1,
                            // Every heal mirrors an earlier activate of the
                            // same links, so the link must be present.
                            Entry::Vacant(_) => unreachable!("healed a link that was never cut"),
                        }
                    }
                    if self.is_emitter {
                        tracker.track_partition_healed(&entry.scenario_name, entry.links.len());
                    }
                }
            }
            self.cursor += 1;
        }
    }
}
