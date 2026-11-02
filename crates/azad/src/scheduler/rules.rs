//! The derivation rules (decision 0005), and the queries that explain each derived fact.
//!
//! ```prolog
//! needed(A)   :- requested(A).
//! needed(D)   :- needed(A), depends_on(A, D).
//! bad(A)      :- failed(A).
//! bad(A)      :- depends_on(A, D), bad(D).
//! blocked(A)  :- depends_on(A, D), not succeeded(D).
//! skipped(A)  :- needed(A), bad(A), not failed(A).
//! must_run(A) :- needed(A), not blocked(A), not bad(A), not succeeded(A), not started(A).
//! ```

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use datafrog::{Iteration, Relation};

pub type Id = u32;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub requested: BTreeSet<Id>,
    /// `(action, dependency)`: the action reads an output of the dependency.
    pub depends_on: BTreeSet<(Id, Id)>,
    pub succeeded: BTreeSet<Id>,
    pub failed: BTreeSet<Id>,
    pub started: BTreeSet<Id>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Derived {
    pub needed: BTreeSet<Id>,
    pub blocked: BTreeSet<Id>,
    pub skipped: BTreeSet<Id>,
    pub must_run: BTreeSet<Id>,
}

pub fn derive(facts: &Facts) -> Derived {
    let depends_on: Relation<(Id, Id)> = facts.depends_on.iter().copied().collect();
    let depended_by: Relation<(Id, Id)> = facts.depends_on.iter().map(|&(a, d)| (d, a)).collect();
    let succeeded: Relation<Id> = facts.succeeded.iter().copied().collect();
    let failed: Relation<Id> = facts.failed.iter().copied().collect();
    let started: Relation<Id> = facts.started.iter().copied().collect();

    let mut iteration = Iteration::new();
    let needed = iteration.variable::<(Id, ())>("needed");
    let bad = iteration.variable::<(Id, ())>("bad");
    needed.extend(facts.requested.iter().map(|&a| (a, ())));
    bad.extend(facts.failed.iter().map(|&a| (a, ())));
    while iteration.changed() {
        needed.from_join(&needed, &depends_on, |_, &(), &d| (d, ()));
        bad.from_join(&bad, &depended_by, |_, &(), &a| (a, ()));
    }
    let needed = needed.complete();
    let bad: Relation<Id> = Relation::from_map(&bad.complete(), |&(a, ())| a);

    let blocked: Relation<Id> = antijoin(&depended_by, &succeeded, |_, &a| a);
    let needed_and_bad = Relation::from_join(&needed, &Relation::from_map(&bad, |&a| (a, ())), |&a, _, _| (a, ()));
    let skipped = antijoin(&needed_and_bad, &failed, |&a, _| a);
    let mut must_run = Relation::from_vec(needed.elements.clone());
    for excluded in [&blocked, &bad, &succeeded, &started] {
        must_run = antijoin(&must_run, excluded, |&a, _| (a, ()));
    }

    Derived {
        needed: needed.elements.iter().map(|&(a, ())| a).collect(),
        blocked: blocked.elements.into_iter().collect(),
        skipped: skipped.elements.into_iter().collect(),
        must_run: must_run.elements.iter().map(|&(a, ())| a).collect(),
    }
}

/// `input` without the tuples whose key is in `exclude`.
///
/// `Relation::from_antijoin` in datafrog 2.0.1 reads only the "recent" tuples of its input, and a `Relation` has none,
/// so it always returns nothing. Inside an iteration, the tuples of a variable are recent in the first round.
fn antijoin<K, V, T>(input: &Relation<(K, V)>, exclude: &Relation<K>, logic: impl Fn(&K, &V) -> T) -> Relation<T>
where
    K: Ord + Clone + 'static,
    V: Ord + Clone + 'static,
    T: Ord + 'static,
{
    let mut iteration = Iteration::new();
    let source = iteration.variable::<(K, V)>("source");
    let result = iteration.variable::<T>("result");
    source.insert(Relation::from_vec(input.elements.clone()));
    while iteration.changed() {
        result.from_antijoin(&source, exclude, &logic);
    }
    result.complete()
}

/// Why an action is needed: a path from a requested action to it, along `depends_on`.
pub fn needed_chain(facts: &Facts, action: Id) -> Option<Vec<Id>> {
    let starts = facts.requested.iter().copied();
    shortest_path(starts, action, |a| dependencies(facts, a))
}

/// Why an action is skipped: a path from it to a failed action, along `depends_on`.
pub fn failure_chain(facts: &Facts, action: Id) -> Option<Vec<Id>> {
    facts
        .failed
        .iter()
        .filter_map(|&failed| shortest_path([action], failed, |a| dependencies(facts, a)))
        .min_by_key(Vec::len)
}

/// The dependencies of an action that have not succeeded.
pub fn waiting_on(facts: &Facts, action: Id) -> Vec<Id> {
    dependencies(facts, action).into_iter().filter(|d| !facts.succeeded.contains(d)).collect()
}

