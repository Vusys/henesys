use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub pid: u32,
    pub slots: u32,
    pub priority: bool,
    pub arrival_ns: u128,
    pub since: u64,
    pub label: String,
    pub cwd: String,
}

#[derive(Debug, PartialEq)]
pub enum Decision {
    Admit,
    Wait { position: usize, busy: u32 },
}

pub fn clamp(weight: u32, budget: u32) -> u32 {
    weight.min(budget)
}

pub fn sort_waiters(waiters: &mut [Entry]) {
    waiters.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then(a.arrival_ns.cmp(&b.arrival_ns))
            .then(a.pid.cmp(&b.pid))
    });
}

pub fn busy(holders: &[Entry]) -> u32 {
    holders.iter().map(|h| h.slots).sum()
}

// waiters must already be sorted with sort_waiters and include me.
pub fn decide(budget: u32, holders: &[Entry], waiters: &[Entry], me: &Entry) -> Decision {
    let busy = busy(holders);
    // A missing me is a bug; it must wait, even when the line is empty.
    let Some(i) = waiters
        .iter()
        .position(|w| w.pid == me.pid && w.arrival_ns == me.arrival_ns)
    else {
        return Decision::Wait {
            position: waiters.len() + 1,
            busy,
        };
    };
    // Written as a subtraction so busy + slots cannot overflow u32.
    let fits = busy <= budget && me.slots <= budget - busy;
    if i == 0 && fits {
        Decision::Admit
    } else {
        Decision::Wait {
            position: i + 1,
            busy,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, slots: u32, priority: bool, arrival_ns: u128) -> Entry {
        Entry {
            pid,
            slots,
            priority,
            arrival_ns,
            since: 0,
            label: String::new(),
            cwd: String::new(),
        }
    }

    #[test]
    fn alone_with_no_holders_admits() {
        let me = entry(1, 2, false, 10);
        assert_eq!(
            decide(3, &[], std::slice::from_ref(&me), &me),
            Decision::Admit
        );
    }

    #[test]
    fn full_budget_waits_with_busy() {
        let holder = entry(9, 3, false, 1);
        let me = entry(1, 1, false, 10);
        assert_eq!(
            decide(3, &[holder], std::slice::from_ref(&me), &me),
            Decision::Wait {
                position: 1,
                busy: 3
            }
        );
    }

    #[test]
    fn second_in_line_waits_even_with_free_slots() {
        let head = entry(2, 1, false, 5);
        let me = entry(1, 1, false, 10);
        let waiters = [head, me.clone()];
        assert_eq!(
            decide(3, &[], &waiters, &me),
            Decision::Wait {
                position: 2,
                busy: 0
            }
        );
    }

    #[test]
    fn strict_head_of_line_no_backfill() {
        let holder = entry(9, 1, false, 1);
        let big = entry(2, 3, false, 5);
        let me = entry(1, 1, false, 10);
        let waiters = [big, me.clone()];
        assert_eq!(
            decide(3, &[holder], &waiters, &me),
            Decision::Wait {
                position: 2,
                busy: 1
            }
        );
    }

    #[test]
    fn later_priority_sorts_before_earlier_normal() {
        let normal = entry(1, 1, false, 5);
        let prio = entry(2, 1, true, 10);
        let mut waiters = [normal.clone(), prio.clone()];
        sort_waiters(&mut waiters);
        assert_eq!(waiters, [prio.clone(), normal.clone()]);
        assert_eq!(decide(3, &[], &waiters, &prio), Decision::Admit);
        assert_eq!(
            decide(3, &[], &waiters, &normal),
            Decision::Wait {
                position: 2,
                busy: 0
            }
        );
    }

    #[test]
    fn priority_waiters_keep_arrival_order() {
        let first = entry(1, 1, true, 5);
        let second = entry(2, 1, true, 10);
        let mut waiters = [second.clone(), first.clone()];
        sort_waiters(&mut waiters);
        assert_eq!(waiters, [first, second]);
    }

    #[test]
    fn equal_arrival_falls_back_to_pid() {
        let high = entry(7, 1, false, 5);
        let low = entry(3, 1, false, 5);
        let mut waiters = [high.clone(), low.clone()];
        sort_waiters(&mut waiters);
        assert_eq!(waiters, [low, high]);
    }

    #[test]
    fn clamp_limits_weight_to_budget() {
        assert_eq!(clamp(5, 3), 3);
        assert_eq!(clamp(2, 3), 2);
    }

    #[test]
    fn busy_over_budget_makes_head_wait() {
        let holder = entry(9, 4, false, 1);
        let me = entry(1, 1, false, 10);
        assert_eq!(
            decide(3, &[holder], std::slice::from_ref(&me), &me),
            Decision::Wait {
                position: 1,
                busy: 4
            }
        );
    }

    #[test]
    fn missing_me_waits_past_the_end() {
        let other = entry(2, 1, false, 5);
        let me = entry(1, 1, false, 10);
        assert_eq!(
            decide(3, &[], std::slice::from_ref(&other), &me),
            Decision::Wait {
                position: 2,
                busy: 0
            }
        );
    }

    #[test]
    fn missing_me_with_empty_line_waits() {
        let me = entry(1, 1, false, 10);
        assert_eq!(
            decide(3, &[], &[], &me),
            Decision::Wait {
                position: 1,
                busy: 0
            }
        );
    }
}
