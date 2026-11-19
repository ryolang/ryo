use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyState {
    Pending,
    Ready,
    InProgress,
    Resolved,
    Failed,
}

pub(super) struct DeclarationScheduler {
    pending: Vec<usize>,
    dependents: Vec<Vec<u32>>,
    ready: BTreeSet<u32>,
    states: Vec<BodyState>,
}

impl DeclarationScheduler {
    pub(super) fn new(dependencies: &[Vec<u32>], failed: &[bool]) -> DeclarationScheduler {
        assert_eq!(dependencies.len(), failed.len());
        let body_count = dependencies.len();
        let mut scheduler = DeclarationScheduler {
            pending: vec![0; body_count],
            dependents: vec![Vec::new(); body_count],
            ready: BTreeSet::new(),
            states: failed
                .iter()
                .map(|&failed| {
                    if failed {
                        BodyState::Failed
                    } else {
                        BodyState::Pending
                    }
                })
                .collect(),
        };

        for (caller, dependencies) in dependencies.iter().enumerate() {
            let mut dependencies = dependencies.clone();
            dependencies.sort_unstable();
            dependencies.dedup();
            for dependency in dependencies {
                let dependency = dependency as usize;
                assert!(
                    dependency < body_count,
                    "dependency body index out of range"
                );
                if dependency == caller {
                    continue;
                }
                scheduler.pending[caller] += 1;
                scheduler.dependents[dependency].push(caller as u32);
            }
        }

        let failed: Vec<u32> = scheduler
            .states
            .iter()
            .enumerate()
            .filter(|(_, state)| **state == BodyState::Failed)
            .map(|(body, _)| body as u32)
            .collect();
        for body in failed {
            scheduler.release_dependents(body);
        }
        for body in 0..body_count {
            if scheduler.states[body] == BodyState::Pending && scheduler.pending[body] == 0 {
                scheduler.states[body] = BodyState::Ready;
                scheduler.ready.insert(body as u32);
            }
        }
        scheduler
    }

    pub(super) fn next(&mut self) -> Option<u32> {
        let body = self.ready.pop_first()?;
        self.states[body as usize] = BodyState::InProgress;
        Some(body)
    }

    pub(super) fn complete(&mut self, body: u32) {
        let index = body as usize;
        assert_eq!(self.states[index], BodyState::InProgress);
        self.states[index] = BodyState::Resolved;
        self.release_dependents(body);
    }

    fn release_dependents(&mut self, body: u32) {
        let dependents = std::mem::take(&mut self.dependents[body as usize]);
        for dependent in dependents {
            let index = dependent as usize;
            if self.states[index] == BodyState::Failed {
                continue;
            }
            assert!(self.pending[index] > 0);
            self.pending[index] -= 1;
            if self.pending[index] == 0 && self.states[index] == BodyState::Pending {
                self.states[index] = BodyState::Ready;
                self.ready.insert(dependent);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DeclarationScheduler;

    #[test]
    fn scheduler_defers_inferred_callers_and_reuses_diamond() {
        let dependencies = vec![vec![], vec![0], vec![0], vec![1, 2], vec![3, 3]];
        let failed = vec![false; dependencies.len()];
        let mut scheduler = DeclarationScheduler::new(&dependencies, &failed);

        assert_eq!(scheduler.next(), Some(0));
        assert_eq!(scheduler.next(), None);
        scheduler.complete(0);
        assert_eq!(scheduler.next(), Some(1));
        assert_eq!(scheduler.next(), Some(2));
        scheduler.complete(1);
        assert_eq!(scheduler.next(), None);
        scheduler.complete(2);
        assert_eq!(scheduler.next(), Some(3));
        scheduler.complete(3);
        assert_eq!(scheduler.next(), Some(4));
        assert_eq!(scheduler.next(), None);
        scheduler.complete(4);
        assert_eq!(scheduler.next(), None);
    }

    #[test]
    fn scheduler_releases_callers_of_failed_bodies() {
        let dependencies = vec![vec![], vec![0], vec![0, 3], vec![], vec![1]];
        let failed = vec![true, false, false, false, true];
        let mut scheduler = DeclarationScheduler::new(&dependencies, &failed);

        assert_eq!(scheduler.next(), Some(1));
        scheduler.complete(1);
        assert_eq!(scheduler.next(), Some(3));
        assert_eq!(scheduler.next(), None);
        scheduler.complete(3);
        assert_eq!(scheduler.next(), Some(2));
        scheduler.complete(2);
        assert_eq!(scheduler.next(), None);
    }
}