fn dependencies(facts: &Facts, action: Id) -> Vec<Id> {
    facts.depends_on.range((action, Id::MIN)..=(action, Id::MAX)).map(|&(_, d)| d).collect()
}

fn shortest_path(starts: impl IntoIterator<Item = Id>, target: Id, next: impl Fn(Id) -> Vec<Id>) -> Option<Vec<Id>> {
    let mut previous: BTreeMap<Id, Option<Id>> = BTreeMap::new();
    let mut queue = VecDeque::new();
    for start in starts {
        if previous.insert(start, None).is_none() {
            queue.push_back(start);
        }
    }
    while let Some(current) = queue.pop_front() {
        if current == target {
            let mut path = vec![current];
            let mut at = current;
            while let Some(Some(before)) = previous.get(&at) {
                path.push(*before);
                at = *before;
            }
            path.reverse();
            return Some(path);
        }
        for n in next(current) {
            if let std::collections::btree_map::Entry::Vacant(slot) = previous.entry(n) {
                slot.insert(Some(current));
                queue.push_back(n);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The rules again, as plain loops, to check the datafrog version against.
    fn reference(facts: &Facts) -> Derived {
        let deps = |a: Id| facts.depends_on.iter().filter(move |&&(x, _)| x == a).map(|&(_, d)| d);
        let mut needed = facts.requested.clone();
        let mut bad = facts.failed.clone();
        loop {
            let before = (needed.len(), bad.len());
            for a in needed.clone() {
                needed.extend(deps(a));
            }
            for &(a, d) in &facts.depends_on {
                if bad.contains(&d) {
                    bad.insert(a);
                }
            }
            if before == (needed.len(), bad.len()) {
                break;
            }
        }
        let blocked: BTreeSet<Id> =
            facts.depends_on.iter().filter(|(_, d)| !facts.succeeded.contains(d)).map(|&(a, _)| a).collect();
        let skipped = needed.iter().filter(|a| bad.contains(a) && !facts.failed.contains(a)).copied().collect();
        let must_run = needed
            .iter()
            .filter(|a| {
                !blocked.contains(a) && !bad.contains(a) && !facts.succeeded.contains(a) && !facts.started.contains(a)
            })
            .copied()
            .collect();
        Derived { needed, blocked, skipped, must_run }
    }

    fn arb_facts() -> impl Strategy<Value = Facts> {
        (2u32..12)
            .prop_flat_map(|n| {
                let edge = (0..n, 0..n)
                    .prop_filter_map("an acyclic edge points to a lower id", |(a, d)| (d < a).then_some((a, d)));
                let subset = move || proptest::collection::btree_set(0..n, 0..n as usize);
                (proptest::collection::btree_set(edge, 0..20), subset(), subset(), subset(), subset())
            })
            .prop_map(|(depends_on, requested, succeeded, failed, started)| {
                let failed: BTreeSet<Id> = failed.difference(&succeeded).copied().collect();
                Facts { requested, depends_on, succeeded, failed, started }
            })
    }

    #[test]
    fn a_chain_runs_one_step_at_a_time() {
        let mut facts =
            Facts { requested: BTreeSet::from([2]), depends_on: BTreeSet::from([(2, 1), (1, 0)]), ..Facts::default() };
        assert_eq!(derive(&facts).must_run, BTreeSet::from([0]));
        facts.succeeded.insert(0);
        assert_eq!(derive(&facts).must_run, BTreeSet::from([1]));
        facts.failed.insert(1);
        let derived = derive(&facts);
        assert!(derived.must_run.is_empty());
        assert_eq!(derived.skipped, BTreeSet::from([2]));
    }

    proptest! {
        #[test]
        fn datafrog_agrees_with_the_reference(facts in arb_facts()) {
            prop_assert_eq!(derive(&facts), reference(&facts));
        }

        #[test]
        fn every_derived_fact_can_be_explained(facts in arb_facts()) {
            let derived = derive(&facts);
            for &a in &derived.needed {
                let chain = needed_chain(&facts, a).expect("a needed action has a chain");
                prop_assert!(facts.requested.contains(&chain[0]));
                prop_assert_eq!(chain.last(), Some(&a));
                for pair in chain.windows(2) {
                    prop_assert!(facts.depends_on.contains(&(pair[0], pair[1])));
                }
            }
            for &a in &derived.skipped {
                let chain = failure_chain(&facts, a).expect("a skipped action has a failure chain");
                prop_assert_eq!(chain[0], a);
                prop_assert!(facts.failed.contains(chain.last().expect("not empty")));
            }
            for &a in &derived.must_run {
                prop_assert!(waiting_on(&facts, a).is_empty());
            }
            for &a in &derived.blocked {
                prop_assert!(!waiting_on(&facts, a).is_empty());
            }
        }
    }
}
